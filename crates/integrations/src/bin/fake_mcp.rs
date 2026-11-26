//! Servidor MCP falso para las pruebas de `iuredesk-integrations` (no se distribuye).
//!
//! Usa `iurefficient_connect::mcp_server`, el mismo servidor que llevan IureTranscribe, IureOCR
//! e IureVoz, con herramientas que imitan las suyas. `FAKE_MCP_MODE` cambia su conducta:
//! - `silent`: lee stdin y nunca responde (prueba de tiempos de espera);
//! - `garbage`: escribe basura en stdout antes de servir (el cliente debe ignorarla);
//! - `exit`: termina sin responder;
//! - `no-recording`: sin las herramientas de grabación (como IureTranscribe 0.8.5).
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

use std::io::{BufRead, Write};
use std::sync::Mutex;
use std::time::Duration;

use iurefficient_connect::mcp_server::{Call, CallError, Handler, Reply, ServerInfo, serve};
use serde_json::{Value, json};

struct Fake {
    status_calls: Mutex<u32>,
    /// Plan abierto con `file_open` (como IureProject).
    plan: Mutex<String>,
    /// Tareas agregadas y vínculos del plan abierto (se escriben junto al plan con `file_save`).
    added: Mutex<Vec<Value>>,
    /// Grabación en curso (como IureTranscribe): en pausa o no.
    recording: Mutex<Option<bool>>,
}

fn s<'a>(a: &'a Value, k: &str) -> &'a str {
    a.get(k).and_then(Value::as_str).unwrap_or("")
}

impl Handler for Fake {
    fn info(&self) -> ServerInfo {
        ServerInfo { name: "fake", title: "Fake", version: "0.0.1", instructions: "Servidor de pruebas.".into() }
    }

    fn tools(&self) -> Value {
        let names = [
            "echo",
            "sleep",
            "fail",
            "transcribe_file",
            "transcription_status",
            "read_transcript",
            "ocr_file",
            "decir",
            "preguntar_si_no",
            "dictar",
            "file_open",
            "project_overview",
            "task_add_list",
            "task_set_hyperlink",
            "file_save",
            "start_recording",
            "recording_status",
            "pause_recording",
            "stop_recording",
        ];
        let old = std::env::var("FAKE_MCP_MODE").as_deref() == Ok("no-recording");
        Value::Array(
            names
                .iter()
                .filter(|n| !(old && n.contains("recording")))
                .map(|n| json!({"name": n, "description": format!("tool {n}"), "inputSchema": {"type": "object"}}))
                .collect(),
        )
    }

