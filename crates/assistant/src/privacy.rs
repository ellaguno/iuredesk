//! Privacidad: qué sale del equipo ([`DataScope`], para que la UI lo muestre antes de enviar) y
//! enmascarado de correos y teléfonos ([`Redactor`]) para proveedores externos. Lo enmascarado se
//! restaura localmente en la respuesta: el proveedor sólo ve `[email-1]`, `[tel-1]`…

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Qué incluir al preparar una tarea.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeOptions {
    /// Cuerpo del correo (si no, sólo asunto y cabeceras).
    pub include_body: bool,
    /// Destinatarios (Para/CC).
    pub include_recipients: bool,
    /// Nombres de los adjuntos (su contenido nunca se envía).
    pub include_attachment_names: bool,
    /// Enmascarar correos y teléfonos. `None` → sí si el proveedor es externo.
    pub redact: Option<bool>,
    /// Tope de caracteres por cuerpo (se recorta con aviso).
    pub max_body_chars: usize,
}

impl Default for ScopeOptions {
    fn default() -> Self {
        Self {
            include_body: true,
            include_recipients: true,
            include_attachment_names: true,
            redact: None,
            max_body_chars: 20_000,
        }
    }
}

/// Clase de dato enviado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataField {
    Subject,
    Sender,
    Recipients,
    Date,
    Body,
    AttachmentNames,
    Transcript,
    UserText,
    Instructions,
}

impl DataField {
    /// Etiqueta en/es.
    pub fn label(self, spanish: bool) -> &'static str {
        let (en, es) = match self {
            DataField::Subject => ("Subject", "Asunto"),
            DataField::Sender => ("Sender", "Remitente"),
            DataField::Recipients => ("Recipients", "Destinatarios"),
            DataField::Date => ("Date", "Fecha"),
            DataField::Body => ("Message body", "Cuerpo del mensaje"),
            DataField::AttachmentNames => {
                ("Attachment names (not their content)", "Nombres de adjuntos (no su contenido)")
            }
            DataField::Transcript => ("Meeting transcript", "Transcripción de la reunión"),
            DataField::UserText => ("Your text", "Tu texto"),
            DataField::Instructions => ("Assistant instructions", "Instrucciones del asistente"),
        };
        if spanish { es } else { en }
    }
}

/// Exactamente qué se enviará, para mostrarlo antes de confirmar.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DataScope {
    /// Proveedor que lo recibirá.
    pub provider: String,
    /// Sale a un tercero (no la instancia, ni el inquilino M365, ni `localhost`).
    pub external: bool,
    /// Correos y teléfonos enmascarados.
    pub redacted: bool,
    /// Cuántos correos o documentos.
    pub items: usize,
    /// Campos y caracteres de cada uno.
    pub fields: BTreeMap<DataField, usize>,
    /// Cuerpos recortados por el tope.
    pub truncated: bool,
    /// Total de caracteres (instrucciones incluidas).
    pub total_chars: usize,
    /// El texto exacto que se enviará (ya enmascarado si aplica).
    pub payload: String,
}

impl DataScope {
    pub(crate) fn add(&mut self, field: DataField, chars: usize) {
        if chars > 0 {
            let e = self.fields.entry(field).or_insert(0);
            *e = e.saturating_add(chars);
        }
    }

    /// Resumen de una línea para la interfaz.
    pub fn summary(&self, spanish: bool) -> String {
        let fields: Vec<&str> = self.fields.keys().map(|f| f.label(spanish)).collect();
        let dest = if spanish { "a" } else { "to" };
        format!("{} {dest} {} ({} chars)", fields.join(", "), self.provider, self.total_chars)
    }
}

/// Clase de dato enmascarado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Email,
    Phone,
}

/// Enmascara correos y teléfonos de forma estable (mismo valor → misma marca) y los restaura.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    forward: BTreeMap<String, String>,
    back: BTreeMap<String, String>,
    emails: usize,
    phones: usize,
}

