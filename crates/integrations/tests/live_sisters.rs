//! De punta a punta con las apps hermanas de verdad (no el servidor falso). Se corren a mano:
//!
//! ```text
//! IUREPROJECT_BIN=…/iureproject/target/debug/iureproject \
//! IUREDITOR2_BIN=…/iureditor/egui/target/release/iureditor2 \
//!   cargo test -p iuredesk-integrations --test live_sisters -- --ignored
//! ```
//!
//! Sin la variable, cada prueba se salta.

use std::path::PathBuf;
use std::time::Duration;

use iuredesk_integrations::{McpClient, McpOptions, NewProjectTask, Outcome, SisterApp, Suite};
use serde_json::json;

fn bin(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).map(PathBuf::from).filter(|p| p.is_file())
}

fn opts() -> McpOptions {
    McpOptions { init_timeout: Duration::from_secs(30), call_timeout: Duration::from_secs(60), ..McpOptions::default() }
}

fn ok(c: &mut McpClient, tool: &str, args: serde_json::Value) -> serde_json::Value {
    let r = c.call_tool(tool, args, None).unwrap_or_else(|e| panic!("{tool}: {e}"));
    let r = r.into_ok().unwrap_or_else(|e| panic!("{tool}: {e}"));
    r.data().unwrap_or_default()
}

/// IureDesk ↔ IureProject: plan con un hito, tarea desde un correo con su enlace, el resumen que
/// lee «Mi día» y el calendario, y que IureProject no reemplaza una versión más nueva.
#[test]
#[ignore = "necesita IUREPROJECT_BIN"]
fn iureproject_real() {
    let Some(project) = bin("IUREPROJECT_BIN") else { return };
    let dir = tempfile::tempdir().unwrap();
    let plan = dir.path().join("Caso Ramírez.iurp");
    // El plan, hecho por MCP como lo haría el usuario en IureProject: una tarea y un hito futuro.
    let mut c = McpClient::spawn(&project, &["--mcp"], &opts()).unwrap();
    assert_eq!(c.server().name, "iureproject");
    let tools: Vec<String> = c.list_tools().unwrap().into_iter().map(|t| t.name).collect();
    for t in ["file_open", "file_save", "task_add_list", "task_set_hyperlink", "project_overview"] {
        assert!(tools.iter().any(|n| n == t), "falta {t}");
    }
    ok(&mut c, "project_info", json!({"name": "Caso Ramírez"}));
    ok(&mut c, "task_insert", json!({"name": "Demanda", "duration": "3d"}));
    let hito = ok(&mut c, "task_insert", json!({"name": "Audiencia", "duration": "0d"}))["uid"].as_u64().unwrap();
    ok(&mut c, "task_milestone", json!({"uid": hito}));
    ok(&mut c, "file_save", json!({"path": plan}));
    drop(c);

    // «Correo → tarea del plan» desde IureDesk.
    let suite = Suite::empty().with_app(SisterApp::Project, &project);
    let task = NewProjectTask {
        name: "Responder a Altamira".into(),
        notes: "**De:** Ana <ana@altamira.mx>\n\n> ¿Me confirmas la audiencia?".into(),
        deadline: Some("2030-01-15".into()),
        resource: Some("Laura".into()),
        link: Some(("iuredesk://message/m1@altamira.mx".into(), "Correo: audiencia".into())),
    };
    let added = match suite.add_project_task(&plan, &task).unwrap() {
        Outcome::Ready(t) => t,
        other => panic!("no se agregó: {other:?}"),
    };
    assert!(added.uid > 0);

    // Lo que quedó en el archivo, leído por IureProject.
    let mut c = McpClient::spawn(&project, &["--mcp"], &opts()).unwrap();
    ok(&mut c, "file_open", json!({"path": plan}));
    let plan_json = ok(&mut c, "project_get", json!({}));
    let s = plan_json.to_string();
    assert!(s.contains("Responder a Altamira"), "{s}");
    assert!(s.contains("iuredesk://message/m1@altamira.mx"), "el enlace al correo: {s}");
    assert!(s.contains("Laura"), "el responsable: {s}");

    // El resumen que usan «Mi día», el calendario y la tarjeta del caso.
    let st = suite.project_overviews(std::slice::from_ref(&plan)).unwrap();
    let Outcome::Ready(st) = st else { panic!("sin resumen") };
    let o = st[0].overview.as_ref().unwrap();
    assert_eq!(o.name, "Caso Ramírez");
    assert!(o.tasks >= 3, "{o:?}");

    // Otra ventana (otro proceso) tenía el plan abierto y guarda después: no pisa la tarea.
    ok(&mut c, "task_insert", json!({"name": "Cambio de la ventana"}));
    let again = NewProjectTask { name: "Segunda tarea".into(), link: None, ..task };
    assert!(matches!(suite.add_project_task(&plan, &again).unwrap(), Outcome::Ready(_)));
    let r = c.call_tool("file_save", json!({}), None).unwrap().into_ok();
    assert!(r.is_err(), "IureProject no debe reemplazar la versión más nueva");
    let mut fresh = McpClient::spawn(&project, &["--mcp"], &opts()).unwrap();
    ok(&mut fresh, "file_open", json!({"path": plan}));
    let all = ok(&mut fresh, "task_find", json!({"text": "Segunda tarea"})).to_string();
    assert!(all.contains("Segunda tarea"), "{all}");
}

/// IureDesk ↔ IureEditor 2.0: el cliente MCP de IureDesk con `iureditor2 --mcp`: exportar a PDF y
/// crear un documento desde una plantilla.
#[test]
#[ignore = "necesita IUREDITOR2_BIN"]
fn iureditor2_real() {
    let Some(editor) = bin("IUREDITOR2_BIN") else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut o = opts();
    o.env.push(("IUREDITOR_TEMPLATES_DIR".into(), dir.path().join("plantillas").display().to_string()));
    let mut c = McpClient::spawn(&editor, &["--mcp"], &o).unwrap();
    assert_eq!(c.server().name, "iureditor");
    let md = dir.path().join("nota.md");
    std::fs::write(&md, "# Minuta\n\n- [x] Acuerdo\n\n```mermaid\ngraph LR; A-->B\n```\n").unwrap();
    let out = ok(&mut c, "exportar_documento", json!({"ruta": md, "formato": "pdf"}));
    let pdf = PathBuf::from(out["destino"].as_str().unwrap());
    assert!(std::fs::read(&pdf).unwrap().starts_with(b"%PDF-"));
    let names = ok(&mut c, "listar_plantillas", json!({}));
    assert!(names["plantillas"].to_string().contains("Acta de reunión"));
    let nuevo = dir.path().join("acta.md");
    ok(&mut c, "crear_desde_plantilla", json!({"plantilla": "Acta de reunión", "destino": nuevo}));
    assert!(std::fs::read_to_string(&nuevo).unwrap().contains("# Acta de reunión"));
    let html = ok(&mut c, "html_a_markdown", json!({"html": "<p>Hola <b>mundo</b></p>"}));
    assert_eq!(html["markdown"].as_str().unwrap().trim(), "Hola **mundo**");
}
