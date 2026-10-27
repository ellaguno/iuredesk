//! Voz con IureVoz: decir un texto, tomar dictado, preguntar sí o no.
//!
//! 1. **Socket de control** (`$XDG_RUNTIME_DIR/iurevoz.sock`, solo Unix): una orden por conexión,
//!    `decir {"texto":…}`, `si_no {"pregunta":…}` o `escuchar {"pregunta":…,"pausa":…}`; se
//!    cierra la escritura y la respuesta llega por el mismo socket (`ok`, `si`/`no`/vacío, el
//!    texto oído).
//! 2. Si el socket no responde y IureVoz está instalado: **MCP** (`iurevoz --mcp`, herramientas
//!    `decir`, `preguntar_si_no`, `dictar`). En Windows es el único camino (IureVoz usa sockets
//!    Unix de Windows, que `std` no abre).
//!
//! Las dos vías necesitan IureVoz **abierto** (el micrófono y la voz viven en su proceso); si no
//! lo está, el resultado es [`Outcome::NotAvailable`] con «Abre IureVoz…».

use std::time::Duration;

use serde_json::{Value, json};

use crate::error::{IntegrationError, Result};
use crate::suite::{Outcome, SisterApp, Suite, Text};

/// Esperas de IureVoz por orden (las suyas + margen).
const SPEAK_WAIT: Duration = Duration::from_secs(330);
const YES_NO_WAIT: Duration = Duration::from_secs(130);
const LISTEN_WAIT: Duration = Duration::from_secs(630);
/// Tope de la respuesta del socket.
const MAX_REPLY: u64 = 1024 * 1024;

fn not_open() -> Text {
    Text::new(
        "Open IureVoz to speak and listen (it keeps the microphone and the voice).",
        "Abre IureVoz para hablar y escuchar (ahí viven el micrófono y la voz).",
    )
}

/// Orden por el socket. `Ok(None)` = no hay nadie escuchando.
#[cfg(unix)]
fn socket_request(path: &std::path::Path, command: &str, args: &Value, wait: Duration) -> Result<Option<String>> {
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    let Ok(mut s) = UnixStream::connect(path) else { return Ok(None) };
    s.set_read_timeout(Some(wait))?;
    s.set_write_timeout(Some(Duration::from_secs(5)))?;
    s.write_all(format!("{command} {args}").as_bytes())?;
    s.shutdown(std::net::Shutdown::Write)?;
    let bytes = crate::mcp_client::read_capped(&s, MAX_REPLY).map_err(|e| match e.kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => IntegrationError::Timeout(wait),
        _ => IntegrationError::Io(e),
    })?;
    Ok(Some(String::from_utf8_lossy(&bytes).trim().to_string()))
}

#[cfg(not(unix))]
fn socket_request(_: &std::path::Path, _: &str, _: &Value, _: Duration) -> Result<Option<String>> {
    Ok(None)
}

impl Suite {
    /// ¿Hay un IureVoz abierto escuchando en el socket?
    pub fn voice_ready(&self) -> bool {
        #[cfg(unix)]
        if let Some(p) = &self.voz_socket {
            return std::os::unix::net::UnixStream::connect(p).is_ok();
        }
        false
    }

    /// Socket primero; si nadie escucha, MCP. Devuelve la respuesta en crudo.
    fn voz(
        &self,
        socket_cmd: &str,
        socket_args: Value,
        tool: &str,
        tool_args: Value,
        wait: Duration,
    ) -> Result<Outcome<Value>> {
        if let Some(path) = &self.voz_socket
            && let Some(reply) = socket_request(path, socket_cmd, &socket_args, wait)?
        {
            return Ok(Outcome::Ready(Value::String(reply)));
        }
        let mut client = match self.connect_mcp(SisterApp::Voz)? {
            Outcome::Ready(c) => c,
            Outcome::NotInstalled(h) => return Ok(Outcome::NotInstalled(h)),
            Outcome::NotAvailable(t) => return Ok(Outcome::NotAvailable(t)),
        };
        let r = client.call_tool(tool, tool_args, Some(wait))?;
        if r.is_error {
            // «IureVoz no está abierto…»: el proceso MCP no encontró la app.
            return Ok(Outcome::NotAvailable(not_open()));
        }
        Ok(Outcome::Ready(r.data().unwrap_or(Value::Null)))
    }

