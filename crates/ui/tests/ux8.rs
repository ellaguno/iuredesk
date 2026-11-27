//! UX-8 con egui_kittest: vista previa nativa de las notas (Vista/Editar, Ctrl+Shift+E, casillas
//! con deshacer y regeneración cuando el archivo cambia desde IureEditor).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iuredesk_accounts::MemoryStore;
use iuredesk_engine::{Module, OfflineTransport, Session, SessionConfig, ThemePref, Zone};
use iuredesk_store::{Store, sample};
use iuredesk_ui::{DeskApp, Options};

static GPU: Mutex<()> = Mutex::new(());

const BODY: &str =
    "Intro con **negritas**.\n\n- [ ] Llamar al notario\n- [x] Enviar propuesta\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 21, 11, 20, 0).unwrap()
}

fn app(dir: &std::path::Path) -> DeskApp {
    iuredesk_ui::iurefficient_ui::lang::set_lang(iuredesk_ui::iurefficient_ui::lang::Lang::Es);
    iuredesk_engine::lang::set_lang(iuredesk_engine::lang::Lang::Es);
    let store = Store::open(dir).unwrap();
    sample::populate(&store, sample::DEFAULT_SEED, now()).unwrap();
    let mut s = Session::new(SessionConfig {
        store: Arc::new(store),
        accounts: None,
        secrets: Arc::new(MemoryStore::default()),
        prefs_path: None,
        now: Some(now()),
        zone: Zone::utc(),
        sample: true,
        sync: None,
        transport: Some(Arc::new(OfflineTransport)),
    })
    .unwrap();
    s.prefs.theme = ThemePref::Light;
    s.new_note().unwrap();
    let id = s.note_focus().unwrap();
    s.save_note(id, "Pendientes UX-8", BODY).unwrap();
    s.module = Module::Notes;
    let mut app = DeskApp::new(s, Options { system_fonts: false, no_theme_watch: true, ..Default::default() });
    app.dismiss_reminders();
    // UX-12: Notas solo con IureEditor; lo que se abre queda anotado, no se lanza nada.
    app.set_suite(
        iuredesk_engine::iuredesk_integrations::Suite::empty()
            .with_app(iuredesk_engine::iuredesk_integrations::SisterApp::Editor, "/opt/iureditor/iureditor")
            .with_opener(Arc::new(Recorder::default())),
    );
    app
}

/// Anota lo que se abriría.
#[derive(Default)]
struct Recorder(std::sync::Mutex<Vec<String>>);

impl iuredesk_engine::iuredesk_integrations::Opener for Recorder {
    fn open_url(&self, url: &str) -> std::io::Result<()> {
        self.0.lock().unwrap().push(url.to_string());
        Ok(())
    }
    fn launch(&self, program: &std::path::Path, args: &[String]) -> std::io::Result<()> {
        self.0.lock().unwrap().push(format!("{} {}", program.display(), args.join(" ")));
        Ok(())
    }
}

fn harness(app: DeskApp) -> Harness<'static, DeskApp> {
    Harness::builder().with_size(vec2(1440.0, 900.0)).build_ui_state(
        |ui, app: &mut DeskApp| {
            let full = ui.ctx().content_rect();
            ui.scope_builder(egui::UiBuilder::new().max_rect(full), |ui| {
                ui.set_clip_rect(full);
                app.ui(ui);
            });
        },
        app,
    )
}

