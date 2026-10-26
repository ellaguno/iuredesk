//! Árbol genérico de líneas de contenido (RFC 5545 §3.1, RFC 6350 §3.3) sin pérdida.
//!
//! Sirve igual para iCalendar y vCard: ambos son `BEGIN:X` … `END:X` con líneas
//! `[grupo.]NOMBRE *(;PARAM=valor[,valor]) :VALOR`, plegadas a 75 octetos. El valor se guarda
//! **crudo** (con sus escapes), así una propiedad que no entendemos se reescribe idéntica y un
//! `PUT` de vuelta conserva lo que puso otro cliente (`X-…`, `ALARM` raros, extensiones de Apple…).
//!
//! La lectura es tolerante (líneas sin `:` se ignoran, `END` sueltos se ignoran, componentes sin
//! cerrar se cierran al final) y acotada por [`ParseLimits`].

use crate::{Error, Result};

/// Límites de lectura para entradas hostiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseLimits {
    /// Tamaño máximo de la entrada en bytes.
    pub max_bytes: usize,
    /// Profundidad máxima de componentes anidados.
    pub max_depth: usize,
    /// Número máximo de propiedades en todo el documento.
    pub max_properties: usize,
    /// Número máximo de componentes en todo el documento.
    pub max_components: usize,
    /// Parámetros máximos por propiedad.
    pub max_params: usize,
}

impl Default for ParseLimits {
    fn default() -> Self {
        Self {
            max_bytes: 16 * 1024 * 1024,
            max_depth: 16,
            max_properties: 500_000,
            max_components: 100_000,
            max_params: 64,
        }
    }
}

/// Parámetro de una propiedad. Los valores ya vienen sin comillas.
///
/// Un parámetro sin `=` (vCard 2.1: `TEL;WORK;VOICE:…`) se guarda con `values` vacío y se
/// reescribe igual.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    /// Nombre en mayúsculas.
    pub name: String,
    /// Valores (separados por `,` en el archivo).
    pub values: Vec<String>,
}

impl Param {
    /// Parámetro con un solo valor.
    pub fn new(name: &str, value: &str) -> Self {
        Self { name: name.to_ascii_uppercase(), values: vec![value.to_string()] }
    }

    /// Primer valor, si lo hay.
    pub fn value(&self) -> Option<&str> {
        self.values.first().map(String::as_str)
    }
}

/// Propiedad: grupo opcional (vCard), nombre, parámetros y valor crudo (con escapes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    /// Grupo vCard (`item1` en `item1.EMAIL`), tal cual.
    pub group: Option<String>,
    /// Nombre en mayúsculas.
    pub name: String,
    /// Parámetros en el orden original.
    pub params: Vec<Param>,
    /// Valor crudo, todavía escapado.
    pub value: String,
}

impl Property {
    /// Propiedad con valor crudo (ya escapado o que no necesita escape: fechas, URIs…).
    pub fn new(name: &str, raw_value: &str) -> Self {
        Self { group: None, name: name.to_ascii_uppercase(), params: Vec::new(), value: raw_value.to_string() }
    }

    /// Propiedad de texto: escapa `\`, `;`, `,` y saltos de línea.
    pub fn text(name: &str, text: &str) -> Self {
        Self::new(name, &crate::value::escape_text(text))
    }

    /// Agrega un parámetro (constructor encadenable).
    pub fn with_param(mut self, name: &str, value: &str) -> Self {
        self.set_param(name, value);
        self
    }

