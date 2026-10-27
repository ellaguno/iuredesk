//! Los cuatro proveedores contra servidores HTTP falsos: flujo, errores, 429 con `Retry-After`,
//! cancelación, tiempo agotado, JSON mal formado (reparado o rechazado) y acciones confirmadas.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use chrono::NaiveDate;
use common::{Reply, serve};
use iuredesk_assistant::actions::{self, ProposedAction};
use iuredesk_assistant::providers::anthropic::AnthropicProvider;
use iuredesk_assistant::providers::copilot::{CopilotCapability, CopilotProvider};
use iuredesk_assistant::providers::iurefficient::IurefficientProvider;
use iuredesk_assistant::providers::openai::OpenAiProvider;
use iuredesk_assistant::schema::{LeadPayload, TicketPayload, TicketPriority};
use iuredesk_assistant::tasks::{self, Target, TaskOptions};
use iuredesk_assistant::{
    Addr, AiError, AiProvider, CancellationToken, ChatMessage, ChatRequest, EmailInput, Limits, StreamEvent, run_chat,
};
use iuredesk_model::account::{Account, AccountKind, AuthMethod, OAuthProvider, Protocol, Security, ServerConfig};
use iuredesk_oauth::{BoxFuture, TokenAudience, TokenProvider};
use iurefficient_connect::rest::Session;
use tokio::sync::mpsc;

fn req(text: &str) -> ChatRequest {
    ChatRequest {
        system: "Eres útil.".into(),
        messages: vec![ChatMessage::user(text)],
        max_output_tokens: 256,
        temperature: 0.0,
        json: false,
        case_id: None,
    }
}

fn fast_limits() -> Limits {
    Limits { timeout: Duration::from_secs(10), max_retry_wait: Duration::from_millis(50), ..Limits::default() }
}

async fn collect(
    p: &dyn AiProvider,
    r: &ChatRequest,
    limits: &Limits,
) -> (Result<iuredesk_assistant::Completion, AiError>, Vec<StreamEvent>) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let out = run_chat(p, r, limits, Some(&tx), &CancellationToken::new()).await;
    drop(tx);
    let mut evs = Vec::new();
    while let Some(e) = rx.recv().await {
        evs.push(e);
    }
    (out, evs)
}

fn deltas(evs: &[StreamEvent]) -> Vec<String> {
    evs.iter()
        .filter_map(|e| match e {
            StreamEvent::Delta(d) => Some(d.clone()),
            StreamEvent::Retrying { .. } => None,
        })
        .collect()
}

const ANTHROPIC_OK: &[&str] = &[
    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":12}}}\n\n",
    "event: ping\ndata: {\"type\":\"ping\"}\n\n",
    "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hola\"}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_",
    "delta\",\"text\":\", Ana\"}}\n\n",
    "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":4}}\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
];

#[tokio::test]
async fn anthropic_streams_text_with_headers_and_usage() {
    let srv = serve(vec![Reply::sse(ANTHROPIC_OK)]).await;
    let p = AnthropicProvider::new(&srv.url, "claude-sonnet-5-5", "sk-ant-test".into()).unwrap();
    let (out, evs) = collect(&p, &req("hola"), &fast_limits()).await;
    let c = out.unwrap();
    assert_eq!(c.text, "Hola, Ana");
    assert_eq!(deltas(&evs), ["Hola", ", Ana"]);
    assert_eq!(c.stop_reason.as_deref(), Some("end_turn"));
    assert_eq!(c.usage.unwrap().input_tokens, 12);
    assert_eq!(c.usage.unwrap().output_tokens, 4);
    let r = &srv.requests()[0];
    assert_eq!(r.path, "/v1/messages");
    assert_eq!(r.header("x-api-key"), Some("sk-ant-test"));
    assert_eq!(r.header("anthropic-version"), Some("2023-06-01"));
    let body = r.json();
    assert_eq!(body["model"], "claude-sonnet-5-5");
    assert_eq!(body["stream"], true);
    assert_eq!(body["system"], "Eres útil.");
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(p.kind().label(), "Anthropic · claude-sonnet-5-5");
}

