//! Prueba IMAP con una máquina de estados mínima (RFC 3501/9051): saludo, CAPABILITY, STARTTLS,
//! autenticación, `LIST "" "*"` acotado y LOGOUT.

use iuredesk_model::account::{Security, ServerConfig};

use super::conn::{LineConn, MAX_LITERAL};
use super::{MAX_FOLDERS, ProbeError, ProbeOptions, ProbeReport, classify_auth_failure, clip, sasl_plain, xoauth2};

/// Tope de líneas de una respuesta (un `LIST` de un buzón enorme se corta aquí).
const MAX_LINES: usize = 20_000;
/// Tope de una respuesta con sus literales.
const MAX_RESPONSE_LINE: usize = 128 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Ok,
    No,
    Bad,
}

#[derive(Debug)]
struct Response {
    status: Status,
    text: String,
    untagged: Vec<String>,
    untagged_count: usize,
}

struct Imap {
    conn: LineConn,
    next: u32,
}

/// `{123}` o `{123+}` al final de una línea.
pub(crate) fn trailing_literal(line: &str) -> Option<usize> {
    let inner = line.strip_suffix('}')?;
    let open = inner.rfind('{')?;
    let n = inner.get(open + 1..)?.trim_end_matches('+');
    if n.is_empty() || n.len() > 9 || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    n.parse().ok()
}

/// Capacidades de `* CAPABILITY …` o de un código `[CAPABILITY …]`.
pub(crate) fn parse_capabilities(text: &str) -> Option<Vec<String>> {
    let upper = text.to_ascii_uppercase();
    let rest = if let Some(i) = upper.find("[CAPABILITY ") {
        let s = text.get(i + 12..)?;
        s.get(..s.find(']')?)?
    } else {
        let i = upper.find("* CAPABILITY ")?;
        text.get(i + 13..)?
    };
    Some(rest.split_ascii_whitespace().take(200).map(|c| c.to_ascii_uppercase()).collect())
}

/// Nombre del buzón de una línea `* LIST (\flags) "/" nombre`.
pub(crate) fn list_name(line: &str) -> Option<String> {
    let rest = line.strip_prefix("* LIST ").or_else(|| line.strip_prefix("* list "))?;
    let after_flags = rest.get(rest.find(')')? + 1..)?.trim_start();
    // Delimitador: NIL o una cadena entre comillas.
    let after_delim = if let Some(r) = after_flags.strip_prefix("NIL") {
        r
    } else {
        let r = after_flags.strip_prefix('"')?;
        let end = if r.starts_with("\\") { 2 } else { 1 };
        r.get(end..)?.strip_prefix('"')?
    };
    let name = after_delim.trim();
    let name = match name.strip_prefix('"').and_then(|n| n.strip_suffix('"')) {
        Some(q) => q.replace("\\\"", "\"").replace("\\\\", "\\"),
        None => name.to_string(),
    };
    (!name.is_empty()).then(|| name.chars().take(200).collect())
}

/// Cadena IMAP entre comillas.
fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

impl Imap {
    fn tag(&mut self) -> String {
        self.next = self.next.saturating_add(1);
        format!("A{}", self.next)
    }

    /// Lee una línea completa con sus literales.
    async fn read_full_line(&mut self) -> Result<String, ProbeError> {
        let mut line = self.conn.read_line().await?;
        for _ in 0..64 {
            let Some(n) = trailing_literal(&line) else { break };
            if n > MAX_LITERAL || line.len().saturating_add(n) > MAX_RESPONSE_LINE {
                return Err(ProbeError::Protocol("literal demasiado grande".into()));
            }
            let cut = line.rfind('{').unwrap_or(line.len());
            line.truncate(cut);
            let literal = self.conn.read_bytes(n).await?;
            line.push_str(&quoted(&literal));
            let more = self.conn.read_line().await?;
            line.push_str(&more);
        }
        Ok(line)
    }