    /// Dice un texto en voz alta y espera a que termine (frases cortas, sin Markdown).
    pub fn speak(&self, text: &str) -> Result<Outcome<()>> {
        let text = text.trim();
        if text.is_empty() {
            return Err(IntegrationError::InvalidInput("texto vacío".into()));
        }
        let r = self.voz("decir", json!({"texto": text}), "decir", json!({"texto": text}), SPEAK_WAIT)?;
        Ok(r.map(|_| ()))
    }

    /// Toma un dictado largo (termina con ~3 s de silencio). `prompt` se dice antes. Vacío = el
    /// usuario no dijo nada.
    pub fn dictate(&self, prompt: Option<&str>) -> Result<Outcome<String>> {
        let prompt = prompt.map(str::trim).filter(|p| !p.is_empty());
        let r = self.voz(
            "escuchar",
            json!({"pregunta": prompt, "pausa": 3.0}),
            "dictar",
            match prompt {
                Some(p) => json!({"indicacion": p}),
                None => json!({}),
            },
            LISTEN_WAIT,
        )?;
        Ok(r.map(|v| match v {
            Value::String(s) => s,
            other => other.get("texto").and_then(Value::as_str).unwrap_or("").trim().to_string(),
        }))
    }

    /// Pregunta de sí o no. `None` = no respondió a tiempo.
    pub fn ask_yes_no(&self, question: &str) -> Result<Outcome<Option<bool>>> {
        let q = question.trim();
        if q.is_empty() {
            return Err(IntegrationError::InvalidInput("pregunta vacía".into()));
        }
        let r = self.voz("si_no", json!({"pregunta": q}), "preguntar_si_no", json!({"pregunta": q}), YES_NO_WAIT)?;
        Ok(r.map(|v| {
            let answer = match &v {
                Value::String(s) => s.as_str(),
                other => other.get("respuesta").and_then(Value::as_str).unwrap_or(""),
            };
            match answer.trim() {
                "si" | "sí" => Some(true),
                "no" => Some(false),
                _ => None,
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sin_socket_ni_app() {
        let s = Suite::empty();
        assert!(!s.voice_ready());
        assert!(matches!(s.speak("hola"), Ok(Outcome::NotInstalled(h)) if h.app == SisterApp::Voz));
        assert!(matches!(s.ask_yes_no("¿sí?"), Ok(Outcome::NotInstalled(_))));
        assert!(matches!(s.dictate(None), Ok(Outcome::NotInstalled(_))));
        assert!(s.speak("  ").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn socket_de_control() {
        use std::io::{Read, Write};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("iurevoz.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let mut got = Vec::new();
            for _ in 0..3 {
                let (mut c, _) = listener.accept().unwrap();
                let mut order = String::new();
                c.read_to_string(&mut order).unwrap();
                let reply = if order.starts_with("decir ") {
                    "ok"
                } else if order.starts_with("si_no ") {
                    "si"
                } else {
                    "  texto dictado \n"
                };
                c.write_all(reply.as_bytes()).unwrap();
                got.push(order);
            }
            got
        });
        let s = Suite::empty().with_voz_socket(Some(path));
        assert!(matches!(s.speak("Hola"), Ok(Outcome::Ready(()))));
        assert_eq!(s.ask_yes_no("¿Envío?").unwrap(), Outcome::Ready(Some(true)));
        assert_eq!(s.dictate(Some("Dicta")).unwrap(), Outcome::Ready("texto dictado".to_string()));
        let got = server.join().unwrap();
        assert_eq!(got.first().map(String::as_str), Some(r#"decir {"texto":"Hola"}"#));
        assert!(got.get(2).is_some_and(|o| o.starts_with("escuchar ") && o.contains("\"pausa\":3.0")));
    }
}