#[tokio::test]
async fn anthropic_429_retry_after_then_success() {
    let srv = serve(vec![
        Reply::json(429, r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#)
            .header("retry-after", "0"),
        Reply::sse(ANTHROPIC_OK),
    ])
    .await;
    let p = AnthropicProvider::new(&srv.url, "m", "k".into()).unwrap();
    let (out, evs) = collect(&p, &req("hola"), &fast_limits()).await;
    assert_eq!(out.unwrap().text, "Hola, Ana");
    assert!(evs.iter().any(|e| matches!(e, StreamEvent::Retrying { attempt: 1, wait } if wait.is_zero())));
    assert_eq!(srv.requests().len(), 2);
}

#[tokio::test]
async fn anthropic_429_gives_up_after_max_retries() {
    let srv = serve(vec![Reply::json(429, "{}").header("retry-after", "0")]).await;
    let p = AnthropicProvider::new(&srv.url, "m", "k".into()).unwrap();
    let limits = Limits { max_retries: 2, ..fast_limits() };
    let (out, _) = collect(&p, &req("x"), &limits).await;
    assert!(matches!(out, Err(AiError::RateLimited { .. })));
    assert_eq!(srv.requests().len(), 3);
}

#[tokio::test]
async fn anthropic_errors_auth_stream_error_and_cut_stream() {
    let srv = serve(vec![Reply::json(
        401,
        r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#,
    )])
    .await;
    let p = AnthropicProvider::new(&srv.url, "m", "bad".into()).unwrap();
    let (out, _) = collect(&p, &req("x"), &fast_limits()).await;
    assert!(matches!(out, Err(AiError::Auth(ref m)) if m.contains("invalid x-api-key")), "{out:?}");

    let srv = serve(vec![Reply::sse(&[
        "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"a\"}}\n\n",
        "data: {\"type\":\"error\",\"error\":{\"type\":\"api_error\",\"message\":\"boom\"}}\n\n",
    ])])
    .await;
    let p = AnthropicProvider::new(&srv.url, "m", "k".into()).unwrap();
    let (out, _) = collect(&p, &req("x"), &fast_limits()).await;
    assert!(matches!(out, Err(AiError::Http { ref message, .. }) if message == "boom"), "{out:?}");

    let srv = serve(vec![Reply::sse(&[
        "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"a\"}}\n\n",
    ])])
    .await;
    let p = AnthropicProvider::new(&srv.url, "m", "k".into()).unwrap();
    let (out, _) = collect(&p, &req("x"), &fast_limits()).await;
    assert!(matches!(out, Err(AiError::Protocol(_))), "{out:?}");
}

#[tokio::test]
async fn response_size_limit_is_enforced() {
    let srv = serve(vec![Reply::sse(ANTHROPIC_OK)]).await;
    let p = AnthropicProvider::new(&srv.url, "m", "k".into()).unwrap();
    let limits = Limits { max_response_bytes: 5, ..fast_limits() };
    let (out, _) = collect(&p, &req("x"), &limits).await;
    assert!(matches!(out, Err(AiError::TooLarge(_))), "{out:?}");
    let limits = Limits { max_input_chars: 3, ..fast_limits() };
    let (out, _) = collect(&p, &req("demasiado"), &limits).await;
    assert!(matches!(out, Err(AiError::TooLarge(_))));
}

#[tokio::test]
async fn cancellation_and_timeout() {
    let srv = serve(vec![Reply::sse(ANTHROPIC_OK).delayed(Duration::from_secs(10))]).await;
    let p = AnthropicProvider::new(&srv.url, "m", "k".into()).unwrap();
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        c2.cancel();
    });
    let t0 = std::time::Instant::now();
    let out = run_chat(&p, &req("x"), &fast_limits(), None, &cancel).await;
    assert!(matches!(out, Err(AiError::Cancelled)));
    assert!(t0.elapsed() < Duration::from_secs(5));

    let limits = Limits { timeout: Duration::from_millis(200), ..fast_limits() };
    let out = run_chat(&p, &req("x"), &limits, None, &CancellationToken::new()).await;
    assert!(matches!(out, Err(AiError::Timeout)));
}