    /// Manda un comando y lee hasta su respuesta etiquetada. `continuations` se mandan, en orden,
    /// cada vez que el servidor pide más (`+`); si se acaban, se responde una línea vacía (así
    /// termina un AUTHENTICATE que falló con un desafío de error, como XOAUTH2).
    async fn command(&mut self, command: &str, continuations: &[String]) -> Result<Response, ProbeError> {
        let tag = self.tag();
        self.conn.write_line(&format!("{tag} {command}")).await?;
        let prefix = format!("{tag} ");
        let mut untagged = Vec::new();
        let mut untagged_count = 0usize;
        let mut pending = continuations.iter();
        let mut empty_sent = 0u8;
        for _ in 0..MAX_LINES {
            let line = self.read_full_line().await?;
            if let Some(rest) = line.strip_prefix(&prefix) {
                let (word, text) = rest.split_once(' ').unwrap_or((rest, ""));
                let status = match word.to_ascii_uppercase().as_str() {
                    "OK" => Status::Ok,
                    "NO" => Status::No,
                    "BAD" => Status::Bad,
                    _ => return Err(ProbeError::Protocol(clip(&line))),
                };
                return Ok(Response { status, text: clip(text), untagged, untagged_count });
            }
            if line.starts_with('+') {
                match pending.next() {
                    Some(c) => self.conn.write_line(c).await?,
                    None if empty_sent < 2 => {
                        empty_sent += 1;
                        self.conn.write_line("").await?;
                    }
                    None => return Err(ProbeError::Protocol(clip(&line))),
                }
                continue;
            }
            if line.starts_with("* ") {
                untagged_count = untagged_count.saturating_add(1);
                if untagged.len() < 1000 {
                    untagged.push(line);
                }
                continue;
            }
            return Err(ProbeError::Protocol(clip(&line)));
        }
        Err(ProbeError::Protocol("respuesta demasiado larga".into()))
    }

    async fn capabilities(&mut self) -> Result<Vec<String>, ProbeError> {
        let r = self.command("CAPABILITY", &[]).await?;
        if r.status != Status::Ok {
            return Err(ProbeError::Protocol(r.text));
        }
        Ok(r.untagged.iter().find_map(|l| parse_capabilities(l)).unwrap_or_default())
    }
}

fn has(caps: &[String], cap: &str) -> bool {
    caps.iter().any(|c| c.eq_ignore_ascii_case(cap))
}

