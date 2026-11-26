//! Las personas como objetos (UX-24): el contacto de una dirección, «sus correos» (búsqueda
//! `de:`) y «seleccionar sus correos» en la lista actual. La tarjeta flotante de la UI y los
//! comandos `mail.from_sender` / `mail.select_sender` (paleta, CLI, MCP) llegan aquí, y la
//! selección por criterio de UX-25 (`mail.select.*`).

use iuredesk_model::{Contact, MessageId};

use super::{Notice, Outcome, Session};
use crate::Result;

/// Longitud máxima de una dirección (RFC 5321: 254 útiles; algo de holgura).
const MAX_ADDR: usize = 320;
/// Filas que se revisan al seleccionar por remitente.
const MAX_SCAN: usize = 20_000;

/// Dirección limpia y razonable, o `None` (dato externo: CLI, MCP, cabeceras).
pub fn clean_address(raw: &str) -> Option<String> {
    let a = raw.trim().trim_start_matches('<').trim_end_matches('>').trim();
    let ok = !a.is_empty()
        && a.len() <= MAX_ADDR
        && a.contains('@')
        && !a.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '"' | '<' | '>' | ',' | ';'));
    ok.then(|| a.to_string())
}

impl Session {
    /// Contacto de la libreta con esta dirección (sin distinguir mayúsculas).
    pub fn contact_by_email(&mut self, addr: &str) -> Option<Contact> {
        let addr = addr.trim();
        if addr.is_empty() {
            return None;
        }
        self.contacts().iter().find(|c| c.emails.iter().any(|e| e.value.trim().eq_ignore_ascii_case(addr))).cloned()
    }

    /// Remitente del mensaje abierto.
    pub fn focused_sender(&mut self) -> Option<String> {
        let id = self.focused?;
        self.envelope(id).and_then(|e| e.from.first()).map(|m| m.addr.clone())
    }

    /// Dirección del argumento del comando o, sin él, el remitente del mensaje abierto.
    fn person_arg(&mut self, arg: Option<&str>) -> Option<String> {
        match arg {
            Some(a) => clean_address(a),
            None => self.focused_sender().and_then(|a| clean_address(&a)),
        }
    }

    /// `mail.from_sender[:dirección]`: busca los correos de esa persona.
    pub(crate) fn run_from_sender(&mut self, arg: Option<&str>) -> Result<Outcome> {
        let Some(addr) = self.person_arg(arg) else {
            return Ok(Outcome::Notice(Notice::info(
                "Open a message or give an email address.",
                "Abre un mensaje o indica una dirección de correo.",
            )));
        };
        self.set_search(&format!("de:{addr}"));
        Ok(Outcome::Done)
    }

    /// Mensajes de la lista actual cuyo remitente es `addr`, en el orden de la lista. En
    /// conversaciones cuenta el mensaje que representa cada fila.
    pub fn rows_from_sender(&mut self, addr: &str) -> Vec<MessageId> {
        let rows = self.rows_shared();
        let mut out = Vec::new();
        for r in rows.iter().take(MAX_SCAN) {
            let from = self.envelope(r.id).is_some_and(|e| e.from.iter().any(|m| m.addr.eq_ignore_ascii_case(addr)));
            if from {
                out.push(r.id);
            }
        }
        out
    }

    /// `mail.select_sender[:dirección]`: selecciona en la lista actual los correos de esa
    /// persona (la UI marca las filas con [`Session::take_select_request`]).
    pub(crate) fn run_select_sender(&mut self, arg: Option<&str>) -> Result<Outcome> {
        let Some(addr) = self.person_arg(arg) else {
            return Ok(Outcome::Notice(Notice::info(
                "Open a message or give an email address.",
                "Abre un mensaje o indica una dirección de correo.",
            )));
        };
        let ids = self.rows_from_sender(&addr);
        if ids.is_empty() {
            return Ok(Outcome::Notice(Notice::info(
                format!("There are no messages from {addr} in this list."),
                format!("No hay correos de {addr} en esta lista."),
            )));
        }
        let n = ids.len();
        self.selection = ids.clone();
        self.select_request = Some(ids);
        Ok(Outcome::Notice(if n == 1 {
            Notice::success(format!("1 message from {addr} selected"), format!("1 correo de {addr} seleccionado"))
        } else {
            Notice::success(
                format!("{n} messages from {addr} selected"),
                format!("{n} correos de {addr} seleccionados"),
            )
        }))
    }

    /// UX-25: selección por criterio (`mail.select.all`, `none`, `read`, `unread`, `flagged`,
    /// `unflagged`), como el desplegable de Gmail. «Ninguno» quita la selección.
    pub(crate) fn run_select_where(&mut self, what: &str) -> Result<Outcome> {
        let keep: fn(&iuredesk_model::Flags) -> bool = match what {
            "all" => |_| true,
            "none" => |_| false,
            "read" => |f| f.seen,
            "unread" => |f| !f.seen,
            "flagged" => |f| f.flagged,
            "unflagged" => |f| !f.flagged,
            _ => return Err(crate::EngineError::UnknownCommand(format!("mail.select.{what}"))),
        };
        let rows = self.rows_shared();
        let mut ids = Vec::new();
        if what != "none" {
            for r in rows.iter().take(MAX_SCAN) {
                if self.envelope(r.id).is_some_and(|e| keep(&e.flags)) {
                    ids.push(r.id);
                }
            }
        }
        let n = ids.len();
        self.selection = ids.clone();
        self.select_request = Some(ids);
        if what == "none" {
            return Ok(Outcome::Done);
        }
        Ok(Outcome::Notice(match n {
            0 => Notice::info("Nothing in this list matches.", "Nada en esta lista coincide."),
            1 => Notice::success("1 message selected", "1 correo seleccionado"),
            n => Notice::success(format!("{n} messages selected"), format!("{n} correos seleccionados")),
        }))
    }

    /// Selección que la UI debe marcar en la lista (una vez).
    pub fn take_select_request(&mut self) -> Option<Vec<MessageId>> {
        self.select_request.take()
    }
}

#[cfg(test)]
mod tests {
    use super::clean_address;

    #[test]
    fn addresses_are_cleaned_or_rejected() {
        assert_eq!(clean_address(" <Ana@Example.com> ").as_deref(), Some("Ana@Example.com"));
        assert_eq!(clean_address("ana@example.com").as_deref(), Some("ana@example.com"));
        assert_eq!(clean_address(""), None);
        assert_eq!(clean_address("sin arroba"), None);
        assert_eq!(clean_address("a b@example.com"), None);
        assert_eq!(clean_address("a@b.com OR asunto:x"), None);
        assert_eq!(clean_address("\"x\"@example.com"), None);
        assert_eq!(clean_address(&format!("{}@x.com", "a".repeat(400))), None);
    }
}
