//! Reglas: «Cuando llegue un correo de… → Moverlo a…».
//!
//! Una regla es una lista de condiciones (todas o cualquiera) y una lista de acciones. Se guarda
//! con serde en el almacén local ([`crate::book`]) y se traduce a Sieve cuando se puede
//! ([`crate::sieve`]).

use iuredesk_model::{AccountId, FolderId, MessageEnvelope};
use serde::{Deserialize, Serialize};

use crate::Bi;
use crate::error::{Result, RulesError};

/// Longitud máxima del texto de una condición.
pub const MAX_VALUE: usize = 500;
/// Condiciones o acciones por regla.
pub const MAX_ITEMS: usize = 32;
/// Reglas por libro.
pub const MAX_RULES: usize = 500;
/// Días mínimo y máximo entre dos respuestas automáticas al mismo remitente.
pub const AUTOREPLY_DAYS: (u32, u32) = (1, 90);

/// Parte del correo que mira una condición de texto.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    /// Remitente (nombre y dirección).
    From,
    /// Para.
    To,
    /// Copia.
    Cc,
    /// Para o Copia.
    Recipients,
    /// Asunto.
    Subject,
    /// Texto del mensaje.
    Body,
    /// Lista de correo (`List-Id`).
    ListId,
    /// Otra cabecera (nombre sin `:`).
    Header(String),
}

impl Field {
    /// Nombre para cualquier usuario.
    pub fn label(&self) -> Bi {
        match self {
            Field::From => Bi::new("the sender", "el remitente"),
            Field::To => Bi::new("To", "Para"),
            Field::Cc => Bi::new("Cc", "CC"),
            Field::Recipients => Bi::new("To or Cc", "Para o CC"),
            Field::Subject => Bi::new("the subject", "el asunto"),
            Field::Body => Bi::new("the message text", "el texto del mensaje"),
            Field::ListId => Bi::new("the mailing list", "la lista de correo"),
            Field::Header(h) => Bi::new(format!("the header {h}"), format!("la cabecera {h}")),
        }
    }

    /// `true` si son direcciones (admite «dominio»).
    pub fn is_address(&self) -> bool {
        matches!(self, Field::From | Field::To | Field::Cc | Field::Recipients)
    }
}

/// Cómo se compara el texto (sin distinguir mayúsculas).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextOp {
    /// Contiene.
    #[default]
    Contains,
    /// No contiene.
    NotContains,
    /// Es exactamente.
    Is,
    /// Empieza por.
    StartsWith,
    /// Termina en.
    EndsWith,
    /// Expresión regular (acotada: tiempo lineal, tamaño máximo).
    Regex,
}

impl TextOp {
    /// Todas, en el orden del editor.
    pub const ALL: [TextOp; 6] =
        [TextOp::Contains, TextOp::NotContains, TextOp::Is, TextOp::StartsWith, TextOp::EndsWith, TextOp::Regex];

    /// Nombre para cualquier usuario.
    pub fn label(self) -> Bi {
        match self {
            TextOp::Contains => Bi::new("contains", "contiene"),
            TextOp::NotContains => Bi::new("doesn't contain", "no contiene"),
            TextOp::Is => Bi::new("is", "es"),
            TextOp::StartsWith => Bi::new("starts with", "empieza por"),
            TextOp::EndsWith => Bi::new("ends with", "termina en"),
            TextOp::Regex => Bi::new("matches the pattern", "coincide con el patrón"),
        }
    }
}

/// Condición de una regla.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "if", rename_all = "snake_case")]
pub enum Condition {
    /// Texto en una parte del correo.
    Text {
        /// Parte.
        field: Field,
        /// Comparación.
        op: TextOp,
        /// Texto buscado.
        value: String,
    },
    /// La dirección es de un dominio (`example.com` o subdominio).
    Domain {
        /// Remitente o destinatarios.
        field: Field,
        /// Dominio.
        domain: String,
    },
    /// Tiene adjuntos.
    HasAttachment,
    /// Pesa más de.
    SizeOver {
        /// Bytes.
        bytes: u64,
    },
    /// Pesa menos de.
    SizeUnder {
        /// Bytes.
        bytes: u64,
    },
    /// Llegó a esta cuenta.
    Account {
        /// Cuenta.
        account: AccountId,
    },
    /// Está en esta carpeta.
    InFolder {
        /// Carpeta.
        folder: FolderId,
    },
    /// Es una respuesta (`In-Reply-To`/`Re:`).
    IsReply,
    /// Viene de una lista o boletín (`List-Id`/`List-Unsubscribe`).
    IsList,
    /// El remitente está en tus contactos o vinculado en Iurefficient.
    KnownContact,
}