#[tokio::test]
async fn openai_compatible_streams_and_sends_bearer_only_with_key() {
    let chunks = [
        "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"Buenos\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\" días\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":2}}\n\n",
        "data: [DONE]\n\n",
    ];
    let srv = serve(vec![Reply::sse(&chunks)]).await;
    let p = OpenAiProvider::new(&format!("{}/v1/", srv.url), "llama3", Some("sk-or".into())).unwrap();
    let (out, evs) = collect(&p, &req("hola"), &fast_limits()).await;
    let c = out.unwrap();
    assert_eq!(c.text, "Buenos días");
    assert_eq!(deltas(&evs), ["Buenos", " días"]);
    assert_eq!(c.stop_reason.as_deref(), Some("stop"));
    assert_eq!(c.usage.unwrap().input_tokens, 9);
    let r = &srv.requests()[0];
    assert_eq!(r.path, "/v1/chat/completions");
    assert_eq!(r.header("authorization"), Some("Bearer sk-or"));
    let b = r.json();
    assert_eq!(b["messages"][0]["role"], "system");
    assert_eq!(b["messages"][1]["content"], "hola");
    assert_eq!(b["model"], "llama3");
    // Local (Ollama) y sin clave: sin cabecera y no externo.
    assert!(!p.is_external());

    let srv = serve(vec![Reply::sse(&chunks)]).await;
    let p = OpenAiProvider::new(&format!("{}/v1", srv.url), "llama3", Some("  ".into())).unwrap();
    collect(&p, &req("hola"), &fast_limits()).await.0.unwrap();
    assert_eq!(srv.requests()[0].header("authorization"), None);
}