fn settle(h: &mut Harness<'_, DeskApp>) {
    let start = Instant::now();
    let mut quiet = 0;
    while start.elapsed() < Duration::from_secs(60) {
        h.run_steps(1);
        if h.state().is_settled() {
            quiet += 1;
            if quiet > 4 {
                break;
            }
        } else {
            quiet = 0;
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    h.run_steps(2);
}

fn body(h: &Harness<'_, DeskApp>) -> String {
    let s = &h.state().session;
    let id = s.note_focus().unwrap();
    s.notes().into_iter().find(|n| n.id == id).unwrap().body
}

/// UX-12 + S1.12: la nota se ve por omisión; «Editar aquí» (Ctrl+Mayús+E) abre el DocEditor,
/// lo escrito se guarda solo al dejar de teclear y «Ver» vuelve a la vista.
/// ¿Se ve `text` en la nota? La vista es el editor solo para leer: el texto es el valor de sus campos.
fn shows(h: &Harness<'_, DeskApp>, text: &str) -> bool {
    h.query_all(egui_kittest::kittest::by().predicate(|n| n.value().is_some_and(|v| v.contains(text)))).next().is_some()
}

#[test]
fn notes_view_and_edit_here() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    assert!(h.query_by_label("Texto de la nota").is_none(), "se ve, no se edita");
    assert!(h.query_by_label("Editar en IureEditor").is_some());
    assert!(shows(&h, "Llamar al notario"), "la vista");
    h.get_by_label("Editar aquí").click();
    settle(&mut h);
    let editor = h.get_by_label("Texto de la nota");
    editor.focus();
    h.run_steps(2);
    h.get_by_label("Texto de la nota").type_text("Nota nueva al principio. ");
    // Se guarda solo tras dejar de teclear.
    let start = Instant::now();
    while !body(&h).contains("Nota nueva al principio.") {
        assert!(start.elapsed() < Duration::from_secs(15), "no se guardó");
        h.run_steps(5);
        std::thread::sleep(Duration::from_millis(30));
    }
    assert!(body(&h).contains("Llamar al notario"), "el resto intacto");
    // La barra de formato actúa sobre la línea del cursor (y se guarda igual).
    h.get_by_label("Lista de tareas").click();
    let start = Instant::now();
    while !body(&h).starts_with("- [ ] Intro con") {
        assert!(start.elapsed() < Duration::from_secs(15), "la barra no cambió la nota: {}", body(&h));
        h.run_steps(5);
        std::thread::sleep(Duration::from_millis(30));
    }
    // Ctrl+Mayús+E (o «Ver») vuelve a la vista.
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, "notes.toggle_edit");
    settle(&mut h);
    assert!(h.query_by_label("Texto de la nota").is_none());
    assert!(h.query_by_label("Editar aquí").is_some());
}

/// UX-12: «Nueva nota» la crea con título y la abre en IureEditor (donde se escribe).
#[test]
fn new_note_opens_in_iureditor() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let notes = tempfile::tempdir().unwrap();
    let mut a = app(dir.path());
    let rec = Arc::new(Recorder::default());
    a.set_suite(
        iuredesk_engine::iuredesk_integrations::Suite::empty()
            .with_app(iuredesk_engine::iuredesk_integrations::SisterApp::Editor, "/opt/iureditor/iureditor")
            .with_opener(rec.clone()),
    );
    a.session.notes_fs_for_tests(notes.path().to_path_buf(), Duration::from_millis(30));
    assert!(a.session.notes_fs_flush(Duration::from_secs(10)));
    let before = a.session.notes().len();
    let mut h = harness(a);
    settle(&mut h);
    h.get_by_label("Nueva nota").click();
    settle(&mut h);
    let start = Instant::now();
    while rec.0.lock().unwrap().is_empty() {
        assert!(start.elapsed() < Duration::from_secs(15), "no se abrió IureEditor");
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(20));
    }
    let s = &h.state().session;
    assert_eq!(s.notes().len(), before + 1);
    let id = s.note_focus().unwrap();
    let n = s.notes().into_iter().find(|n| n.id == id).unwrap();
    assert!(n.title.starts_with("Nota "), "{}", n.title);
    let opened = rec.0.lock().unwrap().join("\n");
    assert!(opened.contains(".md"), "abre su archivo: {opened}");
}

/// UX-12: sin IureEditor no hay Notas: ni en el riel ni por comando, y se sale de ellas.
#[test]
fn without_iureditor_notes_disappear() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut a = app(dir.path());
    a.set_suite(iuredesk_engine::iuredesk_integrations::Suite::empty());
    let mut h = harness(a);
    settle(&mut h);
    assert_eq!(h.state().session.module, Module::Today, "salió de Notas");
    assert!(h.query_by_label("Notas").is_none(), "sin botón en el riel");
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, "nav.notes");
    settle(&mut h);
    assert_eq!(h.state().session.module, Module::Today);
    assert!(h.query_all_by_label_contains("instálalo").next().is_some(), "dice por qué");
}

/// Marcar una casilla en la vista reescribe el Markdown y se deshace.
#[test]
fn task_checkbox_rewrites_markdown_with_undo() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    h.state_mut().toggle_note_task(0);
    settle(&mut h);
    assert!(body(&h).contains("- [x] Llamar al notario"), "{}", body(&h));
    h.state_mut().toggle_note_task(1);
    settle(&mut h);
    assert!(body(&h).contains("- [ ] Enviar propuesta"));
    // Una casilla que no existe no cambia nada.
    h.state_mut().toggle_note_task(9);
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, "undo");
    h.state_mut().run(&ctx, "undo");
    settle(&mut h);
    assert_eq!(body(&h), BODY);
}

/// Abre el submenú cuyo título empieza con `title` y pulsa `item`.
fn pick_in_submenu(h: &mut Harness<'static, DeskApp>, title: &str, item: &str) {
    h.get_by_label_contains(title).click();
    settle(h);
    h.get_by_label(item).click();
    settle(h);
}