impl Condition {
    /// Frase para cualquier usuario («el remitente contiene ana@»).
    pub fn describe(&self) -> Bi {
        match self {
            Condition::Text { field, op, value } => {
                let f = field.label();
                let o = op.label();
                Bi::new(format!("{} {} “{value}”", f.en, o.en), format!("{} {} «{value}»", f.es, o.es))
            }
            Condition::Domain { field, domain } => {
                let f = field.label();
                Bi::new(format!("{} is from {domain}", f.en), format!("{} es de {domain}", f.es))
            }
            Condition::HasAttachment => Bi::new("it has attachments", "tiene adjuntos"),
            Condition::SizeOver { bytes } => Bi::new(
                format!("it's larger than {}", human_size(*bytes)),
                format!("pesa más de {}", human_size(*bytes)),
            ),
            Condition::SizeUnder { bytes } => Bi::new(
                format!("it's smaller than {}", human_size(*bytes)),
                format!("pesa menos de {}", human_size(*bytes)),
            ),
            Condition::Account { account } => {
                Bi::new(format!("it arrives at account {account}"), format!("llega a la cuenta {account}"))
            }
            Condition::InFolder { folder } => {
                Bi::new(format!("it's in folder {folder}"), format!("está en la carpeta {folder}"))
            }
            Condition::IsReply => Bi::new("it's a reply", "es una respuesta"),
            Condition::IsList => Bi::new("it's a newsletter or list", "es un boletín o lista"),
            Condition::KnownContact => Bi::new("the sender is a contact", "el remitente es un contacto"),
        }
    }
}

/// Carpeta destino: id local más ruta y nombre (para Sieve y si el id cambia).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderRef {
    /// Id local.
    pub id: FolderId,
    /// Cuenta de la carpeta.
    pub account: AccountId,
    /// Ruta remota (`INBOX/Clientes`).
    pub path: String,
    /// Nombre visible.
    pub name: String,
}

/// Acción de una regla.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum Action {
    /// Moverlo a una carpeta.
    MoveTo {
        /// Destino.
        folder: FolderRef,
    },
    /// Ponerle una etiqueta o categoría.
    Label {
        /// Nombre.
        name: String,
    },
    /// Marcarlo para seguimiento.
    Flag,
    /// Marcarlo como leído.
    MarkRead,
    /// Enviarlo a la papelera.
    Delete,
    /// Reenviarlo (mensaje nuevo con el original adjunto).
    Forward {
        /// Destinatario.
        to: String,
    },
    /// Redirigirlo (en el servidor conserva el remitente; en local se reenvía adjunto).
    Redirect {
        /// Destinatario.
        to: String,
    },
    /// Responder automáticamente (una vez por remitente cada `days` días).
    AutoReply {
        /// Asunto.
        subject: String,
        /// Texto.
        body: String,
        /// Días entre respuestas al mismo remitente.
        days: u32,
    },
    /// Avisar con una notificación aunque estén desactivadas.
    Notify,
    /// No avisar.
    Silence,
    /// No aplicar más reglas.
    Stop,
}

impl Action {
    /// Frase para cualquier usuario («Moverlo a Clientes»).
    pub fn describe(&self) -> Bi {
        match self {
            Action::MoveTo { folder } => {
                Bi::new(format!("Move it to {}", folder.name), format!("Moverlo a {}", folder.name))
            }
            Action::Label { name } => Bi::new(format!("Label it “{name}”"), format!("Etiquetarlo «{name}»")),
            Action::Flag => Bi::new("Flag it for follow-up", "Marcarlo para seguimiento"),
            Action::MarkRead => Bi::new("Mark it as read", "Marcarlo como leído"),
            Action::Delete => Bi::new("Move it to Trash", "Enviarlo a la papelera"),
            Action::Forward { to } => Bi::new(format!("Forward it to {to}"), format!("Reenviarlo a {to}")),
            Action::Redirect { to } => Bi::new(format!("Redirect it to {to}"), format!("Redirigirlo a {to}")),
            Action::AutoReply { days, .. } => Bi::new(
                format!("Reply automatically (once every {days} days per sender)"),
                format!("Responder automáticamente (una vez cada {days} días por remitente)"),
            ),
            Action::Notify => Bi::new("Always notify me", "Avisarme siempre"),
            Action::Silence => Bi::new("Don't notify me", "No avisarme"),
            Action::Stop => Bi::new("Don't apply more rules", "No aplicar más reglas"),
        }
    }

