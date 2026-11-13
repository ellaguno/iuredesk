//! R4.5: un mensaje de más de ~4 MB por Graph se manda en partes contra un servidor falso:
//! borrador desde el MIME sin los adjuntos grandes (con `Bcc:` y sus cabeceras), grande por sesión
//! de subida en trozos (sin token) y `send`. Si la subida falla, el borrador se borra y el error
//! llega como envío fallido.

#[path = "../../dav/tests/common/mod.rs"]
mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use common::{Req, Resp, serve};
use iuredesk_compose::{OutgoingMessage, Transport};
use iuredesk_graph::{GraphClient, GraphSendTransport, MAX_MIME_BYTES};
use iuredesk_model::account::{Account, AccountKind, AuthMethod, OAuthProvider, Protocol, Security, ServerConfig};
use iuredesk_oauth::{BoxFuture, TokenAudience, TokenProvider};
use serde_json::{Value, json};

struct FakeTokens;

impl TokenProvider for FakeTokens {
    fn access_token<'a>(
        &'a self,
        _a: &'a Account,
        _aud: TokenAudience,
    ) -> BoxFuture<'a, Result<String, iuredesk_oauth::Error>> {
        Box::pin(async { Ok("tok-graph".to_string()) })
    }
    fn invalidate(&self, _account_id: &str) {}
}

fn account() -> Account {
    let srv = |p| ServerConfig {
        protocol: p,
        host: "outlook.office365.com".into(),
        port: 993,
        security: Security::Tls,
        auth: AuthMethod::OAuth2 { provider: OAuthProvider::Microsoft },
        username: "ana@contoso.com".into(),
    };
    Account {
        id: "m365".into(),
        display_name: "Ana".into(),
        email: "ana@contoso.com".into(),
        kind: AccountKind::Microsoft365,
        incoming: srv(Protocol::Imap),
        outgoing: srv(Protocol::Smtp),
        caldav_url: None,
        carddav_url: None,
        identities: vec![],
        shared_of: None,
    }
}

#[derive(Default)]
struct G {
    base: String,
    draft_mime: String,
    small: Vec<Value>,
    session: Option<Value>,
    /// (Content-Range, ¿llevaba token?, tamaño)
    chunks: Vec<(String, bool, usize)>,
    received: usize,
    sent: bool,
    deleted: bool,
    fail_upload: bool,
}

const DRAFT: &str = "/v1.0/me/messages/AAMk%2Fborrador%3D";

fn handle(g: &Mutex<G>, req: Req) -> Resp {
    let mut g = g.lock().unwrap();
    let path = req.path.split('?').next().unwrap_or_default().to_string();
    let authed = req.header("authorization").is_some();
    let m = req.method.as_str();
    if m == "POST" && path == "/v1.0/me/messages" {
        let mime = base64::engine::general_purpose::STANDARD.decode(req.body.trim()).unwrap_or_default();
        g.draft_mime = String::from_utf8_lossy(&mime).into_owned();
        return Resp::new(201, json!({"id": "AAMk/borrador="}).to_string());
    }
    if m == "POST" && path == format!("{DRAFT}/attachments") {
        g.small.push(serde_json::from_str(&req.body).unwrap_or(Value::Null));
        return Resp::new(201, "{}");
    }
    if m == "POST" && path == format!("{DRAFT}/attachments/createUploadSession") {
        g.session = serde_json::from_str(&req.body).ok();
        let url = format!("{}/upload/abc", g.base);
        return Resp::new(201, json!({"uploadUrl": url}).to_string());
    }
    if m == "PUT" && path == "/upload/abc" {
        if g.fail_upload {
            return Resp::new(500, "");
        }
        let range = req.header("content-range").unwrap_or_default().to_string();
        let n: usize = req.header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
        g.received += n;
        g.chunks.push((range, authed, n));
        let total = g.session.as_ref().and_then(|s| s["AttachmentItem"]["size"].as_u64()).unwrap_or(0) as usize;
        return Resp::new(if g.received >= total { 201 } else { 200 }, "{}");
    }
    if m == "POST" && path == format!("{DRAFT}/send") {
        g.sent = true;
        return Resp::new(202, "");
    }
    if m == "DELETE" && path == DRAFT {
        g.deleted = true;
        return Resp::new(204, "");
    }
    Resp::new(404, json!({"error": {"code": "NotFound"}}).to_string())
}

