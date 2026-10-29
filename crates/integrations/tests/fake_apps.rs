//! Cliente MCP y acciones de la suite contra un servidor falso (`iuredesk-fake-mcp`, hecho con
//! el mismo `mcp_server` de connect que usan las apps hermanas).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iuredesk_integrations::{
    EditorTarget, IntegrationError, JobState, McpClient, McpOptions, OcrOptions, Opener, Outcome, SisterApp, Suite,
};
use serde_json::json;

const FAKE: &str = env!("CARGO_BIN_EXE_iuredesk-fake-mcp");

fn opts(mode: Option<&str>) -> McpOptions {
    McpOptions {
        init_timeout: Duration::from_secs(10),
        call_timeout: Duration::from_secs(10),
        env: mode.map(|m| vec![("FAKE_MCP_MODE".to_string(), m.to_string())]).unwrap_or_default(),
        ..McpOptions::default()
    }
}

#[test]
fn negocia_lista_y_llama() {
    let mut c = McpClient::spawn(Path::new(FAKE), &["--mcp"], &opts(None)).unwrap();
    assert_eq!(c.server().name, "fake");
    assert_eq!(c.server().protocol_version, "2025-06-18");
    assert_eq!(c.server().instructions.as_deref(), Some("Servidor de pruebas."));
    let tools = c.list_tools().unwrap();
    assert!(tools.iter().any(|t| t.name == "transcribe_file"));
    let r = c.call_tool("echo", json!({"a": 1}), None).unwrap();
    assert!(!r.is_error);
    assert_eq!(r.data().unwrap(), json!({"echo": {"a": 1}}));
    // isError → Tool con el texto de la app.
    let r = c.call_tool("fail", json!({}), None).unwrap();
    assert!(r.is_error);
    assert!(matches!(r.into_ok(), Err(IntegrationError::Tool(t)) if t.contains("propósito")));
    // Herramienta desconocida: error de protocolo JSON-RPC.
    assert!(matches!(c.call_tool("nada", json!({}), None), Err(IntegrationError::Rpc { code: -32602, .. })));
}

#[test]
fn ignora_basura_en_stdout() {
    let mut c = McpClient::spawn(Path::new(FAKE), &["--mcp"], &opts(Some("garbage"))).unwrap();
    assert!(c.call_tool("echo", json!({}), None).is_ok());
}

#[test]
fn tiempo_de_espera_y_cancelacion() {
    let mut c = McpClient::spawn(Path::new(FAKE), &["--mcp"], &opts(None)).unwrap();
    let t = Instant::now();
    let r = c.call_tool("sleep", json!({"ms": 5000}), Some(Duration::from_millis(200)));
    assert!(matches!(r, Err(IntegrationError::Timeout(_))));
    assert!(t.elapsed() < Duration::from_secs(3));
    // Tras la cancelación el cliente sigue sirviendo (la respuesta tardía no se confunde).
    let r = c.call_tool("echo", json!({"x": 2}), None).unwrap();
    assert_eq!(r.data().unwrap(), json!({"echo": {"x": 2}}));
}

#[test]
fn servidor_mudo_o_que_se_cierra() {
    let mut o = opts(Some("silent"));
    o.init_timeout = Duration::from_millis(300);
    assert!(matches!(McpClient::spawn(Path::new(FAKE), &["--mcp"], &o), Err(IntegrationError::Timeout(_))));
    assert!(matches!(
        McpClient::spawn(Path::new(FAKE), &["--mcp"], &opts(Some("exit"))),
        Err(IntegrationError::Closed(_))
    ));
}

fn fake_suite(app: SisterApp) -> Suite {
    Suite::empty().with_app(app, FAKE).with_mcp_options(opts(None))
}

#[test]
fn transcribir_con_trabajo_en_curso() {
    let dir = tempfile::tempdir().unwrap();
    let audio = dir.path().join("reunion.wav");
    std::fs::write(&audio, b"RIFF").unwrap();
    let s = fake_suite(SisterApp::Transcribe);
    let mut job = s.transcribe_recording(&audio).unwrap().ready().unwrap();
    assert!(matches!(job.state(), JobState::Running { percent: 10, .. }));
    // El jobId vive en el proceso hijo: el trabajo lo conserva entre consultas.
    assert!(matches!(job.poll(Duration::from_secs(1)).unwrap(), JobState::Running { percent: 55, .. }));
    let JobState::Done(t) = job.poll(Duration::from_secs(1)).unwrap() else { panic!("no terminó") };
    assert_eq!(t.text, "[00:00:01] Hola");
    assert_eq!(t.next_offset, Some(15));
    assert_eq!(t.language.as_deref(), Some("es"));
    assert_eq!(t.output("txt"), Some(Path::new("/tmp/rec.txt")));
    assert_eq!(job.read_more(15).unwrap().text, " mundo");
    let m = s.read_transcript(&audio, 0).unwrap().ready().unwrap();
    assert_eq!(m.text, "minuta");
}