    /// Envía correo (necesita protección contra bucles).
    pub fn sends_mail(&self) -> bool {
        matches!(self, Action::Forward { .. } | Action::Redirect { .. } | Action::AutoReply { .. })
    }
}

/// Una regla.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// Id estable (lo asigna el libro).
    #[serde(default)]
    pub id: u64,
    /// Nombre.
    pub name: String,
    /// Activa.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// `true` = se cumplen todas las condiciones; `false` = cualquiera.
    #[serde(default = "yes")]
    pub all: bool,
    /// Condiciones (vacío = todo correo).
    #[serde(default)]
    pub conditions: Vec<Condition>,
    /// Acciones, en orden.
    #[serde(default)]
    pub actions: Vec<Action>,
    /// Solo para esta cuenta (`None` = todas).
    #[serde(default)]
    pub account: Option<AccountId>,
    /// Ejecutarla en el servidor cuando se pueda (Sieve), aunque la app esté cerrada.
    #[serde(default = "yes")]
    pub prefer_server: bool,
}

fn yes() -> bool {
    true
}

impl Default for Rule {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            enabled: true,
            all: true,
            conditions: Vec::new(),
            actions: Vec::new(),
            account: None,
            prefer_server: true,
        }
    }
}

impl Rule {
    /// Comprueba que la regla se pueda guardar; devuelve qué falta, en los dos idiomas.
    pub fn validate(&self) -> std::result::Result<(), Bi> {
        if self.actions.is_empty() || self.actions.iter().all(|a| matches!(a, Action::Stop)) {
            return Err(Bi::new("Choose what to do with the message.", "Elige qué hacer con el mensaje."));
        }
        if self.conditions.len() > MAX_ITEMS || self.actions.len() > MAX_ITEMS {
            return Err(Bi::new("This rule has too many parts.", "Esta regla tiene demasiadas partes."));
        }
        for c in &self.conditions {
            match c {
                Condition::Text { value, op, .. } => {
                    if value.trim().is_empty() {
                        return Err(Bi::new("Write the text to look for.", "Escribe el texto que se busca."));
                    }
                    if value.chars().count() > MAX_VALUE {
                        return Err(Bi::new("The text is too long.", "El texto es demasiado largo."));
                    }
                    if *op == TextOp::Regex
                        && let Err(e) = crate::eval::compile_regex(value)
                    {
                        return Err(Bi::new(
                            format!("The pattern is not valid: {e}"),
                            format!("El patrón no es válido: {e}"),
                        ));
                    }
                }
                Condition::Domain { domain, .. } if domain.trim().is_empty() || domain.contains(' ') => {
                    return Err(Bi::new("Write a domain like example.com.", "Escribe un dominio como example.com."));
                }
                _ => {}
            }
        }
        for a in &self.actions {
            match a {
                Action::Forward { to } | Action::Redirect { to } if !plausible_address(to) => {
                    return Err(Bi::new(
                        "Write the address to forward to.",
                        "Escribe la dirección a la que se reenvía.",
                    ));
                }
                Action::Label { name } if name.trim().is_empty() => {
                    return Err(Bi::new("Write the label name.", "Escribe el nombre de la etiqueta."));
                }
                Action::AutoReply { body, days, .. }
                    if body.trim().is_empty() || !(AUTOREPLY_DAYS.0..=AUTOREPLY_DAYS.1).contains(days) =>
                {
                    return Err(Bi::new(
                        "Write the automatic reply (and 1–90 days between replies).",
                        "Escribe la respuesta automática (y de 1 a 90 días entre respuestas).",
                    ));
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Resumen de una línea: «Cuando llegue un correo de ana@ → Moverlo a Clientes».
    pub fn summary(&self) -> Bi {
        let joiner = if self.all { (" and ", " y ") } else { (" or ", " o ") };
        let conds: Vec<Bi> = self.conditions.iter().map(Condition::describe).collect();
        let when = if conds.is_empty() {
            Bi::new("When any message arrives", "Cuando llegue cualquier correo")
        } else {
            Bi::new(
                format!("When {}", conds.iter().map(|b| b.en.as_str()).collect::<Vec<_>>().join(joiner.0)),
                format!("Cuando {}", conds.iter().map(|b| b.es.as_str()).collect::<Vec<_>>().join(joiner.1)),
            )
        };
        let acts: Vec<Bi> = self.actions.iter().map(Action::describe).collect();
        Bi::new(
            format!("{} → {}", when.en, acts.iter().map(|b| b.en.as_str()).collect::<Vec<_>>().join(", ")),
            format!("{} → {}", when.es, acts.iter().map(|b| b.es.as_str()).collect::<Vec<_>>().join(", ")),
        )
    }

    /// `true` si alguna acción envía correo.
    pub fn sends_mail(&self) -> bool {
        self.actions.iter().any(Action::sends_mail)
    }

    /// Regla propuesta a partir de un correo («Crear regla a partir de este correo»): remitente,
    /// y lista de correo si la tiene; la acción la elige el usuario.
    pub fn from_message(env: &MessageEnvelope, list_id: Option<&str>) -> Rule {
        let mut conditions = Vec::new();
        if let Some(l) = list_id.map(str::trim).filter(|l| !l.is_empty()) {
            let id = l.rsplit_once('<').map_or(l, |(_, r)| r.trim_end_matches('>'));
            conditions.push(Condition::Text { field: Field::ListId, op: TextOp::Contains, value: clip(id) });
        } else if let Some(from) = env.from.first() {
            conditions.push(Condition::Text { field: Field::From, op: TextOp::Contains, value: clip(&from.addr) });
        }
        let who = env.from.first().map(|m| m.display().to_string()).unwrap_or_default();
        Rule { name: clip(&who), account: Some(env.account), conditions, ..Rule::default() }
    }
}

fn clip(s: &str) -> String {
    s.chars().take(MAX_VALUE).collect()
}

/// `a@b.c` mínimo (la validación completa la hace el redactor al enviar).
pub fn plausible_address(s: &str) -> bool {
    let s = s.trim();
    match s.split_once('@') {
        Some((l, d)) => !l.is_empty() && d.contains('.') && !s.contains(char::is_whitespace) && s.len() <= 320,
        None => false,
    }
}

/// «1,5 MB», «300 KB».
pub fn human_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    if bytes >= MB {
        let tenths = bytes.saturating_mul(10) / MB;
        if tenths.is_multiple_of(10) {
            format!("{} MB", tenths / 10)
        } else {
            format!("{}.{} MB", tenths / 10, tenths % 10)
        }
    } else if bytes >= KB {
        format!("{} KB", bytes / KB)
    } else {
        format!("{bytes} B")
    }
}

/// Error si la regla no se puede guardar.
pub fn ensure_valid(rule: &Rule) -> Result<()> {
    rule.validate().map_err(|b| RulesError::Invalid(b.es))
}

#[cfg(test)]
mod tests {
    use super::*;
    use iuredesk_model::Mailbox;

    #[test]
    fn validate_and_summary() {
        let mut r = Rule {
            name: "Clientes".into(),
            conditions: vec![Condition::Text { field: Field::From, op: TextOp::Contains, value: "ana@".into() }],
            ..Rule::default()
        };
        assert!(r.validate().is_err(), "sin acciones");
        r.actions.push(Action::MoveTo {
            folder: FolderRef {
                id: FolderId(3),
                account: AccountId(1),
                path: "Clientes".into(),
                name: "Clientes".into(),
            },
        });
        assert!(r.validate().is_ok());
        assert_eq!(r.summary().es, "Cuando el remitente contiene «ana@» → Moverlo a Clientes");
        r.conditions.push(Condition::Text { field: Field::Subject, op: TextOp::Regex, value: "(".into() });
        assert!(r.validate().is_err(), "regex rota");
        r.conditions.pop();
        r.actions.push(Action::Forward { to: "no es dirección".into() });
        assert!(r.validate().is_err());
    }

    #[test]
    fn from_message_prefers_list() {
        let env = MessageEnvelope {
            from: vec![Mailbox::new("Boletín", "news@tienda.example")],
            account: AccountId(2),
            ..Default::default()
        };
        let r = Rule::from_message(&env, Some("Tienda <ofertas.tienda.example>"));
        assert_eq!(
            r.conditions,
            vec![Condition::Text {
                field: Field::ListId,
                op: TextOp::Contains,
                value: "ofertas.tienda.example".into()
            }]
        );
        let r = Rule::from_message(&env, None);
        assert!(
            matches!(&r.conditions[0], Condition::Text { field: Field::From, value, .. } if value == "news@tienda.example")
        );
        assert_eq!(r.account, Some(AccountId(2)));
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(1536 * 1024), "1.5 MB");
        assert_eq!(human_size(300 * 1024), "300 KB");
        assert_eq!(human_size(10), "10 B");
    }
}
