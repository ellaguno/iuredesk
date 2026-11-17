//! S1.4: grabar una reunión con IureTranscribe sin abrir su ventana.
//!
//! `iuretranscribe --mcp` tiene `start_recording`, `recording_status`, `pause_recording` y
//! `stop_recording`. La grabación vive en ese proceso: [`Recording`] lo mantiene abierto (un
//! [`McpClient`]) desde que empieza hasta que se detiene; si se suelta sin detener, el proceso
//! hijo muere y la grabación queda como esté en disco.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};

use crate::error::Result;
use crate::mcp_client::McpClient;
use crate::suite::{Outcome, SisterApp, Suite};

/// Tiempo máximo de cada llamada (detener puede tardar: cierra y mezcla las pistas).
const TIMEOUT: Duration = Duration::from_secs(60);

/// Una grabación en curso.
pub struct Recording {
    client: McpClient,
    /// Archivo donde se graba.
    pub path: PathBuf,
}

/// Cómo va la grabación.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecordingStatus {
    pub active: bool,
    /// Segundos grabados (sin las pausas).
    pub elapsed_secs: f64,
    pub paused: bool,
    /// Nivel del micrófono y del sistema (0–1).
    pub mic_level: f32,
    pub sys_level: f32,
    /// Lo que salió mal sin detener la grabación.
    pub error: Option<String>,
}

/// Grabación terminada.
#[derive(Clone, Debug, PartialEq)]
pub struct Recorded {
    pub path: PathBuf,
    pub duration_secs: f64,
    pub warning: Option<String>,
}

fn f64_of(v: &Value, k: &str) -> f64 {
    v.get(k).and_then(Value::as_f64).filter(|x| x.is_finite()).unwrap_or(0.0)
}

impl Suite {
    /// Empieza a grabar con los ajustes de IureTranscribe. La grabación sigue mientras viva el
    /// [`Recording`].
    pub fn start_recording(&self) -> Result<Outcome<Recording>> {
        let mut client = match self.connect_mcp(SisterApp::Transcribe)? {
            Outcome::Ready(c) => c,
            Outcome::NotInstalled(h) => return Ok(Outcome::NotInstalled(h)),
            Outcome::NotAvailable(t) => return Ok(Outcome::NotAvailable(t)),
        };
        let data = client.call_tool("start_recording", json!({}), Some(TIMEOUT))?.into_ok()?.data().unwrap_or_default();
        let path = data.get("path").and_then(Value::as_str).map(PathBuf::from).unwrap_or_default();
        Ok(Outcome::Ready(Recording { client, path }))
    }
}

impl Recording {
    /// Cómo va (acotado: niveles entre 0 y 1).
    pub fn status(&mut self) -> Result<RecordingStatus> {
        let v =
            self.client.call_tool("recording_status", json!({}), Some(TIMEOUT))?.into_ok()?.data().unwrap_or_default();
        let level = |k: &str| (f64_of(&v, k) as f32).clamp(0.0, 1.0);
        Ok(RecordingStatus {
            active: v.get("active").and_then(Value::as_bool).unwrap_or(false),
            elapsed_secs: f64_of(&v, "elapsedSecs").max(0.0),
            paused: v.get("paused").and_then(Value::as_bool).unwrap_or(false),
            mic_level: level("micLevel"),
            sys_level: level("sysLevel"),
            error: v.get("error").and_then(Value::as_str).map(|e| e.chars().take(300).collect()),
        })
    }

    /// Pausa o reanuda.
    pub fn set_paused(&mut self, paused: bool) -> Result<()> {
        self.client.call_tool("pause_recording", json!({ "paused": paused }), Some(TIMEOUT))?.into_ok()?;
        Ok(())
    }

    /// Detiene y guarda; consume la grabación (el proceso se cierra después).
    pub fn stop(mut self) -> Result<Recorded> {
        let v =
            self.client.call_tool("stop_recording", json!({}), Some(TIMEOUT))?.into_ok()?.data().unwrap_or_default();
        Ok(Recorded {
            path: v.get("path").and_then(Value::as_str).map(PathBuf::from).unwrap_or(self.path),
            duration_secs: f64_of(&v, "durationSecs").max(0.0),
            warning: v.get("warning").and_then(Value::as_str).map(|e| e.chars().take(300).collect()),
        })
    }
}