/// S1.19–S1.20: pulsar un mapa mental abre su menú; elegir distribución, forma, tamaño del centro
/// y paleta reescribe solo el metadato del mapa, el menú se cierra y todo se deshace.
#[test]
fn mindmap_menu_rewrites_the_meta_with_undo() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut a = app(dir.path());
    let id = a.session.note_focus().unwrap();
    let md = "Antes\n\n> [!MINDMAP|map] Proyecto\n> - Análisis\n> - Diseño\n";
    a.session.save_note(id, "Mapa", md).unwrap();
    let mut h = harness(a);
    settle(&mut h);
    h.state_mut().click_note_mindmap(0);
    settle(&mut h);
    pick_in_submenu(&mut h, "Distribución:", "De arriba abajo");
    assert_eq!(body(&h), md.replace("[!MINDMAP|map]", "[!MINDMAP|map layout-down]"));
    assert!(h.query_by_label_contains("Distribución:").is_none(), "el menú se cerró");
    h.state_mut().click_note_mindmap(0);
    settle(&mut h);
    pick_in_submenu(&mut h, "Forma de los nodos:", "Nube");
    assert_eq!(body(&h), md.replace("[!MINDMAP|map]", "[!MINDMAP|cloud map layout-down]"));
    h.state_mut().click_note_mindmap(0);
    settle(&mut h);
    pick_in_submenu(&mut h, "Tamaño del centro:", "Chico");
    h.state_mut().click_note_mindmap(0);
    settle(&mut h);
    pick_in_submenu(&mut h, "Colores de las ramas:", "Cálida");
    assert_eq!(body(&h), md.replace("[!MINDMAP|map]", "[!MINDMAP|cloud map layout-down root-s palette-warm]"));
    // Esc cierra sin cambiar nada; un mapa que no existe no abre menú.
    h.state_mut().click_note_mindmap(0);
    settle(&mut h);
    assert!(h.query_by_label_contains("Distribución:").is_some());
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    assert!(h.query_by_label_contains("Distribución:").is_none());
    h.state_mut().click_note_mindmap(5);
    settle(&mut h);
    assert!(h.query_by_label_contains("Distribución:").is_none());
    let ctx = h.ctx.clone();
    for _ in 0..4 {
        h.state_mut().run(&ctx, "undo");
    }
    settle(&mut h);
    assert_eq!(body(&h), md);
}

/// S1.20: el menú de un bloque Mermaid cambia el tema en su cabecera, con deshacer.
#[test]
fn mermaid_menu_changes_the_theme_with_undo() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut a = app(dir.path());
    let id = a.session.note_focus().unwrap();
    let md = "Plan\n\n```mermaid\nflowchart LR\n  A[Inicio] --> B[Fin]\n```\n";
    a.session.save_note(id, "Diagrama", md).unwrap();
    let mut h = harness(a);
    settle(&mut h);
    h.state_mut().click_note_mermaid(0);
    settle(&mut h);
    pick_in_submenu(&mut h, "Tema:", "Bosque");
    assert_eq!(body(&h), md.replace("```mermaid\n", "```mermaid\n---\nconfig:\n  theme: forest\n---\n"));
    assert!(h.query_by_label_contains("Tema:").is_none(), "el menú se cerró");
    h.state_mut().click_note_mermaid(0);
    settle(&mut h);
    pick_in_submenu(&mut h, "Tema:", "Como la app");
    assert_eq!(body(&h), md, "volver al tema de la app quita la cabecera");
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, "undo");
    settle(&mut h);
    assert!(body(&h).contains("theme: forest"));
    h.state_mut().run(&ctx, "undo");
    settle(&mut h);
    assert_eq!(body(&h), md);
}

/// S1.20: la ventana de colores dibuja en vivo y guarda `themeVariables` estándar, con deshacer.
#[test]
fn mermaid_colors_with_live_preview_and_undo() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut a = app(dir.path());
    let id = a.session.note_focus().unwrap();
    let md = "```mermaid\nflowchart LR\n  A --> B\n```\n";
    a.session.save_note(id, "Colores", md).unwrap();
    let mut h = harness(a);
    settle(&mut h);
    h.state_mut().click_note_mermaid(0);
    settle(&mut h);
    h.get_by_label("Colores…").click();
    settle(&mut h);
    assert!(h.query_by_label("Relleno de los nodos").is_some(), "la ventana de colores");
    h.state_mut().set_diagram_color("primaryColor", Some([0xdb, 0xea, 0xfe]));
    h.state_mut().set_diagram_color("lineColor", Some([0x1d, 0x4e, 0xd8]));
    settle(&mut h);
    h.get_by_label("Guardar").click();
    settle(&mut h);
    assert_eq!(
        body(&h),
        "```mermaid\n---\nconfig:\n  themeVariables:\n    primaryColor: \"#dbeafe\"\n    lineColor: \"#1d4ed8\"\n---\nflowchart LR\n  A --> B\n```\n"
    );
    assert!(h.query_by_label("Relleno de los nodos").is_none(), "se cerró");
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, "undo");
    settle(&mut h);
    assert_eq!(body(&h), md);
}

