//! Prueba POP3 (RFC 1939, 2449, 2595): saludo, STLS, USER/PASS (o APOP), AUTH XOAUTH2, STAT y QUIT.

use iuredesk_model::account::{Security, ServerConfig};

use super::conn::LineConn;
use super::{ProbeError, ProbeOptions, ProbeReport, classify_auth_failure, clip, xoauth2};

/// Tope de líneas de una respuesta multilínea (CAPA).
const MAX_LINES: usize = 200;

struct Pop3 {
    conn: LineConn,
}

/// Respuesta de una línea: `Ok(texto)` con `+OK`, `Err(texto)` con `-ERR`.
fn status(line: &str) -> Result<Result<String, String>, ProbeError> {
    if let Some(rest) = line.strip_prefix("+OK") {
        Ok(Ok(rest.trim().to_string()))
    } else if let Some(rest) = line.strip_prefix("-ERR") {
        Ok(Err(rest.trim().to_string()))
    } else {
        Err(ProbeError::Protocol(clip(line)))
    }
}

/// Sello de tiempo de APOP en el saludo (`<1896.697170952@dbc.mtview.ca.us>`).
pub(crate) fn apop_timestamp(greeting: &str) -> Option<&str> {
    let start = greeting.find('<')?;
    let end = start + greeting.get(start..)?.find('>')?;
    let ts = greeting.get(start..=end)?;
    (ts.contains('@') && ts.len() <= 256 && ts.is_ascii()).then_some(ts)
}

/// Resumen APOP: `md5(sello + contraseña)` en hexadecimal.
pub(crate) fn apop_digest(timestamp: &str, secret: &str) -> String {
    use md5::{Digest, Md5};
    let d = Md5::digest(format!("{timestamp}{secret}").as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

impl Pop3 {
    async fn cmd(&mut self, line: &str) -> Result<Result<String, String>, ProbeError> {
        self.conn.write_line(line).await?;
        let resp = self.conn.read_line().await?;
        status(&resp)
    }

    async fn capa(&mut self) -> Result<Vec<String>, ProbeError> {
        match self.cmd("CAPA").await? {
            Ok(_) => {}
            Err(_) => return Ok(Vec::new()),
        }
        let mut caps = Vec::new();
        for _ in 0..MAX_LINES {
            let l = self.conn.read_line().await?;
            if l == "." {
                return Ok(caps);
            }
            caps.push(clip(&l).to_ascii_uppercase());
        }
        Err(ProbeError::Protocol("CAPA demasiado larga".into()))
    }
}

pub(super) async fn probe(
    cfg: &ServerConfig,
    secret: &str,
    oauth: bool,
    opts: ProbeOptions,
) -> Result<ProbeReport, ProbeError> {
    let conn = LineConn::connect(&cfg.host, cfg.port, cfg.security == Security::Tls, opts.io_timeout).await?;
    let mut pop = Pop3 { conn };
    let greeting = pop.conn.read_line().await?;
    if let Err(text) = status(&greeting)? {
        return Err(match classify_auth_failure(&text) {
            e @ ProbeError::TooManyConnections(_) => e,
            _ => ProbeError::Protocol(clip(&greeting)),
        });
    }
    let timestamp = apop_timestamp(&greeting).map(str::to_string);
    let mut report = ProbeReport { greeting: clip(&greeting), ..Default::default() };
    let mut caps = pop.capa().await?;

    if cfg.security == Security::StartTls {
        if !caps.iter().any(|c| c == "STLS") {
            return Err(ProbeError::StartTlsUnavailable);
        }
        if let Err(text) = pop.cmd("STLS").await? {
            return Err(ProbeError::Tls(text));
        }
        pop.conn = pop.conn.upgrade().await?;
        caps = pop.capa().await?;
    }

    if oauth {
        if !caps.iter().any(|c| c.starts_with("SASL") && c.contains("XOAUTH2")) {
            return Err(ProbeError::AuthUnsupported);
        }
        pop.conn.write_line(&format!("AUTH XOAUTH2 {}", xoauth2(&cfg.username, secret))).await?;
        let mut line = pop.conn.read_line().await?;
        if line.starts_with('+') && !line.starts_with("+OK") {
            // Desafío con el detalle del error: se responde vacío y llega el -ERR.
            pop.conn.write_line("").await?;
            line = pop.conn.read_line().await?;
        }
        if let Err(text) = status(&line)? {
            return Err(classify_auth_failure(&text));
        }
    } else {
        match pop.cmd(&format!("USER {}", cfg.username)).await? {
            Ok(_) => {
                if let Err(text) = pop.cmd(&format!("PASS {secret}")).await? {
                    return Err(classify_auth_failure(&text));
                }
            }
            Err(user_text) => {
                // Algunos servidores solo aceptan APOP.
                let Some(ts) = timestamp else { return Err(classify_auth_failure(&user_text)) };
                if let Err(text) = pop.cmd(&format!("APOP {} {}", cfg.username, apop_digest(&ts, secret))).await? {
                    return Err(classify_auth_failure(&text));
                }
            }
        }
    }
    report.capabilities = caps;

    if let Ok(text) = pop.cmd("STAT").await? {
        report.message_count = text.split_ascii_whitespace().next().and_then(|n| n.parse().ok());
    }
    pop.cmd("QUIT").await.ok();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apop_rfc1939_example() {
        let g = "+OK POP3 server ready <1896.697170952@dbc.mtview.ca.us>";
        let ts = apop_timestamp(g).unwrap();
        assert_eq!(ts, "<1896.697170952@dbc.mtview.ca.us>");
        assert_eq!(apop_digest(ts, "tanstaaf"), "c4c9334bac560ecc979e58001b3e22fb");
        assert_eq!(apop_timestamp("+OK ready"), None);
        assert_eq!(apop_timestamp("+OK <sin-arroba>"), None);
    }

    #[test]
    fn statuses() {
        assert_eq!(status("+OK 2 320").unwrap(), Ok("2 320".to_string()));
        assert_eq!(status("-ERR nope").unwrap(), Err("nope".to_string()));
        assert!(status("HTTP/1.1 400").is_err());
    }
}