#[test]
fn ocr_de_un_adjunto() {
    let dir = tempfile::tempdir().unwrap();
    let s = fake_suite(SisterApp::Ocr).with_cache_dir(dir.path());
    let file = s.stage_file("../escaneo.pdf", b"%PDF").unwrap();
    assert_eq!(file.parent(), Some(dir.path().join("adjuntos").as_path()));
    let o = OcrOptions { languages: Some("spa+eng".into()), ..OcrOptions::default() };
    let r = s.ocr_attachment(&file, &o).unwrap().ready().unwrap();
    assert_eq!(r.text, "TEXTO OCR");
    assert_eq!(r.pages, 2);
    let pdf = r.pdf_path.unwrap();
    assert!(pdf.to_string_lossy().ends_with("escaneo.pdf-ocr.pdf"));
    // Ruta relativa en output_dir: entrada no válida, sin lanzar nada.
    let bad = OcrOptions { output_dir: Some(PathBuf::from("rel")), ..OcrOptions::default() };
    assert!(matches!(s.ocr_attachment(&file, &bad), Err(IntegrationError::InvalidInput(_))));
}

#[test]
fn voz_por_mcp_sin_iurevoz_abierto() {
    let dir = tempfile::tempdir().unwrap();
    // Socket inexistente → cae a MCP, que dice que IureVoz no está abierto.
    let s = fake_suite(SisterApp::Voz).with_voz_socket(Some(dir.path().join("no.sock")));
    assert!(matches!(s.speak("hola").unwrap(), Outcome::NotAvailable(_)));
    assert!(matches!(s.ask_yes_no("¿sí?").unwrap(), Outcome::NotAvailable(_)));
}

#[test]
fn app_que_no_arranca_no_es_error() {
    let s = Suite::empty().with_app(SisterApp::Ocr, "/no/existe/iureocr");
    assert!(matches!(s.ocr_attachment(Path::new(FAKE), &OcrOptions::default()), Ok(Outcome::NotAvailable(_))));
}

#[derive(Default)]
struct Recorder {
    urls: Mutex<Vec<String>>,
    launched: Mutex<Vec<(PathBuf, Vec<String>)>>,
    fail_urls: bool,
}

impl Opener for Recorder {
    fn open_url(&self, url: &str) -> std::io::Result<()> {
        self.urls.lock().unwrap().push(url.to_string());
        if self.fail_urls { Err(std::io::Error::other("sin manejador")) } else { Ok(()) }
    }
    fn launch(&self, program: &Path, args: &[String]) -> std::io::Result<()> {
        self.launched.lock().unwrap().push((program.to_path_buf(), args.to_vec()));
        Ok(())
    }
}

#[test]
fn iureditor_por_deep_link() {
    let dir = tempfile::tempdir().unwrap();
    let rec = Arc::new(Recorder::default());
    let s = Suite::empty().with_opener(rec.clone()).with_cache_dir(dir.path());
    assert!(matches!(
        s.open_in_iureditor(&EditorTarget::File(PathBuf::from("/tmp"))).unwrap(),
        Outcome::NotInstalled(h) if h.app == SisterApp::Editor
    ));
    let s = s.with_app(SisterApp::Editor, "/opt/iureditor/iureditor");
    let opened = s
        .open_in_iureditor(&EditorTarget::NewDocument {
            file_name: "Respuesta a Ana".into(),
            markdown: "# Hola".into(),
        })
        .unwrap()
        .ready()
        .unwrap();
    let path = opened.path.unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "# Hola");
    assert!(path.to_string_lossy().ends_with("Respuesta a Ana.md"));
    // Ruta absoluta codificada: «%2F…» en Unix, «C%3A%5C…» en Windows.
    assert!(opened.link.starts_with("iureditor://open?path="), "{}", opened.link);
    assert!(opened.link.len() > "iureditor://open?path=".len() + 3);
    assert_eq!(rec.urls.lock().unwrap().len(), 1);
    assert!(rec.launched.lock().unwrap().is_empty());

    // Sin manejador del esquema: se lanza el ejecutable con el enlace.
    let rec = Arc::new(Recorder { fail_urls: true, ..Recorder::default() });
    let s = s.with_opener(rec.clone());
    let doc = EditorTarget::IurefficientDoc { case_id: None, doc_id: "7".into(), file_name: None, case_title: None };
    assert!(s.open_in_iureditor(&doc).unwrap().is_ready());
    let launched = rec.launched.lock().unwrap();
    let (prog, args) = launched.first().unwrap();
    assert_eq!(prog, Path::new("/opt/iureditor/iureditor"));
    assert_eq!(args, &vec!["iureditor://iurefficient/doc?doc=7".to_string()]);
}