/// S1.24: el enlace de un nodo pide confirmación; cancelar no abre nada.
#[test]
fn node_links_ask_before_opening() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    h.state_mut().click_note_node_link("https://example.com/caso");
    settle(&mut h);
    assert!(h.query_by_label("https://example.com/caso").is_some(), "muestra a dónde lleva");
    assert_eq!(h.state().pending_node_link().as_deref(), Some("https://example.com/caso"));
    h.get_by_label("Cancelar").click();
    settle(&mut h);
    assert_eq!(h.state().pending_node_link(), None);
}

/// S1.20: el editor dibuja en vivo; con un error lo explica, conserva el último dibujo válido y
/// al guardar reescribe solo el bloque.
#[test]
fn mermaid_editor_live_preview_and_save() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut a = app(dir.path());
    let id = a.session.note_focus().unwrap();
    let md = "Antes\n\n```mermaid\nflowchart LR\n  A --> B\n```\n\nDespués\n";
    a.session.save_note(id, "Diagrama", md).unwrap();
    let mut h = harness(a);
    settle(&mut h);
    h.state_mut().click_note_mermaid(0);
    settle(&mut h);
    h.get_by_label("Editar código…").click();
    settle(&mut h);
    assert_eq!(h.state().diagram_editor_state(), (None, true), "dibuja el código actual");
    h.state_mut().set_diagram_code("flowchart LR\n  A --> B\n  C -->");
    settle(&mut h);
    let (err, drawing) = h.state().diagram_editor_state();
    let err = err.expect("explica el error");
    assert!(err.contains("línea 3"), "{err}");
    assert!(drawing, "se sigue viendo el último dibujo válido");
    assert!(h.query_all_by_label_contains("línea 3").next().is_some(), "el error se ve");
    h.state_mut().set_diagram_code("flowchart TD\n  A --> B\n  B --> C");
    settle(&mut h);
    assert_eq!(h.state().diagram_editor_state(), (None, true));
    h.get_by_label("Guardar").click();
    settle(&mut h);
    assert_eq!(body(&h), md.replace("flowchart LR\n  A --> B\n", "flowchart TD\n  A --> B\n  B --> C\n"));
    assert!(h.query_by_label("Guardar").is_none(), "el editor se cerró");
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, "undo");
    settle(&mut h);
    assert_eq!(body(&h), md);
}

/// S1.20: «Ver en grande» abre el visor con zoom.
#[test]
fn diagram_viewer_zooms() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut a = app(dir.path());
    let id = a.session.note_focus().unwrap();
    a.session.save_note(id, "Mapa", "> [!MINDMAP] Centro\n> - Uno\n> - Dos\n").unwrap();
    let mut h = harness(a);
    settle(&mut h);
    h.state_mut().click_note_mindmap(0);
    settle(&mut h);
    h.get_by_label("Ver en grande").click();
    settle(&mut h);
    assert!(h.query_by_label("100 %").is_some());
    h.get_by_label("+").click();
    settle(&mut h);
    assert!(h.query_by_label("125 %").is_some(), "acercó");
    h.get_by_label("−").click();
    h.get_by_label("−").click();
    settle(&mut h);
    assert!(h.query_by_label("80 %").is_some(), "alejó");
}

/// Lo guardado en IureEditor (el archivo) regenera la vista sin bloquear.
#[test]
fn preview_follows_the_file() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let notes = tempfile::tempdir().unwrap();
    let mut a = app(dir.path());
    a.session.notes_fs_for_tests(notes.path().to_path_buf(), Duration::from_millis(30));
    assert!(a.session.notes_fs_flush(Duration::from_secs(10)));
    assert!(a.session.notes_fs_flush(Duration::from_secs(10)));
    let mut h = harness(a);
    settle(&mut h);
    let id = h.state().session.note_focus().unwrap();
    let p = h.state_mut().session.note_path(id).unwrap();
    let md = std::fs::read_to_string(&p).unwrap();
    std::fs::write(&p, format!("{md}\n## Desde IureEditor\n")).unwrap();
    let start = Instant::now();
    while !body(&h).contains("Desde IureEditor") {
        assert!(start.elapsed() < Duration::from_secs(15));
        h.state_mut().session.notes_fs_flush(Duration::from_secs(5));
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(40));
    }
    settle(&mut h);
    assert!(shows(&h, "Desde IureEditor"), "la vista muestra lo nuevo");
}