    /// Primer valor del parámetro `name` (sin distinguir mayúsculas).
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params.iter().find(|p| p.name.eq_ignore_ascii_case(name)).and_then(Param::value)
    }

    /// Todos los valores del parámetro `name` (p. ej. `TYPE=work,voice` o repetido).
    pub fn param_values(&self, name: &str) -> Vec<&str> {
        self.params
            .iter()
            .filter(|p| p.name.eq_ignore_ascii_case(name))
            .flat_map(|p| p.values.iter().map(String::as_str))
            .collect()
    }

    /// Fija (o reemplaza) un parámetro de un solo valor.
    pub fn set_param(&mut self, name: &str, value: &str) {
        let upper = name.to_ascii_uppercase();
        if let Some(p) = self.params.iter_mut().find(|p| p.name == upper) {
            p.values = vec![value.to_string()];
        } else {
            self.params.push(Param { name: upper, values: vec![value.to_string()] });
        }
    }

    /// Quita un parámetro.
    pub fn remove_param(&mut self, name: &str) {
        self.params.retain(|p| !p.name.eq_ignore_ascii_case(name));
    }

    /// Valor de texto sin escapes.
    pub fn text_value(&self) -> String {
        crate::value::unescape_text(&self.value)
    }

    /// Escribe la línea (plegada, terminada en CRLF).
    pub fn write(&self, out: &mut String) {
        let mut line = String::with_capacity(self.name.len() + self.value.len() + 16);
        if let Some(g) = &self.group {
            line.push_str(g);
            line.push('.');
        }
        line.push_str(&self.name);
        for p in &self.params {
            line.push(';');
            line.push_str(&p.name);
            if p.values.is_empty() {
                continue;
            }
            line.push('=');
            for (i, v) in p.values.iter().enumerate() {
                if i > 0 {
                    line.push(',');
                }
                // DQUOTE no puede ir dentro de un valor de parámetro: se descarta.
                let clean: String = v.chars().filter(|c| *c != '"' && *c != '\r' && *c != '\n').collect();
                if clean.contains([':', ';', ',']) {
                    line.push('"');
                    line.push_str(&clean);
                    line.push('"');
                } else {
                    line.push_str(&clean);
                }
            }
        }
        line.push(':');
        // Un salto de línea crudo en el valor rompería el formato: se escapa.
        if self.value.contains(['\r', '\n']) {
            line.push_str(&self.value.replace("\r\n", "\\n").replace(['\n', '\r'], "\\n"));
        } else {
            line.push_str(&self.value);
        }
        fold_into(&line, out);
    }
}

/// Pliega una línea lógica a 75 octetos sin partir caracteres UTF-8 (RFC 5545 §3.1).
fn fold_into(line: &str, out: &mut String) {
    let mut width = 0usize;
    let mut first = true;
    for ch in line.chars() {
        let len = ch.len_utf8();
        let limit = if first { 75 } else { 74 };
        if width.saturating_add(len) > limit {
            out.push_str("\r\n ");
            width = 0;
            first = false;
        }
        out.push(ch);
        width = width.saturating_add(len);
    }
    out.push_str("\r\n");
}

/// Componente (`VCALENDAR`, `VEVENT`, `VALARM`, `VCARD`…) con sus propiedades y subcomponentes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Component {
    /// Nombre en mayúsculas.
    pub name: String,
    /// Propiedades en el orden original.
    pub properties: Vec<Property>,
    /// Subcomponentes en el orden original.
    pub components: Vec<Component>,
}

impl Component {
    /// Componente vacío.
    pub fn new(name: &str) -> Self {
        Self { name: name.to_ascii_uppercase(), properties: Vec::new(), components: Vec::new() }
    }

    /// Primera propiedad con ese nombre.
    pub fn prop(&self, name: &str) -> Option<&Property> {
        self.properties.iter().find(|p| p.name.eq_ignore_ascii_case(name))
    }

    /// Primera propiedad con ese nombre (mutable).
    pub fn prop_mut(&mut self, name: &str) -> Option<&mut Property> {
        self.properties.iter_mut().find(|p| p.name.eq_ignore_ascii_case(name))
    }

