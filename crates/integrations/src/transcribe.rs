//! Transcribir una grabación con IureTranscribe (`iuretranscribe --mcp`).
//!
//! `transcribe_file` responde en seguida si el audio es corto; si no, devuelve un `jobId` y el
//! trabajo sigue **dentro de ese proceso**: los ids se pierden si el proceso muere. Por eso
//! [`TranscriptionJob`] es dueño del cliente MCP (y del hijo) hasta que termina.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::error::{IntegrationError, Result};
use crate::mcp_client::McpClient;
use crate::suite::{Outcome, SisterApp, Suite, absolute};

/// Espera máxima que acepta `transcription_status` por llamada.
const MAX_WAIT_SECS: u64 = 110;

/// Un archivo de transcripción escrito junto al audio.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptFile {
    /// `txt`, `srt`, `vtt`, `json`, `md`…
    pub format: String,
    /// Ruta.
    pub path: PathBuf,
}

/// Una transcripción terminada (o un trozo, si es larga: ver `next_offset`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Transcript {
    /// Texto (con `[hh:mm:ss]` si se pidió).
    pub text: String,
    /// Caracteres totales.
    pub total_chars: u64,
    /// Si hay más texto, desde dónde seguir.
    pub next_offset: Option<u64>,
    /// Archivos escritos.
    pub outputs: Vec<TranscriptFile>,
    /// Idioma detectado.
    pub language: Option<String>,
    /// Duración del audio.
    pub audio_secs: Option<f64>,
}

impl Transcript {
    fn from_value(v: &Value) -> Self {
        let outputs = v
            .get("outputs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .take(32)
            .filter_map(|o| {
                Some(TranscriptFile {
                    format: o.get("format").and_then(Value::as_str).unwrap_or("").to_string(),
                    path: PathBuf::from(o.get("path").and_then(Value::as_str)?),
                })
            })
            .collect();
        Self {
            text: v.get("text").and_then(Value::as_str).unwrap_or("").to_string(),
            total_chars: v.get("totalChars").and_then(Value::as_u64).unwrap_or(0),
            next_offset: v.get("nextOffset").and_then(Value::as_u64),
            outputs,
            language: v.get("detectedLanguage").and_then(Value::as_str).map(String::from),
            audio_secs: v.get("audioSecs").and_then(Value::as_f64),
        }
    }

    /// El archivo de un formato, si se escribió.
    pub fn output(&self, format: &str) -> Option<&Path> {
        self.outputs.iter().find(|o| o.format.eq_ignore_ascii_case(format)).map(|o| o.path.as_path())
    }
}

/// Estado del trabajo.
#[derive(Clone, Debug, PartialEq)]
pub enum JobState {
    /// En curso.
    Running {
        /// 0–100.
        percent: u8,
        /// `decoding`, `loading`, `transcribing`…
        stage: String,
    },
    /// Terminado.
    Done(Transcript),
}

/// Transcripción en curso. Soltarlo cancela (mata el proceso de IureTranscribe).
#[derive(Debug)]
pub struct TranscriptionJob {
    client: McpClient,
    job_id: Option<String>,
    input: PathBuf,
    state: JobState,
}

fn parse_state(v: &Value) -> Result<(JobState, Option<String>)> {
    let job = v.get("jobId").and_then(Value::as_str).map(String::from);
    match v.get("status").and_then(Value::as_str) {
        Some("running") => Ok((
            JobState::Running {
                percent: v.get("percent").and_then(Value::as_u64).unwrap_or(0).min(100) as u8,
                stage: v.get("stage").and_then(Value::as_str).unwrap_or("").to_string(),
            },
            job,
        )),
        Some("done") => Ok((JobState::Done(Transcript::from_value(v)), job)),
        other => Err(IntegrationError::Protocol(format!("estado de transcripción desconocido: {other:?}"))),
    }
}

impl TranscriptionJob {
    /// Archivo de audio o video.
    pub fn input(&self) -> &Path {
        &self.input
    }

    /// Último estado conocido.
    pub fn state(&self) -> &JobState {
        &self.state
    }

