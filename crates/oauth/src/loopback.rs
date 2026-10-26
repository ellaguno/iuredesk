//! Receptor *loopback* (RFC 8252 §7.3): un servidor HTTP mínimo en `127.0.0.1` que recibe la
//! redirección del navegador con `code` y `state`, contesta una página «Ya puedes volver a
//! IureDesk» y entrega el resultado.
//!
//! Todo lo que llega es hostil: petición acotada a 8 KiB, tiempo de espera por conexión, número
//! máximo de conexiones, y solo se atiende `GET /`.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::Error;

/// Tamaño máximo de la petición del navegador.
const MAX_REQUEST: usize = 8 * 1024;
/// Tiempo para que una conexión mande su petición (los navegadores abren conexiones especulativas
/// que no mandan nada; cada una se atiende en su propia tarea).
const CONN_TIMEOUT: Duration = Duration::from_secs(10);
/// Conexiones que se atienden como mucho antes de rendirse.
const MAX_CONNECTIONS: usize = 64;

/// Resultado de una petición: `None` si no era la redirección (p. ej. `/favicon.ico`).
type Outcome = Option<Result<String, Error>>;

/// Espera la redirección y devuelve el `code`. Sin límite de tiempo propio: quien llama lo envuelve
/// en su tiempo de espera y su cancelación.
pub(crate) async fn wait_for_code(listener: &TcpListener, expected_state: &str) -> Result<String, Error> {
    let (tx, mut rx) = mpsc::channel::<Result<String, Error>>(4);
    let mut tasks = JoinSet::new();
    let mut accepted = 0usize;
    loop {
        tokio::select! {
            got = rx.recv() => {
                // `tx` sigue vivo aquí, así que `None` no ocurre; por si acaso, se trata como error.
                return got.unwrap_or_else(|| Err(Error::Listener("canal cerrado".into())));
            }
            conn = listener.accept(), if accepted < MAX_CONNECTIONS => {
                let (stream, _) = conn.map_err(|e| Error::Listener(e.to_string()))?;
                accepted = accepted.saturating_add(1);
                let tx = tx.clone();
                let state = expected_state.to_string();
                tasks.spawn(async move {
                    if let Ok(Some(r)) = tokio::time::timeout(CONN_TIMEOUT, handle(stream, &state)).await {
                        let _ = tx.send(r).await;
                    }
                });
            }
            // Agotado el cupo solo se espera a las conexiones en curso o al tiempo de quien llama.
        }
    }
}

/// Atiende una conexión.
async fn handle(mut stream: TcpStream, expected_state: &str) -> Outcome {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(chunk.get(..n)?);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() > MAX_REQUEST {
            let _ = respond(&mut stream, 431, false).await;
            return None;
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let line = text.lines().next().unwrap_or("");
    let mut parts = line.split(' ');
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if method != "GET" || path != "/" {
        let _ = respond(&mut stream, 404, false).await;
        return None;
    }
    let outcome = parse_query(query, expected_state);
    let _ = respond(&mut stream, if outcome.is_ok() { 200 } else { 400 }, outcome.is_ok()).await;
    Some(outcome)
}

/// Interpreta la consulta de la redirección (RFC 6749 §4.1.2).
pub(crate) fn parse_query(query: &str, expected_state: &str) -> Result<String, Error> {
    let mut code = None;
    let mut state = None;
    let mut error = None;
    let mut description = None;
    for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
        let v: String = v.chars().take(2048).collect();
        match k.as_ref() {
            "code" => code = Some(v),
            "state" => state = Some(v),
            "error" => error = Some(v),
            "error_description" => description = Some(v),
            _ => {}
        }
    }
    if state.as_deref() != Some(expected_state) {
        return Err(Error::StateMismatch);
    }
    if let Some(error) = error {
        return Err(if error == "access_denied" {
            Error::AccessDenied(description.unwrap_or_default())
        } else {
            Error::Authorization { error, description }
        });
    }
    match code {
        Some(c) if !c.is_empty() => Ok(c),
        _ => Err(Error::InvalidCallback("falta el parámetro code".into())),
    }
}

/// Página que ve el usuario en el navegador, en español e inglés.
fn page(ok: bool) -> String {
    let (title, es, en) = if ok {
        (
            "IureDesk",
            "Listo. Ya puedes volver a IureDesk y cerrar esta pestaña.",
            "Done. You can now return to IureDesk and close this tab.",
        )
    } else {
        (
            "IureDesk",
            "No se completó el inicio de sesión. Vuelve a IureDesk para intentarlo de nuevo.",
            "Sign-in was not completed. Go back to IureDesk to try again.",
        )
    };
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title>\
         <meta name=\"viewport\" content=\"width=device-width\">\
         <style>body{{font-family:system-ui,sans-serif;max-width:32em;margin:4em auto;padding:0 1em;\
         color:#222;background:#fafafa}}@media(prefers-color-scheme:dark){{body{{color:#eee;\
         background:#1e1e1e}}}}h1{{font-size:1.4em}}p{{line-height:1.5}}</style></head><body>\
         <h1>{title}</h1><p lang=\"es\">{es}</p><p lang=\"en\">{en}</p></body></html>"
    )
}

async fn respond(stream: &mut TcpStream, status: u16, ok: bool) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Request Header Fields Too Large",
    };
    let body = page(ok);
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.shutdown().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_cases() {
        assert_eq!(parse_query("code=abc&state=s1", "s1").unwrap(), "abc");
        assert_eq!(parse_query("code=abc&state=s2", "s1"), Err(Error::StateMismatch));
        assert_eq!(parse_query("code=abc", "s1"), Err(Error::StateMismatch));
        assert!(matches!(parse_query("error=access_denied&state=s1", "s1"), Err(Error::AccessDenied(_))));
        assert!(matches!(
            parse_query("error=invalid_scope&error_description=x%20y&state=s1", "s1"),
            Err(Error::Authorization { error, description: Some(d) }) if error == "invalid_scope" && d == "x y"
        ));
        assert!(matches!(parse_query("state=s1&code=", "s1"), Err(Error::InvalidCallback(_))));
    }

    proptest::proptest! {
        #[test]
        fn parse_query_never_panics(q in ".{0,300}") {
            let _ = parse_query(&q, "s");
        }
    }

    #[test]
    fn page_is_bilingual() {
        let p = page(true);
        assert!(p.contains("Ya puedes volver a IureDesk"));
        assert!(p.contains("return to IureDesk"));
    }
}