/// S1.4: mientras se graba hay una barra visible con el título y el tiempo; «Detener» la quita
/// (sin proceso que detener no se queda en «Guardando»). UX-31: la reunión de una cita llega como
/// carpeta «Reuniones/AAAA-MM-DD Título» con minuta, resumen y transcripción (cada una con las
/// ligas a IureTranscribe y a la cita), y la grabación queda en la cita.
#[test]
fn recording_pill_and_minutes_note() {
    use iuredesk_engine::iuredesk_integrations::{DocKind, GeneratedDoc};
    use iuredesk_engine::suite::MeetingResult;
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let notes = tempfile::tempdir().unwrap();
    let mut a = app(dir.path());
    a.session.notes_fs_for_tests(notes.path().to_path_buf(), Duration::from_millis(30));
    assert!(a.session.notes_fs_flush(Duration::from_secs(10)));
    let mut h = harness(a);
    settle(&mut h);
    h.state_mut().set_recording_for_tests("Comité de socios", 724.0);
    h.run_steps(3);
    assert!(h.query_by_label_contains("Grabando · Comité de socios · 12:04").is_some(), "la barra");
    h.get_by_label("Pausar").click();
    h.get_by_label("Detener").click();
    let start = Instant::now();
    while h.state().is_recording() {
        assert!(start.elapsed() < Duration::from_secs(10), "la barra no se fue");
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(20));
    }
    h.run_steps(3);
    assert!(h.query_by_label_contains("Grabando ·").is_none(), "la barra se fue");

    // La cita «Comité de socios» del ejemplo, como al pulsar «Grabar» en ella.
    let from = Zone::utc().day_start(chrono::NaiveDate::from_ymd_opt(2026, 10, 21).unwrap());
    let occ = h
        .state_mut()
        .session
        .visible_occurrences(from, from + chrono::Duration::days(1))
        .into_iter()
        .find(|o| o.summary == "Comité de socios")
        .unwrap();
    let meeting = h.state().meeting_for_tests(&occ);
    assert_eq!(meeting.meta.title, "Comité de socios");
    assert!(meeting.meta.date.starts_with("2026-10-21"), "{}", meeting.meta.date);
    let uid = meeting.event.as_ref().unwrap().uid.clone();
    let doc = |kind, content: &str| GeneratedDoc { kind, path: "/tmp/reunion_x.md".into(), content: content.into() };
    let before = h.state().session.notes().len();
    h.state_mut().meeting_transcript_for_tests(&MeetingResult {
        title: "Comité de socios".into(),
        text: "[00:00:01] Buenos días a todos.".into(),
        audio: "/tmp/reunion.wav".into(),
        open_link: Some("iuretranscribe://open?path=%2Ftmp%2Freunion.wav".into()),
        summary: Some(doc(DocKind::Summary, "# Resumen\n\nSe revisó el presupuesto.")),
        minutes: Some(doc(DocKind::Minutes, "# Minuta\n\n- Acuerdo: contratar perito.")),
        docs_problem: None,
        meeting,
    });
    settle(&mut h);
    assert!(h.state_mut().session.notes_fs_flush(Duration::from_secs(10)));
    let s = &h.state().session;
    assert_eq!(s.notes().len(), before + 3);
    let folder = "Reuniones/2026-10-21 Comité de socios";
    assert_eq!(s.notes_folder_view(), folder);
    let id = s.note_focus().unwrap();
    let n = s.notes().into_iter().find(|n| n.id == id).unwrap();
    assert_eq!(n.title, "Minuta — Comité de socios", "queda abierta la minuta");
    assert!(n.body.contains("contratar perito") && n.body.contains("/tmp/reunion.wav"), "{}", n.body);
    assert!(n.body.contains("(iuretranscribe://open?path=%2Ftmp%2Freunion.wav)"), "liga a IureTranscribe");
    assert!(n.body.contains("(iuredesk://event/"), "liga a la cita");
    assert_eq!(s.note_folder(&n), folder);
    for f in ["Minuta — Comité de socios.md", "Resumen — Comité de socios.md", "Transcripción — Comité de socios.md"]
    {
        assert!(notes.path().join(folder).join(f).is_file(), "{f}");
    }
    assert_eq!(s.module, Module::Notes);
    let rec = s.event_recordings(&uid);
    assert_eq!(rec.len(), 1);
    assert_eq!(rec[0].notes_folder.as_deref(), Some(folder));
    assert!(h.query_all_by_label_contains("Minuta, resumen y transcripción guardados en").next().is_some(), "el aviso");
}