    /// Espera hasta `wait` (máx. 110 s) y devuelve el estado.
    pub fn poll(&mut self, wait: Duration) -> Result<JobState> {
        if matches!(self.state, JobState::Done(_)) {
            return Ok(self.state.clone());
        }
        let Some(job) = self.job_id.clone() else {
            return Err(IntegrationError::Protocol("trabajo sin jobId".into()));
        };
        let secs = wait.as_secs().min(MAX_WAIT_SECS);
        let r = self
            .client
            .call_tool(
                "transcription_status",
                json!({"job_id": job, "wait_seconds": secs, "timestamps": true}),
                Some(Duration::from_secs(secs.saturating_add(30))),
            )?
            .into_ok()?;
        let data = r.data().ok_or_else(|| IntegrationError::Protocol("respuesta sin datos".into()))?;
        self.state = parse_state(&data)?.0;
        Ok(self.state.clone())
    }

    /// Sigue leyendo una transcripción larga desde `offset`.
    pub fn read_more(&mut self, offset: u64) -> Result<Transcript> {
        let Some(job) = self.job_id.clone() else {
            return match &self.state {
                JobState::Done(t) => Ok(t.clone()),
                JobState::Running { .. } => Err(IntegrationError::Protocol("trabajo sin jobId".into())),
            };
        };
        let r = self
            .client
            .call_tool(
                "transcription_status",
                json!({"job_id": job, "offset": offset, "wait_seconds": 0, "timestamps": true}),
                None,
            )?
            .into_ok()?;
        let data = r.data().ok_or_else(|| IntegrationError::Protocol("respuesta sin datos".into()))?;
        match parse_state(&data)?.0 {
            JobState::Done(t) => Ok(t),
            JobState::Running { .. } => Err(IntegrationError::Protocol("la transcripción aún no termina".into())),
        }
    }
}

impl Suite {
    /// Empieza a transcribir una grabación (ruta local). Vuelve enseguida (≤ ~5 s): sigue con
    /// [`TranscriptionJob::poll`] en el runtime de fondo.
    pub fn transcribe_recording(&self, file: &Path) -> Result<Outcome<TranscriptionJob>> {
        let mut client = match self.connect_mcp(SisterApp::Transcribe)? {
            Outcome::Ready(c) => c,
            Outcome::NotInstalled(h) => return Ok(Outcome::NotInstalled(h)),
            Outcome::NotAvailable(t) => return Ok(Outcome::NotAvailable(t)),
        };
        let input = absolute(file)?;
        let r = client
            .call_tool(
                "transcribe_file",
                json!({"path": input, "wait_seconds": 2, "timestamps": true}),
                Some(Duration::from_secs(60)),
            )?
            .into_ok()?;
        let data = r.data().ok_or_else(|| IntegrationError::Protocol("respuesta sin datos".into()))?;
        let (state, job_id) = parse_state(&data)?;
        Ok(Outcome::Ready(TranscriptionJob { client, job_id, input, state }))
    }

    /// Lee una transcripción o minuta ya escrita (`.txt`, `.srt`, `.vtt`, `.json`, `.md`).
    pub fn read_transcript(&self, file: &Path, offset: u64) -> Result<Outcome<Transcript>> {
        let mut client = match self.connect_mcp(SisterApp::Transcribe)? {
            Outcome::Ready(c) => c,
            Outcome::NotInstalled(h) => return Ok(Outcome::NotInstalled(h)),
            Outcome::NotAvailable(t) => return Ok(Outcome::NotAvailable(t)),
        };
        let path = absolute(file)?;
        let r = client.call_tool("read_transcript", json!({"path": path, "offset": offset}), None)?.into_ok()?;
        let data = r.data().ok_or_else(|| IntegrationError::Protocol("respuesta sin datos".into()))?;
        Ok(Outcome::Ready(Transcript::from_value(&data)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estados() {
        let (s, j) =
            parse_state(&json!({"status": "running", "jobId": "a", "percent": 250, "stage": "decoding"})).unwrap();
        assert_eq!(s, JobState::Running { percent: 100, stage: "decoding".into() });
        assert_eq!(j.as_deref(), Some("a"));
        let (s, _) = parse_state(&json!({"status": "done", "text": "hola", "totalChars": 4,
            "outputs": [{"format": "txt", "path": "/a/b.txt"}]}))
        .unwrap();
        let JobState::Done(t) = s else { panic!("no terminó") };
        assert_eq!(t.output("TXT"), Some(Path::new("/a/b.txt")));
        assert!(parse_state(&json!({"status": 3})).is_err());
    }

    #[test]
    fn no_instalado() {
        let r = Suite::empty().transcribe_recording(Path::new("/tmp/x.wav"));
        assert!(matches!(r, Ok(Outcome::NotInstalled(h)) if h.app == SisterApp::Transcribe));
    }
}
