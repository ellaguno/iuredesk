//! Prueba manual del asistente con un correo `.eml` por la entrada estándar.
//!
//! ```text
//! ANTHROPIC_API_KEY=… cargo run -p iuredesk-assistant --example ask -- --provider anthropic --task summarize < correo.eml
//! OPENAI_BASE_URL=http://localhost:11434/v1 OPENAI_MODEL=llama3.2 \
//!     cargo run -p iuredesk-assistant --example ask -- --provider openai --task extract --lang en < correo.eml
//! cargo run -p iuredesk-assistant --example ask -- --provider iurefficient --task draft < correo.eml
//! ```
//!
//! Proveedores: `anthropic` (`ANTHROPIC_API_KEY`, `ANTHROPIC_MODEL`), `openai` (`OPENAI_BASE_URL`,
//! `OPENAI_MODEL`, `OPENAI_API_KEY` opcional), `iurefficient` (sesión compartida de la suite).
//! Tareas: `summarize`, `draft`, `rewrite`, `extract`, `classify`, `outline`, `minutes`, `triage`.
//! Opciones: `--lang es|en`, `--redact`, `--dry-run` (sólo muestra qué se enviaría).
//!
//! Muestra antes qué se enviará (DataScope); ejecutar el ejemplo es la llamada explícita. El texto
//! llega por stderr mientras se genera y el resultado validado sale en JSON por stdout.

use std::io::Read;
use std::sync::Arc;

use iuredesk_assistant::config::DEFAULT_ANTHROPIC_MODEL;
use iuredesk_assistant::providers::anthropic::{AnthropicProvider, DEFAULT_BASE};
use iuredesk_assistant::providers::iurefficient::IurefficientProvider;
use iuredesk_assistant::providers::openai::OpenAiProvider;
use iuredesk_assistant::schema::TaskOutput;
use iuredesk_assistant::tasks::{self, Prepared, Target, TaskOptions};
use iuredesk_assistant::{Addr, AiProvider, CancellationToken, EmailInput, Lang, Length, Limits, StreamEvent, Tone};
use mail_parser::MimeHeaders;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn addrs(a: Option<&mail_parser::Address<'_>>) -> Vec<Addr> {
    a.map(|a| a.iter().map(|x| Addr::new(x.name(), x.address().unwrap_or_default())).collect()).unwrap_or_default()
}

fn parse_eml(raw: &[u8]) -> Result<EmailInput, String> {
    let m = mail_parser::MessageParser::default().parse(raw).ok_or("no es un correo RFC 5322")?;
    Ok(EmailInput {
        id: m.message_id().unwrap_or("stdin").to_string(),
        from: addrs(m.from()).into_iter().next().unwrap_or_default(),
        to: addrs(m.to()),
        cc: addrs(m.cc()),
        subject: m.subject().unwrap_or_default().to_string(),
        date: m.date().map(|d| d.to_rfc3339()),
        body_text: m.body_text(0).map(|b| b.to_string()).unwrap_or_default(),
        attachments: m.attachments().filter_map(|a| a.attachment_name().map(str::to_string)).collect(),
        unread: true,
    })
}

async fn provider(name: &str) -> Result<Box<dyn AiProvider>, String> {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    match name {
        "anthropic" => {
            let key = env("ANTHROPIC_API_KEY").ok_or("falta ANTHROPIC_API_KEY")?;
            let model = env("ANTHROPIC_MODEL").unwrap_or_else(|| DEFAULT_ANTHROPIC_MODEL.into());
            let base = env("ANTHROPIC_BASE_URL").unwrap_or_else(|| DEFAULT_BASE.into());
            Ok(Box::new(AnthropicProvider::new(&base, &model, key).map_err(|e| e.to_string())?))
        }
        "openai" => {
            let base = env("OPENAI_BASE_URL").unwrap_or_else(|| "https://api.openai.com/v1".into());
            let model = env("OPENAI_MODEL").ok_or("falta OPENAI_MODEL")?;
            Ok(Box::new(OpenAiProvider::new(&base, &model, env("OPENAI_API_KEY")).map_err(|e| e.to_string())?))
        }
        "iurefficient" => {
            let shared = iuredesk_accounts::iurefficient_session().await;
            let sess = shared.session.ok_or_else(|| format!("sin sesión de Iurefficient: {:?}", shared.status))?;
            Ok(Box::new(IurefficientProvider::new(Arc::new(sess))))
        }
        other => Err(format!("proveedor desconocido: {other}")),
    }
}

async fn go<T: TaskOutput>(p: &dyn AiProvider, prepared: Prepared<T>, dry_run: bool) -> Result<(), String> {
    eprintln!("── Se enviará a {} ──", prepared.scope.provider);
    eprintln!("{}", prepared.scope.summary(true));
    eprintln!("{}\n──", prepared.scope.payload);
    if dry_run {
        return Ok(());
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let printer = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            match ev {
                StreamEvent::Delta(d) => eprint!("{d}"),
                StreamEvent::Retrying { attempt, wait } => eprintln!("\n[reintento {attempt} en {wait:?}]"),
            }
        }
    });
    let cancel = CancellationToken::new();
    let r = tasks::run(p, prepared, &Limits::default(), Some(&tx), &cancel).await;
    drop(tx);
    printer.await.ok();
    eprintln!();
    let proposal = r.map_err(|e| format!("{e} — {}", e.human(true)))?;
    if proposal.repaired {
        eprintln!("(JSON reparado)");
    }
    println!("{}", serde_json::to_string_pretty(&proposal.value).map_err(|e| e.to_string())?);
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(e) = real_main().await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

async fn real_main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let pname = arg(&args, "--provider").unwrap_or_else(|| "anthropic".into());
    let task = arg(&args, "--task").unwrap_or_else(|| "summarize".into());
    let lang = if arg(&args, "--lang").as_deref() == Some("en") { Lang::En } else { Lang::Es };
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let mut raw = Vec::new();
    std::io::stdin().read_to_end(&mut raw).map_err(|e| e.to_string())?;
    let p = provider(&pname).await?;
    let target = Target::of(p.as_ref());
    let mut opts = TaskOptions { lang, ..Default::default() };
    if args.iter().any(|a| a == "--redact") {
        opts.scope.redact = Some(true);
    }
    let p = p.as_ref();
    match task.as_str() {
        "rewrite" => {
            let text = String::from_utf8_lossy(&raw).to_string();
            go(p, tasks::prepare_rewrite(&target, &text, Tone::Formal, Length::Same, lang, &opts), dry_run).await
        }
        "minutes" => {
            let text = String::from_utf8_lossy(&raw).to_string();
            go(p, tasks::prepare_meeting_minutes(&target, &text, None, &opts), dry_run).await
        }
        _ => {
            let email = parse_eml(&raw)?;
            let one = std::slice::from_ref(&email);
            match task.as_str() {
                "summarize" => go(p, tasks::prepare_summarize_thread(&target, one, &opts), dry_run).await,
                "draft" => {
                    go(p, tasks::prepare_draft_reply(&target, one, Tone::Neutral, lang, "", &opts), dry_run).await
                }
                "extract" => go(p, tasks::prepare_extract(&target, &email, &opts), dry_run).await,
                "classify" => go(p, tasks::prepare_classify_lead_or_ticket(&target, &email, &opts), dry_run).await,
                "outline" => go(p, tasks::prepare_document_outline(&target, one, "", &opts), dry_run).await,
                "triage" => go(p, tasks::prepare_triage_today(&target, one, &opts), dry_run).await,
                other => Err(format!("tarea desconocida: {other}")),
            }
        }
    }
}