/// UX-31: sin IA en IureTranscribe, la transcripción se guarda igual y el aviso dice por qué no hay
/// minuta.
#[test]
fn meeting_without_minutes_says_why() {
    use iuredesk_engine::suite::{MeetingInfo, MeetingResult};
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    h.state_mut().meeting_transcript_for_tests(&MeetingResult {
        title: "Llamada".into(),
        text: "hola".into(),
        audio: "/tmp/llamada.wav".into(),
        open_link: None,
        summary: None,
        minutes: None,
        docs_problem: Some(iuredesk_engine::lang::Text::new("Set your key.", "Configura tu llave de OpenRouter.")),
        meeting: MeetingInfo::default(),
    });
    settle(&mut h);
    let s = &h.state().session;
    let n = s.notes().into_iter().find(|n| Some(n.id) == s.note_focus()).unwrap();
    assert_eq!(n.title, "Transcripción — Llamada");
    assert!(h.query_all_by_label_contains("Sin minuta: Configura tu llave de OpenRouter.").next().is_some());
}

/// IureEditor en las notas: Ctrl+clic en `[[Título]]` abre esa nota; si no existe, la crea con ese
/// título (como IureEditor con sus archivos).
#[test]
fn wiki_links_open_or_create_notes() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    let ctx = h.ctx.clone();
    let before = h.state().session.notes().len();
    h.state_mut().follow_note_link(&ctx, "[[Ideas para la audiencia#Pruebas]]");
    settle(&mut h);
    let s = &h.state().session;
    assert_eq!(s.notes().len(), before + 1, "se creó");
    let id = s.note_focus().unwrap();
    let n = s.notes().into_iter().find(|n| n.id == id).unwrap();
    assert_eq!(n.title, "Ideas para la audiencia");
    // Otra vez (sin mayúsculas): la misma nota, no otra.
    h.state_mut().follow_note_link(&ctx, "[[ideas para la audiencia]]");
    settle(&mut h);
    assert_eq!(h.state().session.notes().len(), before + 1);
    assert_eq!(h.state().session.note_focus(), Some(id));
}

/// S1.33: en un diagrama de cuadrantes, un clic en un hueco de la gráfica ofrece «Agregar
/// elemento aquí»; pide el nombre y escribe el punto con las coordenadas del clic.
#[test]
fn quadrant_chart_adds_a_point_where_clicked() {
    use iuredesk_html_view as hv;
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    let code = "quadrantChart\n    title Prioridades\n    Campaña A: [0.3, 0.6]";
    let id = h.state().session.note_focus().unwrap();
    h.state_mut().session.save_note(id, "Cuadrante", &format!("```mermaid\n{code}\n```\n")).unwrap();
    h.get_by_label("Editar aquí").click();
    let start = Instant::now();
    while h.query_by_label("Diagrama 1").is_none() {
        assert!(start.elapsed() < Duration::from_secs(20), "no se dibujó");
        h.run_steps(2);
        std::thread::sleep(Duration::from_millis(30));
    }
    h.run_steps(3);
    let svg = hv::diagram::render_drawn(code, &hv::markdown::NoteStyle::default()).unwrap().svg;
    let (w, _) = hv::diagram::svg_size(&svg).unwrap();
    let g = hv::mermaid_edit::quadrant::geometry(&svg).unwrap();
    let img = h.get_by_label("Diagrama 1").rect();
    let k = img.width() / w;
    let [x0, y0, x1, y1] = g.plot;
    let p = img.min + egui::vec2((x0 + x1) / 2.0 * k, (y0 + y1) / 2.0 * k);
    h.event(egui::Event::PointerMoved(p));
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    h.run_steps(3);
    h.get_by_label("Agregar elemento aquí").click();
    h.run_steps(3);
    h.event(egui::Event::Text("Centro".into()));
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    let start = Instant::now();
    while !body(&h).contains("Centro: [0.5, 0.5]") {
        assert!(start.elapsed() < Duration::from_secs(15), "no se agregó: {}", body(&h));
        h.run_steps(5);
        std::thread::sleep(Duration::from_millis(30));
    }
    assert!(body(&h).contains("Campaña A: [0.3, 0.6]"), "lo demás igual");
}