pub(super) async fn probe(
    cfg: &ServerConfig,
    secret: &str,
    oauth: bool,
    opts: ProbeOptions,
) -> Result<ProbeReport, ProbeError> {
    let conn = LineConn::connect(&cfg.host, cfg.port, cfg.security == Security::Tls, opts.io_timeout).await?;
    let mut imap = Imap { conn, next: 0 };
    let greeting = imap.conn.read_line().await?;
    let upper = greeting.to_ascii_uppercase();
    let preauth = if upper.starts_with("* OK") {
        false
    } else if upper.starts_with("* PREAUTH") {
        true
    } else if upper.starts_with("* BYE") {
        return Err(match classify_auth_failure(&greeting) {
            e @ ProbeError::TooManyConnections(_) => e,
            _ => ProbeError::Protocol(clip(&greeting)),
        });
    } else {
        return Err(ProbeError::Protocol(clip(&greeting)));
    };
    let mut report = ProbeReport { greeting: clip(&greeting), ..Default::default() };
    let mut caps = match parse_capabilities(&greeting) {
        Some(c) => c,
        None => imap.capabilities().await?,
    };

    if cfg.security == Security::StartTls {
        if !has(&caps, "STARTTLS") {
            return Err(ProbeError::StartTlsUnavailable);
        }
        let r = imap.command("STARTTLS", &[]).await?;
        if r.status != Status::Ok {
            return Err(ProbeError::Tls(r.text));
        }
        imap.conn = imap.conn.upgrade().await?;
        // Las capacidades de antes de cifrar no valen (RFC 3501 §6.2.1).
        caps = imap.capabilities().await?;
    }

    if !preauth {
        let r = if oauth {
            if !has(&caps, "AUTH=XOAUTH2") {
                return Err(ProbeError::AuthUnsupported);
            }
            let token = xoauth2(&cfg.username, secret);
            if has(&caps, "SASL-IR") {
                imap.command(&format!("AUTHENTICATE XOAUTH2 {token}"), &[]).await?
            } else {
                imap.command("AUTHENTICATE XOAUTH2", &[token]).await?
            }
        } else if has(&caps, "AUTH=PLAIN") {
            let token = sasl_plain(&cfg.username, secret);
            if has(&caps, "SASL-IR") {
                imap.command(&format!("AUTHENTICATE PLAIN {token}"), &[]).await?
            } else {
                imap.command("AUTHENTICATE PLAIN", &[token]).await?
            }
        } else if has(&caps, "LOGINDISABLED") {
            return Err(if cfg.security == Security::Plain {
                ProbeError::StartTlsUnavailable
            } else {
                ProbeError::AuthUnsupported
            });
        } else {
            imap.command(&format!("LOGIN {} {}", quoted(&cfg.username), quoted(secret)), &[]).await?
        };
        if r.status != Status::Ok {
            return Err(classify_auth_failure(&r.text));
        }
        if let Some(c) = parse_capabilities(&r.text) {
            caps = c;
        } else if let Ok(c) = imap.capabilities().await {
            caps = c;
        }
    }
    report.capabilities = caps;

    let list = imap.command("LIST \"\" \"*\"", &[]).await?;
    if list.status == Status::Ok {
        report.folder_count = list.untagged_count;
        report.folders = list.untagged.iter().filter_map(|l| list_name(l)).take(MAX_FOLDERS).collect();
    }
    // Salir con cortesía; si falla, la prueba ya pasó.
    imap.command("LOGOUT", &[]).await.ok();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals() {
        assert_eq!(trailing_literal("* LIST () \"/\" {5}"), Some(5));
        assert_eq!(trailing_literal("A1 LOGIN {3+}"), Some(3));
        assert_eq!(trailing_literal("* OK {x}"), None);
        assert_eq!(trailing_literal("* OK {}"), None);
        assert_eq!(trailing_literal("* OK {99999999999}"), None);
        assert_eq!(trailing_literal("plain"), None);
    }

    #[test]
    fn capabilities() {
        let c = parse_capabilities("* OK [CAPABILITY IMAP4rev1 SASL-IR AUTH=PLAIN] ready").unwrap();
        assert_eq!(c, vec!["IMAP4REV1", "SASL-IR", "AUTH=PLAIN"]);
        let c = parse_capabilities("* CAPABILITY IMAP4rev1 STARTTLS LOGINDISABLED").unwrap();
        assert!(c.contains(&"STARTTLS".to_string()));
        assert_eq!(parse_capabilities("* OK hola"), None);
        assert_eq!(parse_capabilities("* OK [CAPABILITY sin cierre"), None);
    }

    #[test]
    fn list_names() {
        assert_eq!(list_name(r#"* LIST (\HasNoChildren) "/" "INBOX""#).as_deref(), Some("INBOX"));
        assert_eq!(list_name(r#"* LIST (\HasNoChildren) "." Sent"#).as_deref(), Some("Sent"));
        assert_eq!(list_name(r#"* LIST () NIL "Con \"comillas\"""#).as_deref(), Some("Con \"comillas\""));
        assert_eq!(list_name(r#"* LIST (\Noselect) "\\" "a\\b""#).as_deref(), Some("a\\b"));
        assert_eq!(list_name("* LIST sin parentesis"), None);
        assert_eq!(list_name("* OK"), None);
    }

    proptest::proptest! {
        #[test]
        fn helpers_never_panic(s in "\\PC{0,400}") {
            let _ = trailing_literal(&s);
            let _ = parse_capabilities(&s);
            let _ = list_name(&s);
            let _ = super::super::classify_auth_failure(&s);
            let _ = super::super::pop3::apop_timestamp(&s);
        }

        #[test]
        fn list_lines_never_panic(flags in "[\\\\A-Za-z ]{0,30}", delim in "\\PC{0,3}", name in "\\PC{0,40}") {
            let _ = list_name(&format!("* LIST ({flags}) \"{delim}\" {name}"));
            let _ = list_name(&format!("* LIST ({flags}) {delim}{name}"));
        }
    }

    #[test]
    fn quoting() {
        assert_eq!(quoted(r#"pa"ss\"#), r#""pa\"ss\\""#);
    }
}