    /// Todas las propiedades con ese nombre.
    pub fn props<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Property> + 'a {
        self.properties.iter().filter(move |p| p.name.eq_ignore_ascii_case(name))
    }

    /// Valor de texto (sin escapes) de la primera propiedad con ese nombre.
    pub fn text(&self, name: &str) -> Option<String> {
        self.prop(name).map(Property::text_value)
    }

    /// Reemplaza la primera propiedad con ese nombre (borrando las demás) o la agrega al final.
    pub fn set(&mut self, prop: Property) {
        if let Some(pos) = self.properties.iter().position(|p| p.name == prop.name) {
            let name = prop.name.clone();
            if let Some(slot) = self.properties.get_mut(pos) {
                *slot = prop;
            }
            let mut index = 0usize;
            self.properties.retain(|p| {
                let keep = index == pos || p.name != name;
                index = index.saturating_add(1);
                keep
            });
        } else {
            self.properties.push(prop);
        }
    }

    /// Agrega una propiedad al final (aunque ya exista otra con el mismo nombre).
    pub fn push(&mut self, prop: Property) {
        self.properties.push(prop);
    }

    /// Quita todas las propiedades con ese nombre.
    pub fn remove(&mut self, name: &str) {
        self.properties.retain(|p| !p.name.eq_ignore_ascii_case(name));
    }

    /// Subcomponentes con ese nombre.
    pub fn children<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Component> + 'a {
        self.components.iter().filter(move |c| c.name.eq_ignore_ascii_case(name))
    }

    /// Escribe el componente completo (líneas plegadas, CRLF).
    pub fn write(&self, out: &mut String) {
        out.push_str("BEGIN:");
        out.push_str(&self.name);
        out.push_str("\r\n");
        for p in &self.properties {
            p.write(out);
        }
        for c in &self.components {
            c.write(out);
        }
        out.push_str("END:");
        out.push_str(&self.name);
        out.push_str("\r\n");
    }

    /// El componente como texto.
    pub fn to_ical_string(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }
}

/// Lee todos los componentes de primer nivel con los límites por omisión.
pub fn parse(input: &str) -> Result<Vec<Component>> {
    parse_with(input, ParseLimits::default())
}

/// Lee bytes (UTF-8 con pérdida: los bytes inválidos se reemplazan) con los límites por omisión.
pub fn parse_bytes(input: &[u8]) -> Result<Vec<Component>> {
    let limits = ParseLimits::default();
    if input.len() > limits.max_bytes {
        return Err(Error::Limit(format!("entrada de {} bytes", input.len())));
    }
    parse_with(&String::from_utf8_lossy(input), limits)
}

/// Lee todos los componentes de primer nivel. Las propiedades fuera de un componente se ignoran.
pub fn parse_with(input: &str, limits: ParseLimits) -> Result<Vec<Component>> {
    if input.len() > limits.max_bytes {
        return Err(Error::Limit(format!("entrada de {} bytes", input.len())));
    }
    let input = input.strip_prefix('\u{feff}').unwrap_or(input);
    let mut roots: Vec<Component> = Vec::new();
    let mut stack: Vec<Component> = Vec::new();
    let (mut n_props, mut n_comps) = (0usize, 0usize);

    for line in unfold(input) {
        let Some(prop) = parse_line(&line, limits.max_params)? else { continue };
        if prop.name == "BEGIN" {
            if stack.len() >= limits.max_depth {
                return Err(Error::Limit(format!("más de {} componentes anidados", limits.max_depth)));
            }
            n_comps = n_comps.saturating_add(1);
            if n_comps > limits.max_components {
                return Err(Error::Limit(format!("más de {} componentes", limits.max_components)));
            }
            stack.push(Component::new(prop.value.trim()));
        } else if prop.name == "END" {
            let name = prop.value.trim();
            // END suelto (sin BEGIN correspondiente): se ignora.
            if !stack.iter().any(|c| c.name.eq_ignore_ascii_case(name)) {
                continue;
            }
            // Cierra hasta el BEGIN correspondiente (los intermedios sin END también).
            while let Some(done) = stack.pop() {
                let matched = done.name.eq_ignore_ascii_case(name);
                attach(&mut stack, &mut roots, done);
                if matched {
                    break;
                }
            }
        } else if let Some(top) = stack.last_mut() {
            n_props = n_props.saturating_add(1);
            if n_props > limits.max_properties {
                return Err(Error::Limit(format!("más de {} propiedades", limits.max_properties)));
            }
            top.properties.push(prop);
        }
    }
    // Componentes sin END al final del archivo: se cierran.
    while let Some(done) = stack.pop() {
        attach(&mut stack, &mut roots, done);
    }
    Ok(roots)
}