/// S1.33: en un diagrama de flujo, un clic en un paso abre su barra; «Agregar siguiente» pide la
/// forma y el texto y escribe el paso nuevo con su flecha.
#[test]
fn flowchart_adds_the_next_step_from_the_drawing() {
    use iuredesk_html_view as hv;
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    let code = "flowchart TD\n    A[Inicio] --> B{¿Listo?}\n    B -->|sí| C(Fin)";
    let id = h.state().session.note_focus().unwrap();
    h.state_mut().session.save_note(id, "Flujo", &format!("```mermaid\n{code}\n```\n")).unwrap();
    h.get_by_label("Editar aquí").click();
    let start = Instant::now();
    while h.query_by_label("Diagrama 1").is_none() {
        assert!(start.elapsed() < Duration::from_secs(20), "no se dibujó");
        h.run_steps(2);
        std::thread::sleep(Duration::from_millis(30));
    }
    h.run_steps(3);
    let svg = hv::diagram::render_drawn(code, &hv::markdown::NoteStyle::default()).unwrap().svg;
    let (w, _) = hv::diagram::svg_size(&svg).unwrap();
    let nodes = hv::mermaid_edit::flowchart::nodes(code);
    // Accesibilidad: cada paso y cada flecha tienen su nombre (lector de pantalla y teclado).
    assert!(h.query_by_label("Inicio").is_some() && h.query_by_label("Inicio → ¿Listo?").is_some());
    let [x0, y0, x1, y1] = hv::mermaid_edit::flowchart::boxes(&svg, &nodes)[2].unwrap();
    let img = h.get_by_label("Diagrama 1").rect();
    let k = img.width() / w;
    let p = img.min + egui::vec2((x0 + x1) / 2.0 * k, (y0 + y1) / 2.0 * k);
    h.event(egui::Event::PointerMoved(p));
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    h.run_steps(3);
    h.get_by_label("Agregar siguiente").click();
    h.run_steps(3);
    h.get_by_label("Decisión").click();
    h.run_steps(3);
    h.event(egui::Event::Text("¿Otra vez?".into()));
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    let start = Instant::now();
    while !body(&h).contains("C --> N1{¿Otra vez?}") {
        assert!(start.elapsed() < Duration::from_secs(15), "no se agregó: {}", body(&h));
        h.run_steps(5);
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// S1.33: un clic en una flecha (en su etiqueta) abre su barra; «Tipo de flecha» la cambia.
#[test]
fn flowchart_arrow_type_changes_from_the_drawing() {
    use iuredesk_html_view as hv;
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    let code = "flowchart TD\n    A[Inicio] --> B{¿Listo?}\n    B -->|sí| C(Fin)";
    let id = h.state().session.note_focus().unwrap();
    h.state_mut().session.save_note(id, "Flujo", &format!("```mermaid\n{code}\n```\n")).unwrap();
    h.get_by_label("Editar aquí").click();
    let start = Instant::now();
    while h.query_by_label("Diagrama 1").is_none() {
        assert!(start.elapsed() < Duration::from_secs(20), "no se dibujó");
        h.run_steps(2);
        std::thread::sleep(Duration::from_millis(30));
    }
    h.run_steps(3);
    let svg = hv::diagram::render_drawn(code, &hv::markdown::NoteStyle::default()).unwrap().svg;
    let (w, _) = hv::diagram::svg_size(&svg).unwrap();
    let [x0, y0, x1, y1] = hv::mermaid_edit::flowchart::edge_boxes(&svg, 2)[1];
    let img = h.get_by_label("Diagrama 1").rect();
    let k = img.width() / w;
    let p = img.min + egui::vec2((x0 + x1) / 2.0 * k, (y0 + y1) / 2.0 * k);
    h.event(egui::Event::PointerMoved(p));
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    h.run_steps(3);
    h.get_by_label("Tipo de flecha").click();
    h.run_steps(3);
    h.get_by_label("Flecha punteada").click();
    let start = Instant::now();
    while !body(&h).contains("B -.->|sí| C(Fin)") {
        assert!(start.elapsed() < Duration::from_secs(15), "no cambió: {}", body(&h));
        h.run_steps(5);
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// S1.33 (fase 2): en un diagrama de secuencia, un clic en un participante ofrece «Mensaje a…»;
/// se elige el destino y se escribe el texto.
#[test]
fn sequence_adds_a_message_from_the_drawing() {
    use iuredesk_html_view as hv;
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    let code = "sequenceDiagram\n    participant C as Cliente\n    participant A as Abogado\n    C->>A: Consulta";
    let id = h.state().session.note_focus().unwrap();
    h.state_mut().session.save_note(id, "Secuencia", &format!("```mermaid\n{code}\n```\n")).unwrap();
    h.get_by_label("Editar aquí").click();
    let start = Instant::now();
    while h.query_by_label("Diagrama 1").is_none() {
        assert!(start.elapsed() < Duration::from_secs(20), "no se dibujó");
        h.run_steps(2);
        std::thread::sleep(Duration::from_millis(30));
    }
    h.run_steps(3);
    let svg = hv::diagram::render_drawn(code, &hv::markdown::NoteStyle::default()).unwrap().svg;
    let (w, _) = hv::diagram::svg_size(&svg).unwrap();
    let t = hv::mermaid_edit::sequence::tree(code);
    let [x0, y0, x1, y1] = hv::mermaid_edit::sequence::boxes(&svg, &t).0[1].unwrap();
    let img = h.get_by_label("Diagrama 1").rect();
    let k = img.width() / w;
    let p = img.min + egui::vec2((x0 + x1) / 2.0 * k, (y0 + y1) / 2.0 * k);
    h.event(egui::Event::PointerMoved(p));
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    h.run_steps(3);
    h.get_by_label("Mensaje a…").click();
    h.run_steps(3);
    // La opción del menú (el elemento del dibujo con ese nombre se crea antes).
    h.get_all_by_label("Cliente").last().unwrap().click();
    h.run_steps(3);
    h.event(egui::Event::Text("Propuesta".into()));
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    let start = Instant::now();
    while !body(&h).contains("A->>C: Propuesta") {
        assert!(start.elapsed() < Duration::from_secs(15), "no se agregó: {}", body(&h));
        h.run_steps(5);
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// Abre una nota con un diagrama, pulsa la zona `zone` (en unidades del dibujo) y devuelve el
/// arnés listo para usar la barra.
fn open_diagram_and_click(
    code: &str,
    zone: impl Fn(&str) -> [f32; 4],
) -> (Harness<'static, DeskApp>, tempfile::TempDir) {
    use iuredesk_html_view as hv;
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    let id = h.state().session.note_focus().unwrap();
    h.state_mut().session.save_note(id, "Diagrama", &format!("```mermaid\n{code}\n```\n")).unwrap();
    h.get_by_label("Editar aquí").click();
    let start = Instant::now();
    while h.query_by_label("Diagrama 1").is_none() {
        assert!(start.elapsed() < Duration::from_secs(20), "no se dibujó");
        h.run_steps(2);
        std::thread::sleep(Duration::from_millis(30));
    }
    h.run_steps(3);
    let svg = hv::diagram::render_drawn(code, &hv::markdown::NoteStyle::default()).unwrap().svg;
    let (w, _) = hv::diagram::svg_size(&svg).unwrap();
    let [x0, y0, x1, y1] = zone(&svg);
    let img = h.get_by_label("Diagrama 1").rect();
    let k = img.width() / w;
    let p = img.min + egui::vec2((x0 + x1) / 2.0 * k, (y0 + y1) / 2.0 * k);
    h.event(egui::Event::PointerMoved(p));
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    h.run_steps(3);
    (h, dir)
}

fn wait_body(h: &mut Harness<'static, DeskApp>, want: &str) {
    let start = Instant::now();
    while !body(h).contains(want) {
        assert!(start.elapsed() < Duration::from_secs(15), "falta «{want}»: {}", body(h));
        h.run_steps(5);
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// S1.33 (fase 2): una tarjeta de Kanban se mueve a otra columna desde su barra.
#[test]
fn kanban_card_moves_from_the_drawing() {
    use iuredesk_html_view::mermaid_edit::charts::kanban;
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let code = "kanban\n    Pendiente\n        t1[Redactar]\n    Hecho\n        t2[Firmar]";
    let (mut h, _dir) = open_diagram_and_click(code, |svg| kanban::boxes(svg, code)[0].1[0].unwrap());
    h.get_by_label("Mover a la columna").click();
    h.run_steps(3);
    // La opción del menú (el elemento del dibujo con ese nombre se crea antes).
    h.get_all_by_label("Hecho").last().unwrap().click();
    wait_body(&mut h, "    Hecho\n        t2[Firmar]\n        t1[Redactar]");
}

/// S1.33 (fase 2): a una clase se le agrega un atributo desde su barra.
#[test]
fn class_gets_a_member_from_the_drawing() {
    use iuredesk_html_view::mermaid_edit::structure::class;
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let code = "classDiagram\n    class Expediente {\n        +String numero\n    }\n    class Parte\n    Expediente --> Parte";
    let (mut h, _dir) = open_diagram_and_click(code, |svg| class::boxes(svg, code)[0].unwrap());
    h.get_by_label("Agregar atributo o método").click();
    h.run_steps(3);
    h.event(egui::Event::Text("+Date apertura".into()));
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    wait_body(&mut h, "        +String numero\n        +Date apertura\n    }");
}
