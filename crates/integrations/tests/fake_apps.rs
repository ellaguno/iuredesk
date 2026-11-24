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

/// S1.4: grabar con IureTranscribe sin abrir su ventana: empezar, pausar, estado y detener.
#[test]
fn grabar_una_reunion() {
    let s = fake_suite(SisterApp::Transcribe);
    let mut rec = s.start_recording().unwrap().ready().unwrap();
    assert_eq!(rec.path, Path::new("/tmp/grabaciones/reunion.wav"));
    let st = rec.status().unwrap();
    assert!(st.active && !st.paused && st.elapsed_secs > 12.0);
    assert_eq!(st.mic_level, 1.0, "niveles acotados a 0–1");
    rec.set_paused(true).unwrap();
    assert!(rec.status().unwrap().paused);
    let done = rec.stop().unwrap();
    assert_eq!(done.path, Path::new("/tmp/grabaciones/reunion.wav"));
    assert_eq!(done.duration_secs, 12.5);
    assert!(matches!(Suite::empty().start_recording().unwrap(), Outcome::NotInstalled(_)));
}

/// S1.6: el estado de varios planes con un solo proceso; uno roto no detiene a los demás.
#[test]
fn resumen_de_planes_de_iureproject() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("Caso Ramírez.iurp");
    let b = dir.path().join("roto.iurp");
    std::fs::write(&a, b"plan").unwrap();
    std::fs::write(&b, b"plan").unwrap();
    let missing = dir.path().join("no-existe.iurp");
    let s = fake_suite(SisterApp::Project);
    let r = s.project_overviews(&[a.clone(), b, missing]).unwrap().ready().unwrap();
    assert_eq!(r.len(), 3);
    let o = r[0].overview.as_ref().unwrap();
    assert_eq!((o.name.as_str(), o.percent_complete, o.late), ("Caso Ramírez", 42.5, 1));
    assert_eq!(o.late_tasks[0].name, "Peritaje");
    assert_eq!(o.upcoming_milestones[0].name, "Audiencia");
    assert!(r[1].overview.as_ref().unwrap_err().es.contains("no pudo leer"), "{:?}", r[1].overview);
    assert!(r[2].overview.as_ref().unwrap_err().es.contains("ya no está"), "{:?}", r[2].overview);
    // El expediente de Iurefficient del plan (panel del caso).
    assert_eq!(o.case.as_ref().map(|c| c.number.as_str()), Some("LAB-2026-0012"));
    // Sin IureProject: invitación, no error.
    assert!(matches!(Suite::empty().project_overviews(&[a]).unwrap(), Outcome::NotInstalled(_)));
}

/// «Correo → tarea del plan»: abrir, agregar con notas, fecha y responsable, enlazar el correo y
/// guardar; si IureProject guardó entre tanto, se pide intentar de nuevo.
#[test]
fn tarea_de_un_correo_en_un_plan() {
    let dir = tempfile::tempdir().unwrap();
    let plan = dir.path().join("Caso Ramírez.iurp");
    std::fs::write(&plan, b"plan").unwrap();
    let s = fake_suite(SisterApp::Project);
    let task = iuredesk_integrations::NewProjectTask {
        name: "Responder a Altamira".into(),
        notes: "De: Ana <ana@altamira.mx>".into(),
        deadline: Some("2026-11-25".into()),
        resource: Some("Laura".into()),
        link: Some(("iuredesk://message/m1@x".into(), "Correo de Ana".into())),
    };
    let t = s.add_project_task(&plan, &task).unwrap().ready().unwrap();
    assert_eq!((t.uid, t.name.as_str()), (40, "Responder a Altamira"));
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("Caso Ramírez.iurp.agregado.json")).unwrap())
            .unwrap();
    assert_eq!(saved[0]["deadline"], "2026-11-25");
    assert_eq!(saved[0]["resource"], "Laura");
    assert_eq!(saved[1]["hyperlink"]["address"], "iuredesk://message/m1@x");
    assert_eq!(saved[1]["hyperlink"]["uid"], 40);
    // Sin nombre: se dice, no se llama.
    let empty = iuredesk_integrations::NewProjectTask { name: "  ".into(), ..task.clone() };
    let Outcome::NotAvailable(e) = s.add_project_task(&plan, &empty).unwrap() else { panic!("sin nombre") };
    assert!(e.es.contains("nombre"));
    // El plan se acaba de guardar en IureProject.
    let busy = dir.path().join("ocupado.iurp");
    std::fs::write(&busy, b"plan").unwrap();
    let Outcome::NotAvailable(e) = s.add_project_task(&busy, &task).unwrap() else { panic!("debía fallar") };
    assert!(e.es.contains("inténtalo de nuevo"), "{e:?}");
    assert!(matches!(Suite::empty().add_project_task(&plan, &task).unwrap(), Outcome::NotInstalled(_)));
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

/// S1.35: abrir un plan en una tarea de Iurefficient lanza `iureproject plan.iurp --task <id>`.
#[test]
fn opens_a_plan_at_a_task() {
    let rec = Arc::new(Recorder::default());
    let s = Suite::empty().with_opener(rec.clone()).with_app(SisterApp::Project, "/opt/iureproject/iureproject");
    let dir = tempfile::tempdir().unwrap();
    let plan = dir.path().join("Caso.iurp");
    std::fs::write(&plan, b"{}").unwrap();
    assert!(matches!(s.open_in_iureproject_at(&plan, "srv-7"), Ok(Outcome::Ready(()))));
    let launched = rec.launched.lock().unwrap().clone();
    let (_, args) = launched.last().unwrap();
    assert_eq!(args.get(1..).unwrap(), ["--task", "srv-7"]);
    // Sin IureProject: se invita a instalarlo.
    assert!(matches!(Suite::empty().open_in_iureproject_at(&plan, "x"), Ok(Outcome::NotInstalled(_))));
}

/// S1.37: un caso de Iurefficient sin plan se abre en IureProject por su expediente.
#[test]
fn opens_a_case_in_iureproject() {
    let rec = Arc::new(Recorder::default());
    let s = Suite::empty().with_opener(rec.clone()).with_app(SisterApp::Project, "/opt/iureproject/iureproject");
    assert!(matches!(s.open_case_in_iureproject("c-241", "241/2026 · Arrendamiento\n"), Ok(Outcome::Ready(()))));
    let launched = rec.launched.lock().unwrap().clone();
    let (prog, args) = launched.last().unwrap();
    assert_eq!(prog, Path::new("/opt/iureproject/iureproject"));
    assert_eq!(args, &["--iurefficient-case", "c-241", "--case-title", "241/2026 · Arrendamiento"]);
    // Sin id: solo abre IureProject. Sin IureProject: se invita a instalarlo.
    assert!(matches!(s.open_case_in_iureproject(" ", ""), Ok(Outcome::Ready(()))));
    assert!(rec.launched.lock().unwrap().last().unwrap().1.is_empty());
    assert!(matches!(Suite::empty().open_case_in_iureproject("c-1", ""), Ok(Outcome::NotInstalled(_))));
}