#[tokio::test]
async fn openai_503_retry_and_in_stream_error() {
    let ok =
        ["data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\n", "data: [DONE]\n\n"];
    let srv = serve(vec![Reply::json(503, r#"{"error":{"message":"busy"}}"#), Reply::sse(&ok)]).await;
    let p = OpenAiProvider::new(&format!("{}/v1", srv.url), "m", None).unwrap();
    assert_eq!(collect(&p, &req("x"), &fast_limits()).await.0.unwrap().text, "ok");

    let srv = serve(vec![Reply::sse(&["data: {\"error\":{\"message\":\"model not found\",\"code\":404}}\n\n"])]).await;
    let p = OpenAiProvider::new(&format!("{}/v1", srv.url), "m", None).unwrap();
    let (out, _) = collect(&p, &req("x"), &fast_limits()).await;
    assert!(matches!(out, Err(AiError::Http { status: 404, .. })), "{out:?}");

    let srv = serve(vec![Reply::json(
        429,
        r#"{"error":{"message":"You exceeded your current quota","code":"insufficient_quota"}}"#,
    )])
    .await;
    let p = OpenAiProvider::new(&format!("{}/v1", srv.url), "m", None).unwrap();
    let (out, _) = collect(&p, &req("x"), &fast_limits()).await;
    assert!(matches!(out, Err(AiError::Quota(_)) | Err(AiError::RateLimited { .. })), "{out:?}");
}

fn email() -> EmailInput {
    EmailInput {
        id: "m1".into(),
        from: Addr::new(Some("Ana Ruiz"), "ana@cliente.mx"),
        to: vec![Addr::new(None, "yo@despacho.mx")],
        subject: "Contrato".into(),
        body_text: "Hola, ¿me mandas el contrato el jueves? Mi cel: 55 1234 5678".into(),
        ..Default::default()
    }
}

fn opts() -> TaskOptions {
    TaskOptions { today: NaiveDate::from_ymd_opt(2026, 10, 26).unwrap(), ..Default::default() }
}

fn openai_text_stream(text: &str) -> Reply {
    let piece = serde_json::json!({"choices": [{"delta": {"content": text}, "finish_reason": "stop"}]});
    let owned = [format!("data: {piece}\n\n"), "data: [DONE]\n\n".to_string()];
    let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
    Reply::sse(&refs)
}

#[tokio::test]
async fn task_end_to_end_repairs_json_and_restores_redaction() {
    // El modelo contesta en bloque de código, con coma final y con las marcas de privacidad.
    let model_out = "Aquí está:\n```json\n{\"summary\": \"Ana ([email-1], [tel-1]) pide el contrato.\", \"key_points\": [\"jueves\",],}\n```";
    let srv = serve(vec![openai_text_stream(model_out)]).await;
    // Proveedor «externo» simulado: forzamos la redacción.
    let p = OpenAiProvider::new(&format!("{}/v1", srv.url), "m", None).unwrap();
    let mut o = opts();
    o.scope.redact = Some(true);
    let prepared = tasks::prepare_summarize_thread(&Target::of(&p), &[email()], &o);
    assert!(prepared.scope.redacted);
    let proposal = tasks::run(&p, prepared, &fast_limits(), None, &CancellationToken::new()).await.unwrap();
    assert!(proposal.repaired);
    assert_eq!(proposal.value.summary, "Ana (ana@cliente.mx, 55 1234 5678) pide el contrato.");
    assert_eq!(proposal.value.key_points, ["jueves"]);
    // Lo que salió no llevaba los datos.
    let sent = srv.requests()[0].body.clone();
    assert!(!sent.contains("ana@cliente.mx") && !sent.contains("1234 5678"), "{sent}");
}

#[tokio::test]
async fn task_rejects_unrepairable_output() {
    let srv = serve(vec![openai_text_stream("Lo siento, no puedo ayudar con eso.")]).await;
    let p = OpenAiProvider::new(&format!("{}/v1", srv.url), "m", None).unwrap();
    let prepared = tasks::prepare_extract(&Target::of(&p), &email(), &opts());
    let r = tasks::run(&p, prepared, &fast_limits(), None, &CancellationToken::new()).await;
    assert!(matches!(r, Err(AiError::InvalidOutput(_))), "{r:?}");
}

fn session(url: &str) -> Arc<Session> {
    let acc = iurefficient_connect::Account::new(url, "ana@despacho.mx").unwrap();
    Arc::new(Session::new(acc, "IureDesk-test").unwrap())
}

#[tokio::test]
async fn iurefficient_global_and_case_chat() {
    let srv = serve(vec![
        Reply::json(200, r#"{"assistant_message":{"content":" {\"text\": \"Listo\"} "}}"#),
        Reply::json(200, r#"{"assistant_message":{"content":"Del caso"}}"#),
    ])
    .await;
    let p = IurefficientProvider::new(session(&srv.url));
    assert!(!p.is_external());
    let (out, evs) = collect(&p, &req("hola"), &fast_limits()).await;
    assert_eq!(out.unwrap().text, "{\"text\": \"Listo\"}");
    assert_eq!(deltas(&evs).len(), 1);
    let mut r = req("resume");
    r.case_id = Some("c-42".into());
    assert_eq!(collect(&p, &r, &fast_limits()).await.0.unwrap().text, "Del caso");
    let reqs = srv.requests();
    assert_eq!(reqs[0].path, "/api/chat/global/chat");
    assert_eq!(reqs[0].json()["system_prompt"], "Eres útil.");
    assert_eq!(reqs[0].json()["message"], "hola");
    assert_eq!(reqs[1].path, "/api/chat/cases/c-42/chat");
    assert!(reqs[1].json()["message"].as_str().unwrap().starts_with("Eres útil."));

    let mut bad = req("x");
    bad.case_id = Some("../admin".into());
    assert!(collect(&p, &bad, &fast_limits()).await.0.is_err());
}

#[tokio::test]
async fn iurefficient_quota_terms_and_rate_limit() {
    let srv = serve(vec![
        Reply::json(403, r#"{"error":"quota_exceeded"}"#),
        Reply::json(403, r#"{"code":"terms_acceptance_required","message":"terms_acceptance_required"}"#),
        Reply::json(429, r#"{"error":"slow"}"#),
        Reply::json(200, r#"{"assistant_message":{"content":"ok"}}"#),
    ])
    .await;
    let p = IurefficientProvider::new(session(&srv.url));
    let limits = fast_limits();
    assert!(matches!(collect(&p, &req("x"), &limits).await.0, Err(AiError::Quota(_))));
    assert!(matches!(collect(&p, &req("x"), &limits).await.0, Err(AiError::NotAvailable(_))));
    assert_eq!(collect(&p, &req("x"), &limits).await.0.unwrap().text, "ok");
}

struct FakeTokens(AtomicU32);

impl TokenProvider for FakeTokens {
    fn access_token<'a>(
        &'a self,
        _account: &'a Account,
        aud: TokenAudience,
    ) -> BoxFuture<'a, Result<String, iuredesk_oauth::Error>> {
        assert_eq!(aud, TokenAudience::Api);
        let n = self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok(format!("at-{n}")) })
    }

    fn invalidate(&self, _account_id: &str) {}
}

fn m365() -> Account {
    let srv = |p| ServerConfig {
        protocol: p,
        host: "outlook.office365.com".into(),
        port: 993,
        security: Security::Tls,
        auth: AuthMethod::OAuth2 { provider: OAuthProvider::Microsoft },
        username: "ana@contoso.com".into(),
    };
    Account {
        id: "ana-m365".into(),
        display_name: "Ana".into(),
        email: "ana@contoso.com".into(),
        kind: AccountKind::Microsoft365,
        incoming: srv(Protocol::Imap),
        outgoing: srv(Protocol::Smtp),
        caldav_url: None,
        carddav_url: None,
        identities: vec![],
    }
}

#[tokio::test]
async fn copilot_probe_and_chat() {
    let conv = r#"{"id":"0d110e7e-2b7e-4270-a899-fd2af6fde333","state":"active","turnCount":0}"#;
    let answer = r##"{"id":"0d110e7e-2b7e-4270-a899-fd2af6fde333","messages":[
        {"@odata.type":"#microsoft.graph.copilotConversationResponseMessage","id":"1","text":"PREGUNTA"},
        {"@odata.type":"#microsoft.graph.copilotConversationResponseMessage","id":"2","text":"Tienes una reunión a las 9.","attributions":[]}]}"##;
    let srv = serve(vec![
        Reply::json(201, conv),
        Reply::json(201, conv),
        Reply::json(200, answer),
        Reply::json(403, r#"{"error":{"code":"Forbidden","message":"Copilot license required"}}"#),
    ])
    .await;
    let tokens: Arc<dyn TokenProvider> = Arc::new(FakeTokens(AtomicU32::new(0)));
    let p = CopilotProvider::new(m365(), tokens, &format!("{}/beta", srv.url), "America/Mexico_City").unwrap();
    assert_eq!(p.probe().await, CopilotCapability::Available);
    let (out, _) = collect(&p, &req("¿Qué reunión tengo?"), &fast_limits()).await;
    assert_eq!(out.unwrap().text, "Tienes una reunión a las 9.");
    assert!(matches!(p.probe().await, CopilotCapability::Unavailable(m) if m.contains("license")));
    let reqs = srv.requests();
    assert_eq!(reqs[0].path, "/beta/copilot/conversations");
    assert!(reqs[0].header("authorization").unwrap().starts_with("Bearer at-"));
    assert_eq!(reqs[2].path, "/beta/copilot/conversations/0d110e7e-2b7e-4270-a899-fd2af6fde333/chat");
    let b = reqs[2].json();
    assert_eq!(b["locationHint"]["timeZone"], "America/Mexico_City");
    assert_eq!(b["contextualResources"]["webContext"]["isWebEnabled"], false);
    assert!(b["message"]["text"].as_str().unwrap().contains("¿Qué reunión tengo?"));
}

#[tokio::test]
async fn copilot_without_license_is_not_available() {
    let srv = serve(vec![Reply::json(403, r#"{"error":{"message":"Forbidden"}}"#)]).await;
    let tokens: Arc<dyn TokenProvider> = Arc::new(FakeTokens(AtomicU32::new(0)));
    let p = CopilotProvider::new(m365(), tokens, &srv.url, "").unwrap();
    let (out, _) = collect(&p, &req("x"), &fast_limits()).await;
    assert!(matches!(out, Err(AiError::NotAvailable(_))), "{out:?}");
}

#[tokio::test]
async fn approved_actions_call_iurefficient() {
    let srv = serve(vec![
        Reply::json(200, r#"{"plugins":[{"id":"iur-tickets"},{"id":"iur-collab"}]}"#),
        Reply::json(201, r#"{"ticket":{"id":57,"ticket_number":"T-57"}}"#),
        Reply::json(200, r#"{"plugins":[{"id":"iur-tickets"}]}"#),
    ])
    .await;
    let sess = session(&srv.url);
    let ticket = TicketPayload {
        title: "No funciona la factura".into(),
        description: Some("Error al timbrar".into()),
        priority: TicketPriority::High,
        category: None,
        requester_name: Some("Ana".into()),
        requester_email: Some("ana@cliente.mx".into()),
        tags: vec![],
    };
    let a = ProposedAction::CreateTicket { ticket, notify_requester: false };
    let out = actions::execute(Some(&sess), a.approve(), chrono_tz::UTC).await.unwrap();
    assert_eq!((out.entity.as_str(), out.id.as_str()), ("ticket", "57"));
    let reqs = srv.requests();
    assert_eq!(reqs[0].path, "/api/plugins");
    assert_eq!(reqs[1].path, "/api/plugins/tickets");
    let b = reqs[1].json();
    assert_eq!(b["priority"], "high");
    assert!(b.get("requester_email").is_none());

    // Sin el plugin del CRM: no disponible y no se crea nada.
    let lead = ProposedAction::CreateLead { lead: LeadPayload { name: "Ana".into(), ..Default::default() } };
    let r = actions::execute(Some(&sess), lead.approve(), chrono_tz::UTC).await;
    assert!(matches!(r, Err(AiError::NotAvailable(_))), "{r:?}");
    assert_eq!(srv.requests().len(), 3);
}