fn is_local_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'%' | b'+' | b'-')
}
fn is_domain_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-')
}
fn is_phone_char(b: u8) -> bool {
    b.is_ascii_digit() || matches!(b, b' ' | b'-' | b'.' | b'(' | b')')
}

/// Tramos `[inicio, fin)` de correos.
fn find_emails(s: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < s.len() {
        if s.get(i) != Some(&b'@') {
            i = i.saturating_add(1);
            continue;
        }
        let mut start = i;
        while start > 0 && s.get(start - 1).copied().is_some_and(is_local_char) {
            start -= 1;
        }
        while start < i && s.get(start) == Some(&b'.') {
            start = start.saturating_add(1);
        }
        let mut end = i.saturating_add(1);
        while s.get(end).copied().is_some_and(is_domain_char) {
            end = end.saturating_add(1);
        }
        while end > i.saturating_add(1) && matches!(s.get(end - 1), Some(b'.' | b'-')) {
            end -= 1;
        }
        let domain = s.get(i.saturating_add(1)..end).unwrap_or_default();
        let tld_ok =
            domain.rsplit(|b| *b == b'.').next().is_some_and(|t| t.len() >= 2 && t.iter().all(u8::is_ascii_alphabetic));
        if start < i && domain.contains(&b'.') && tld_ok {
            out.push((start, end));
            i = end;
        } else {
            i = i.saturating_add(1);
        }
    }
    out
}

/// Tramos de teléfonos (9 a 15 dígitos, con espacios, guiones, puntos o paréntesis; `+` inicial).
fn find_phones(s: &[u8], skip: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0usize;
    let inside = |p: usize| skip.iter().any(|(a, b)| p >= *a && p < *b);
    while i < s.len() {
        let c = s.get(i).copied().unwrap_or(0);
        let prev_ok = i == 0 || !s.get(i - 1).copied().is_some_and(|p| p.is_ascii_alphanumeric() || p == b'+');
        if !(c == b'+' || c == b'(' || c.is_ascii_digit()) || !prev_ok || inside(i) {
            i = i.saturating_add(1);
            continue;
        }
        let mut end = i.saturating_add(1);
        while s.get(end).copied().is_some_and(is_phone_char) && !inside(end) {
            end = end.saturating_add(1);
        }
        while end > i && !s.get(end - 1).copied().is_some_and(|b| b.is_ascii_digit()) {
            end -= 1;
        }
        let span = s.get(i..end).unwrap_or_default();
        let digits = span.iter().filter(|b| b.is_ascii_digit()).count();
        // Una hora (`16:00`) o una fracción detrás no son teléfono.
        let next_ok =
            !s.get(end).copied().is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'/' | b','));
        let no_double_space = !span.windows(2).any(|w| w == b"  ");
        if (9..=15).contains(&digits) && next_ok && no_double_space {
            out.push((i, end));
            i = end;
        } else {
            i = end.max(i.saturating_add(1));
        }
    }
    out
}

impl Redactor {
    pub fn new() -> Self {
        Self::default()
    }

    /// ¿Se enmascaró algo?
    pub fn is_empty(&self) -> bool {
        self.forward.is_empty()
    }

    fn token(&mut self, value: &str, kind: Kind) -> String {
        if let Some(t) = self.forward.get(value) {
            return t.clone();
        }
        let t = match kind {
            Kind::Email => {
                self.emails = self.emails.saturating_add(1);
                format!("[email-{}]", self.emails)
            }
            Kind::Phone => {
                self.phones = self.phones.saturating_add(1);
                format!("[tel-{}]", self.phones)
            }
        };
        self.forward.insert(value.to_string(), t.clone());
        self.back.insert(t.clone(), value.to_string());
        t
    }

