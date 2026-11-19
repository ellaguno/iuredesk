//! «Correo → tarea del plan» (IureProject): el formulario sale lleno con el correo abierto y
//! «Crear tarea» la manda a IureProject en segundo plano.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use egui::accesskit::Role;
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use iuredesk_accounts::MemoryStore;
use iuredesk_engine::iuredesk_integrations::{SisterApp, Suite};
use iuredesk_engine::{OfflineTransport, Session, SessionConfig, ThemePref, Zone};
use iuredesk_store::{Store, sample};
use iuredesk_ui::{DeskApp, Options};

static GPU: Mutex<()> = Mutex::new(());

fn app(dir: &std::path::Path) -> DeskApp {
    iuredesk_ui::iurefficient_ui::lang::set_lang(iuredesk_ui::iurefficient_ui::lang::Lang::Es);
    iuredesk_engine::lang::set_lang(iuredesk_engine::lang::Lang::Es);
    let now = Utc.with_ymd_and_hms(2026, 10, 21, 11, 20, 0).unwrap();
    let store = Store::open(dir).unwrap();
    sample::populate(&store, sample::DEFAULT_SEED, now).unwrap();
    let mut s = Session::new(SessionConfig {
        store: Arc::new(store),
        accounts: None,
        secrets: Arc::new(MemoryStore::default()),
        prefs_path: None,
        now: Some(now),
        zone: Zone::utc(),
        sample: true,
        sync: None,
        transport: Some(Arc::new(OfflineTransport)),
    })
    .unwrap();
    s.prefs.theme = ThemePref::Light;
    let mut app = DeskApp::new(s, Options { system_fonts: false, no_theme_watch: true, ..Default::default() });
    app.dismiss_reminders();
    app
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
}

#[test]
fn an_email_becomes_a_task_in_a_followed_plan() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let plans = tempfile::tempdir().unwrap();
    let plan = plans.path().join("Caso Ramírez.iurp");
    std::fs::write(&plan, b"plan").unwrap();
    let mut a = app(dir.path());
    a.set_suite(Suite::empty().with_app(SisterApp::Project, "/opt/iureproject/iureproject"));
    a.set_projects_for_tests(vec![plan.to_string_lossy().into_owned()], vec![]);
    let mut h = harness(a);
    settle(&mut h);
    // Abrir el primer correo (la fila, no su casilla).
    let first = h.state().session.rows_shared().first().map(|r| r.id).unwrap();
    let subject = h.state_mut().session.envelope(first).unwrap().subject.clone();
    h.query_all_by_label_contains(&subject).find(|n| n.accesskit_node().role() != Role::CheckBox).unwrap().click();
    settle(&mut h);
    // Desde «Más acciones» del lector.
    h.get_by_label("Más acciones").click();
    h.run_steps(2);
    h.get_by_label("Tarea en un proyecto…").click();
    settle(&mut h);
    assert!(h.query_by_label("Tarea nueva en un proyecto").is_some(), "el formulario");
    assert!(h.query_by_label("La tarea lleva un enlace a este correo.").is_some());
    // Fecha mal escrita: se dice y no se puede crear.
    let fecha = h.get_by_label("Fecha límite");
    fecha.focus();
    h.run();
    h.get_by_label("Fecha límite").type_text("25/11");
    h.run_steps(2);
    assert!(h.query_by_label("Usa AAAA-MM-DD").is_some());
    for _ in 0..5 {
        h.key_press(egui::Key::Backspace);
        h.run_steps(1);
    }
    h.get_by_label("Fecha límite").type_text("2026-11-25");
    h.run_steps(2);
    assert!(h.query_by_label("Usa AAAA-MM-DD").is_none());
    // «Crear tarea» solo se puede con un plan elegido: el que se sigue, ya puesto.
    // (El panel de contexto tiene su propio «Crear tarea», el de Iurefficient: el del formulario
    // se dibuja al final.)
    h.get_all_by_label("Crear tarea").last().unwrap().click();
    h.run_steps(3);
    assert!(h.query_by_label("Tarea nueva en un proyecto").is_none(), "se cerró y se mandó");
    assert!(h.query_all_by_label_contains("Agregando la tarea").next().is_some());
}

#[test]
fn without_iureproject_it_invites_to_install_it() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut a = app(dir.path());
    a.set_suite(Suite::empty());
    let mut h = harness(a);
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, "suite.project_task");
    settle(&mut h);
    assert!(h.query_by_label("Tarea nueva en un proyecto").is_none());
    assert!(h.query_all_by_label_contains("no está instalada").next().is_some(), "la invitación");
}

#[derive(Default)]
struct Launches(std::sync::Mutex<Vec<String>>);

impl iuredesk_engine::iuredesk_integrations::Opener for Launches {
    fn open_url(&self, url: &str) -> std::io::Result<()> {
        self.0.lock().unwrap().push(url.to_string());
        Ok(())
    }
    fn launch(&self, program: &std::path::Path, args: &[String]) -> std::io::Result<()> {
        self.0.lock().unwrap().push(format!("{} {}", program.display(), args.join(" ")));
        Ok(())
    }
}

/// S1.6: la tarjeta del caso (panel de contexto) muestra el plan de IureProject de ese expediente
/// —el que IureProject guardó en el plan— y lo abre.
#[test]
fn the_case_card_shows_its_plan() {
    use iuredesk_engine::iuredesk_integrations::{ProjectCase, ProjectOverview, ProjectStatus};
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let plans = tempfile::tempdir().unwrap();
    let plan = plans.path().join("Arrendamiento Altamira.iurp");
    std::fs::write(&plan, b"plan").unwrap();
    let launches = Arc::new(Launches::default());
    let mut a = app(dir.path());
    a.set_suite(
        Suite::empty().with_app(SisterApp::Project, "/opt/iureproject/iureproject").with_opener(launches.clone()),
    );
    let o = ProjectOverview {
        case: Some(ProjectCase { id: "c-241/2026".into(), number: "241/2026".into(), ..Default::default() }),
        name: "Arrendamiento Altamira".into(),
        percent_complete: 42.5,
        late: 1,
        ..Default::default()
    };
    let other = ProjectOverview {
        case: Some(ProjectCase { id: "c-otro".into(), ..Default::default() }),
        name: "Otro caso".into(),
        ..Default::default()
    };
    a.set_projects_for_tests(
        vec![plan.to_string_lossy().into_owned(), "/x/otro.iurp".into()],
        vec![
            ProjectStatus { file: plan.clone(), overview: Ok(o) },
            ProjectStatus { file: "/x/otro.iurp".into(), overview: Ok(other) },
        ],
    );
    let mut h = harness(a);
    settle(&mut h);
    // El correo del caso 241/2026 del buzón de ejemplo.
    let ids: Vec<_> = h.state().session.rows_shared().iter().map(|r| r.id).collect();
    let id = ids
        .into_iter()
        .find(|&id| {
            h.state_mut().session.envelope(id).is_some_and(|e| e.subject.contains("Dudas sobre el caso 241/2026"))
        })
        .unwrap();
    h.state_mut().session.set_selection(vec![id], Some(id));
    settle(&mut h);
    assert!(h.query_by_label_contains("Arrendamiento Altamira · 42 %").is_some(), "el plan del caso");
    assert!(h.query_by_label_contains("Otro caso").is_none(), "no el de otro expediente");
    h.get_by_label("Abrir en IureProject").click();
    let start = Instant::now();
    while launches.0.lock().unwrap().is_empty() && start.elapsed() < Duration::from_secs(10) {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(launches.0.lock().unwrap().join("\n").contains("Arrendamiento Altamira.iurp"));
}