    fn call(&self, name: &str, a: &Value, call: &Call) -> Result<Reply, CallError> {
        match name {
            "echo" => Ok(Reply::Data(json!({"echo": a}))),
            "sleep" => {
                let ms = a.get("ms").and_then(Value::as_u64).unwrap_or(10_000);
                let mut left = ms;
                while left > 0 && !call.cancelled() {
                    std::thread::sleep(Duration::from_millis(left.min(20)));
                    left = left.saturating_sub(20);
                }
                Ok(Reply::Data(json!({"slept": ms})))
            }
            "fail" => Err(CallError::Tool(anyhow::anyhow!("falló a propósito"))),
            "transcribe_file" => Ok(Reply::Data(json!({
                "status": "running", "jobId": "job-1", "percent": 10, "stage": "decoding", "input": s(a, "path"),
            }))),
            "transcription_status" => {
                let mut n = self.status_calls.lock().unwrap_or_else(|e| e.into_inner());
                *n += 1;
                if s(a, "job_id") != "job-1" {
                    return Err(CallError::Tool(anyhow::anyhow!("No hay transcripción con id")));
                }
                if *n == 1 {
                    return Ok(Reply::Data(
                        json!({"status": "running", "jobId": "job-1", "percent": 55, "stage": "transcribing"}),
                    ));
                }
                let offset = a.get("offset").and_then(Value::as_u64).unwrap_or(0);
                let text = if offset == 0 { "[00:00:01] Hola" } else { " mundo" };
                Ok(Reply::Data(json!({
                    "status": "done", "jobId": "job-1", "audioSecs": 2.5, "detectedLanguage": "es",
                    "outputs": [{"format": "txt", "path": "/tmp/rec.txt"}],
                    "text": text, "totalChars": 21, "offset": offset,
                    "nextOffset": if offset == 0 { json!(15) } else { Value::Null },
                })))
            }
            "read_transcript" => Ok(Reply::Data(json!({"path": s(a, "path"), "text": "minuta", "totalChars": 6}))),
            "ocr_file" => Ok(Reply::Data(json!({
                "skipped": false, "pdfPath": format!("{}-ocr.pdf", s(a, "path")), "txtPath": format!("{}-ocr.txt", s(a, "path")),
                "pages": 2, "chars": 9, "elapsedSecs": 0.1, "note": null, "text": "TEXTO OCR", "textTruncated": false,
                "languages": a.get("languages").cloned().unwrap_or(Value::Null),
            }))),
            // Como IureVoz cuando la app no está abierta.
            "decir" | "preguntar_si_no" | "dictar" => {
                Err(CallError::Tool(anyhow::anyhow!("IureVoz no está abierto: ábrelo para poder hablar y escuchar")))
            }
            // Como IureProject: abrir un plan y su resumen.
            "file_open" => {
                let path = s(a, "path");
                if path.contains("roto") {
                    return Err(CallError::Tool(anyhow::anyhow!("no es un plan válido")));
                }
                *self.plan.lock().unwrap_or_else(|e| e.into_inner()) = path.to_string();
                self.added.lock().unwrap_or_else(|e| e.into_inner()).clear();
                Ok(Reply::Data(json!({"ok": true})))
            }
            "task_add_list" => {
                let items = a.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
                let mut added = self.added.lock().unwrap_or_else(|e| e.into_inner());
                let uids: Vec<u64> = (0..items.len() as u64).map(|i| 40 + i).collect();
                added.extend(items);
                // Como IureProject: el JSON en texto y una línea de resumen (sin structuredContent).
                let text = format!("{}\n\n{} tareas; fin 2026-11-23 17:00", json!({"uids": uids}), added.len());
                Ok(Reply::Raw(json!({"content": [{"type": "text", "text": text}], "isError": false})))
            }
            "task_set_hyperlink" => {
                self.added.lock().unwrap_or_else(|e| e.into_inner()).push(json!({"hyperlink": a}));
                Ok(Reply::Data(json!({"ok": true})))
            }
            // Como IureProject cuando su ventana guardó el plan entre tanto.
            "file_save" => {
                let plan = self.plan.lock().unwrap_or_else(|e| e.into_inner()).clone();
                if plan.contains("ocupado") {
                    return Err(CallError::Tool(anyhow::anyhow!(
                        "otro programa cambió {plan} desde que se abrió; vuelve a abrirlo (o guarda con force)"
                    )));
                }
                let added = self.added.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let _ = std::fs::write(format!("{plan}.agregado.json"), Value::Array(added).to_string());
                Ok(Reply::Data(json!({"ok": true})))
            }
            "project_overview" => {
                let plan = self.plan.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let name = std::path::Path::new(&plan)
                    .file_stem()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                Ok(Reply::Data(json!({
                    "case": {"id": "c-7", "number": "LAB-2026-0012", "title": "Despido injustificado", "client": "Ramírez"},
                    "name": name, "as_of": "2026-11-17", "finish": "2027-01-29T17:00:00", "percent_complete": 42.5,
                    "tasks": 12, "done": 4, "in_progress": 3, "not_started": 5, "late": 1, "critical": 2,
                    "late_tasks": [{"uid": 7, "name": "Peritaje", "finish": "2026-11-10T17:00:00", "percent_complete": 30, "critical": true}],
                    "upcoming_milestones": [{"uid": 9, "name": "Audiencia", "finish": "2026-11-30T17:00:00", "percent_complete": 0, "critical": true}],
                    "resources": []
                })))
            }
            // Como IureTranscribe: una grabación a la vez.
            n if n.contains("recording") && std::env::var("FAKE_MCP_MODE").as_deref() == Ok("no-recording") => {
                Err(CallError::Params(format!("unknown tool: {n}")))
            }
            "start_recording" => {
                let mut r = self.recording.lock().unwrap_or_else(|e| e.into_inner());
                if r.is_some() {
                    return Err(CallError::Tool(anyhow::anyhow!("Ya hay una grabación en curso.")));
                }
                *r = Some(false);
                Ok(Reply::Data(json!({"path": "/tmp/grabaciones/reunion.wav", "active": true})))
            }
            "recording_status" => {
                let r = *self.recording.lock().unwrap_or_else(|e| e.into_inner());
                Ok(Reply::Data(json!({
                    "active": r.is_some(), "elapsedSecs": 12.5, "paused": r == Some(true),
                    "micLevel": 3.0, "sysLevel": 0.2, "path": "/tmp/grabaciones/reunion.wav", "error": null,
                })))
            }
            "pause_recording" => {
                let mut r = self.recording.lock().unwrap_or_else(|e| e.into_inner());
                if r.is_none() {
                    return Err(CallError::Tool(anyhow::anyhow!("No se está grabando.")));
                }
                *r = Some(a.get("paused").and_then(Value::as_bool).unwrap_or(true));
                Ok(Reply::Data(json!({"paused": *r == Some(true)})))
            }
            "stop_recording" => {
                if self.recording.lock().unwrap_or_else(|e| e.into_inner()).take().is_none() {
                    return Err(CallError::Tool(anyhow::anyhow!("No se está grabando.")));
                }
                Ok(Reply::Data(json!({"path": "/tmp/grabaciones/reunion.wav", "durationSecs": 12.5, "warning": null})))
            }
            other => Err(CallError::Params(format!("herramienta desconocida: {other}"))),
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if !args.iter().any(|a| a == "--mcp") {
        eprintln!("uso: iuredesk-fake-mcp --mcp");
        std::process::exit(2);
    }
    match std::env::var("FAKE_MCP_MODE").as_deref() {
        Ok("silent") => {
            for line in std::io::stdin().lock().lines() {
                if line.is_err() {
                    break;
                }
            }
            return;
        }
        Ok("exit") => std::process::exit(3),
        Ok("garbage") => {
            let mut out = std::io::stdout();
            let _ = writeln!(out, "esto no es JSON");
            let _ = writeln!(out, "[1,2,3]");
            let _ = out.flush();
        }
        _ => {}
    }
    std::process::exit(serve(Fake {
        status_calls: Mutex::new(0),
        plan: Mutex::new(String::new()),
        added: Mutex::new(Vec::new()),
        recording: Mutex::new(None),
    }));
}