    /// Enmascara un texto.
    pub fn mask(&mut self, text: &str) -> String {
        let bytes = text.as_bytes();
        let emails = find_emails(bytes);
        let phones = find_phones(bytes, &emails);
        let mut spans: Vec<(usize, usize, Kind)> = emails
            .into_iter()
            .map(|(a, b)| (a, b, Kind::Email))
            .chain(phones.into_iter().map(|(a, b)| (a, b, Kind::Phone)))
            .collect();
        spans.sort_by_key(|s| s.0);
        let mut out = String::with_capacity(text.len());
        let mut pos = 0usize;
        for (a, b, kind) in spans {
            if a < pos {
                continue;
            }
            // Los tramos empiezan y terminan en ASCII: los cortes caen en límites de carácter.
            let (Some(before), Some(value)) = (text.get(pos..a), text.get(a..b)) else { continue };
            out.push_str(before);
            let t = self.token(value, kind);
            out.push_str(&t);
            pos = b;
        }
        out.push_str(text.get(pos..).unwrap_or_default());
        out
    }

    /// Restaura las marcas en un texto.
    pub fn restore(&self, text: &str) -> String {
        if self.back.is_empty() || !text.contains('[') {
            return text.to_string();
        }
        let mut out = text.to_string();
        // De la marca más larga a la más corta: `[tel-12]` antes que `[tel-1]`.
        let mut tokens: Vec<(&String, &String)> = self.back.iter().collect();
        tokens.sort_by_key(|(t, _)| std::cmp::Reverse(t.len()));
        for (t, v) in tokens {
            out = out.replace(t.as_str(), v);
        }
        out
    }

    /// Restaura todas las cadenas de un JSON (antes de validarlo).
    pub fn restore_value(&self, v: &mut Value) {
        match v {
            Value::String(s) => *s = self.restore(s),
            Value::Array(a) => a.iter_mut().for_each(|x| self.restore_value(x)),
            Value::Object(o) => o.values_mut().for_each(|x| self.restore_value(x)),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn masks_emails_and_phones_stably() {
        let mut r = Redactor::new();
        let text = "Escríbele a ana.ruiz@cliente.com.mx o llama al +52 (55) 1234-5678. \
                    Copia a ana.ruiz@cliente.com.mx. Audiencia el 2026-10-27 a las 16:00, monto $12,000,000.";
        let masked = r.mask(text);
        assert_eq!(
            masked,
            "Escríbele a [email-1] o llama al [tel-1]. \
             Copia a [email-1]. Audiencia el 2026-10-27 a las 16:00, monto $12,000,000."
        );
        assert_eq!(r.restore(&masked), text);
        let mut v = serde_json::json!({"a": ["[email-1]", {"b": "tel: [tel-1]"}], "n": 3});
        r.restore_value(&mut v);
        assert_eq!(v["a"][0], "ana.ruiz@cliente.com.mx");
        assert_eq!(v["a"][1]["b"], "tel: +52 (55) 1234-5678");
    }

    #[test]
    fn leaves_non_contacts_alone() {
        let mut r = Redactor::new();
        for t in [
            "@usuario en redes",
            "a@b",
            "x@localhost",
            "pedido 12345678",
            "folio ABC123456789",
            "1.234.567,00",
            "2026-10-27 16:00",
        ] {
            assert_eq!(r.mask(t), t, "{t}");
        }
        assert!(r.is_empty());
        assert_eq!(r.mask("tel 5512345678"), "tel [tel-1]");
    }

    #[test]
    fn many_tokens_restore_without_prefix_clash() {
        let mut r = Redactor::new();
        let text: String = (0..12).map(|i| format!("u{i}@x.com ")).collect();
        let masked = r.mask(&text);
        assert!(masked.contains("[email-12]"));
        assert_eq!(r.restore(&masked), text);
    }

    proptest! {
        #[test]
        fn mask_never_panics_and_round_trips(s in "\\PC{0,200}") {
            let mut r = Redactor::new();
            let m = r.mask(&s);
            if !s.contains("[email-") && !s.contains("[tel-") {
                prop_assert_eq!(r.restore(&m), s);
            }
        }

        #[test]
        fn masked_text_has_no_emails(local in "[a-z]{1,10}", dom in "[a-z]{1,10}", pre in "[ a-zA-Z]{0,20}") {
            let mut r = Redactor::new();
            let s = format!("{pre} {local}@{dom}.com fin");
            let m = r.mask(&s);
            prop_assert!(!m.contains('@'));
        }
    }
}