/// MIME con un cuerpo y dos adjuntos: uno chico y otro de 4 MB (en base64 pasa del tope).
fn big_message() -> Vec<u8> {
    let b64 = |d: &[u8]| {
        let s = base64::engine::general_purpose::STANDARD.encode(d);
        s.as_bytes().chunks(76).map(|l| String::from_utf8_lossy(l).into_owned()).collect::<Vec<_>>().join("\r\n")
    };
    let small = vec![b's'; 20_000];
    let big = vec![b'B'; 4_000_000];
    format!(
        "From: ana@contoso.com\r\nTo: bob@x.com\r\nSubject: Planos\r\nIn-Reply-To: <hilo@x.com>\r\nMIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=\"=_b\"\r\n\r\n--=_b\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
         Van los planos.\r\n--=_b\r\nContent-Type: application/pdf; name=\"nota.pdf\"\r\nContent-Disposition: attachment; \
         filename=\"nota.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n--=_b\r\nContent-Type: image/png; \
         name=\"plano.png\"\r\nContent-Disposition: attachment; filename=\"plano.png\"\r\nContent-Transfer-Encoding: \
         base64\r\n\r\n{}\r\n--=_b--\r\n",
        b64(&small),
        b64(&big)
    )
    .into_bytes()
}

async fn transport(state: &Arc<Mutex<G>>) -> GraphSendTransport {
    let s = Arc::clone(state);
    let base = serve(move |req| handle(&s, req)).await;
    state.lock().unwrap().base = base.clone();
    let client =
        GraphClient::new(account(), Arc::new(FakeTokens), &format!("{base}/v1.0"), Duration::from_secs(10)).unwrap();
    GraphSendTransport::new(client)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn large_message_goes_in_parts() {
    let state = Arc::new(Mutex::new(G::default()));
    let t = transport(&state).await;
    let raw = big_message();
    assert!(raw.len() > MAX_MIME_BYTES);
    let out = OutgoingMessage {
        envelope_from: "ana@contoso.com".into(),
        recipients: vec!["bob@x.com".into(), "oculto@x.com".into()],
        raw,
    };
    let receipt = t.send(&out).await.unwrap();
    assert!(receipt.server_reply.contains("202"), "{}", receipt.server_reply);
    let g = state.lock().unwrap();
    // Borrador: cabeceras, cuerpo y Bcc; el adjunto grande ya no va dentro.
    assert!(g.draft_mime.starts_with("Bcc: oculto@x.com\r\n"), "{}", &g.draft_mime[..80]);
    assert!(g.draft_mime.contains("In-Reply-To: <hilo@x.com>"));
    assert!(g.draft_mime.contains("Van los planos."));
    assert!(g.draft_mime.contains("nota.pdf"), "el adjunto chico se queda en el MIME");
    assert!(!g.draft_mime.contains("plano.png"));
    assert!(g.small.is_empty(), "el chico no se separó");
    // Sesión de subida del grande, en trozos sin token y cubriendo todo.
    let item = &g.session.as_ref().unwrap()["AttachmentItem"];
    assert_eq!(item["name"], "plano.png");
    assert_eq!(item["contentType"], "image/png");
    assert_eq!(item["size"], 4_000_000);
    assert_eq!(g.chunks.len(), 2, "{:?}", g.chunks);
    assert!(g.chunks.iter().all(|(_, authed, _)| !authed), "la URL firmada no lleva token");
    assert_eq!(g.chunks[0].0, "bytes 0-3276799/4000000");
    assert_eq!(g.chunks[1].0, "bytes 3276800-3999999/4000000");
    assert_eq!(g.received, 4_000_000);
    assert!(g.sent && !g.deleted);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_upload_deletes_the_draft() {
    let state = Arc::new(Mutex::new(G { fail_upload: true, ..Default::default() }));
    let t = transport(&state).await;
    let out = OutgoingMessage {
        envelope_from: "ana@contoso.com".into(),
        recipients: vec!["bob@x.com".into()],
        raw: big_message(),
    };
    let err = t.send(&out).await.unwrap_err();
    assert!(err.is_transient(), "un 500 al subir se reintenta luego: {err:?}");
    let g = state.lock().unwrap();
    assert!(!g.sent && g.deleted, "sin borrador a medias");
}