fn attach(stack: &mut [Component], roots: &mut Vec<Component>, done: Component) {
    match stack.last_mut() {
        Some(parent) => parent.components.push(done),
        None => roots.push(done),
    }
}

/// Despliega las líneas: un CRLF (o LF) seguido de espacio o tabulador es continuación.
fn unfold(input: &str) -> impl Iterator<Item = String> + '_ {
    let mut lines = input.split('\n').peekable();
    std::iter::from_fn(move || {
        loop {
            let first = lines.next()?;
            let mut line = first.strip_suffix('\r').unwrap_or(first).to_string();
            while let Some(next) = lines.peek() {
                if let Some(rest) = next.strip_prefix([' ', '\t']) {
                    line.push_str(rest.strip_suffix('\r').unwrap_or(rest));
                    lines.next();
                } else {
                    break;
                }
            }
            if !line.trim().is_empty() {
                return Some(line);
            }
        }
    })
}

/// Lee una línea lógica. `Ok(None)` si no tiene forma de propiedad (se ignora).
fn parse_line(line: &str, max_params: usize) -> Result<Option<Property>> {
    // Nombre: hasta el primer ';' o ':'.
    let Some(name_end) = line.find([';', ':']) else { return Ok(None) };
    let (raw_name, mut rest) = line.split_at(name_end);
    let raw_name = raw_name.trim();
    if raw_name.is_empty() || !raw_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.') {
        return Ok(None);
    }
    let (group, name) = match raw_name.rsplit_once('.') {
        Some((g, n)) if !g.is_empty() && !n.is_empty() => (Some(g.to_string()), n.to_ascii_uppercase()),
        _ => (None, raw_name.to_ascii_uppercase()),
    };
    let mut params = Vec::new();
    // `rest` empieza con ';' (parámetros) o ':' (valor).
    loop {
        let mut chars = rest.char_indices();
        match chars.next() {
            Some((_, ':')) => {
                let value = rest.get(1..).unwrap_or_default().to_string();
                return Ok(Some(Property { group, name, params, value }));
            }
            Some((_, ';')) => {
                if params.len() >= max_params {
                    return Err(Error::Limit(format!("más de {max_params} parámetros en {name}")));
                }
                let (param, after) = parse_param(rest.get(1..).unwrap_or_default());
                let Some(param) = param else { return Ok(None) };
                params.push(param);
                rest = after;
            }
            _ => return Ok(None),
        }
    }
}

/// Lee `NOMBRE[=valor[,valor]]` y devuelve el resto (que empieza con ';' o ':').
fn parse_param(s: &str) -> (Option<Param>, &str) {
    let Some(stop) = s.find(['=', ';', ':']) else { return (None, "") };
    let (pname, rest) = s.split_at(stop);
    let pname = pname.trim().to_ascii_uppercase();
    if pname.is_empty() {
        return (None, "");
    }
    let Some(after_eq) = rest.strip_prefix('=') else {
        return (Some(Param { name: pname, values: Vec::new() }), rest);
    };
    let mut values = Vec::new();
    let mut cur = after_eq;
    loop {
        let (value, after) = if let Some(q) = cur.strip_prefix('"') {
            match q.find('"') {
                Some(end) => (q.get(..end).unwrap_or_default(), q.get(end.saturating_add(1)..).unwrap_or_default()),
                // Comilla sin cerrar: se toma hasta el ':' como valor.
                None => match q.find(':') {
                    Some(end) => (q.get(..end).unwrap_or_default(), q.get(end..).unwrap_or_default()),
                    None => return (None, ""),
                },
            }
        } else {
            let end = cur.find([',', ';', ':']).unwrap_or(cur.len());
            (cur.get(..end).unwrap_or_default(), cur.get(end..).unwrap_or_default())
        };
        values.push(value.to_string());
        match after.strip_prefix(',') {
            Some(next) => cur = next,
            None => return (Some(Param { name: pname, values }), after),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_preserves_unknown_and_order() {
        let src = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//X//Y//EN\r\nX-WR-CALNAME:Trabajo\r\n\
BEGIN:VEVENT\r\nUID:1@x\r\nX-APPLE-STRUCTURED-LOCATION;VALUE=URI;X-TITLE=\"Casa; centro\":geo:1,2\r\n\
SUMMARY:Hola\\, mundo\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let comps = parse(src).unwrap();
        assert_eq!(comps.len(), 1);
        let out = comps[0].to_ical_string();
        assert_eq!(out, src);
    }

    #[test]
    fn unfolds_and_folds() {
        let long = "x".repeat(200);
        let src = format!("BEGIN:VEVENT\r\nDESCRIPTION:{}\r\n {}\r\nEND:VEVENT\r\n", &long[..100], &long[100..]);
        let c = parse(&src).unwrap();
        assert_eq!(c[0].prop("DESCRIPTION").unwrap().value, long);
        let out = c[0].to_ical_string();
        assert!(out.lines().all(|l| l.len() <= 75));
        assert_eq!(parse(&out).unwrap()[0], c[0]);
    }

    #[test]
    fn fold_respects_utf8() {
        let mut p = Property::text("SUMMARY", &"ñ".repeat(100));
        p.group = None;
        let mut out = String::new();
        p.write(&mut out);
        for l in out.split("\r\n") {
            assert!(l.len() <= 75);
        }
        let c = parse(&format!("BEGIN:X\r\n{out}END:X\r\n")).unwrap();
        assert_eq!(c[0].text("SUMMARY").unwrap(), "ñ".repeat(100));
    }

    #[test]
    fn vcard_groups_and_valueless_params() {
        let src = "BEGIN:VCARD\nVERSION:2.1\nitem1.EMAIL;type=INTERNET:a@b.c\nTEL;WORK;VOICE:123\nEND:VCARD\n";
        let c = parse(src).unwrap();
        let email = c[0].prop("EMAIL").unwrap();
        assert_eq!(email.group.as_deref(), Some("item1"));
        let tel = c[0].prop("TEL").unwrap();
        assert_eq!(tel.params.len(), 2);
        assert!(tel.params[0].values.is_empty());
        assert!(c[0].to_ical_string().contains("TEL;WORK;VOICE:123"));
    }

    #[test]
    fn lenient_with_garbage() {
        let src = "garbage\nEND:VEVENT\nBEGIN:VCALENDAR\nno colon\nBEGIN:VEVENT\nUID:1\n";
        let c = parse(src).unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].components.len(), 1);
    }

    #[test]
    fn depth_limit() {
        let src = "BEGIN:A\n".repeat(100);
        assert!(matches!(parse(&src), Err(Error::Limit(_))));
    }

    #[test]
    fn set_replaces_all() {
        let mut c = Component::new("VEVENT");
        c.push(Property::new("ATTENDEE", "a"));
        c.push(Property::new("SUMMARY", "1"));
        c.push(Property::new("SUMMARY", "2"));
        c.set(Property::new("SUMMARY", "3"));
        assert_eq!(c.props("SUMMARY").count(), 1);
        assert_eq!(c.properties[1].value, "3");
    }

    proptest::proptest! {
        #[test]
        fn never_panics(s in "\\PC{0,400}") {
            let _ = parse(&s);
        }

        #[test]
        fn text_round_trip(t in "\\PC{0,300}") {
            let mut c = Component::new("VEVENT");
            c.push(Property::text("SUMMARY", &t));
            let back = parse(&c.to_ical_string()).unwrap();
            proptest::prop_assert_eq!(back[0].text("SUMMARY").unwrap(), t.replace("\r\n", "\n").replace('\r', "\n"));
        }
    }
}
