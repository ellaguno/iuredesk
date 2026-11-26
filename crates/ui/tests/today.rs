//! «Mi día» (F6.1) con egui_kittest: la pantalla se pinta con el buzón de ejemplo, el riel la
//! ofrece primero, y pulsar un elemento lleva a él (tarea → Tareas, correo → Correo enfocado,
//! cita → Calendario).

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

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 21, 11, 20, 0).unwrap()
}

fn app(dir: &std::path::Path) -> DeskApp {
    app_at(dir, now())
}

fn app_at(dir: &std::path::Path, at: chrono::DateTime<Utc>) -> DeskApp {
    iuredesk_ui::iurefficient_ui::lang::set_lang(iuredesk_ui::iurefficient_ui::lang::Lang::Es);
    iuredesk_engine::lang::set_lang(iuredesk_engine::lang::Lang::Es);
    let store = Store::open(dir).unwrap();
    sample::populate(&store, sample::DEFAULT_SEED, now()).unwrap();
    let mut s = Session::new(SessionConfig {
        store: Arc::new(store),
        accounts: None,
        secrets: Arc::new(MemoryStore::default()),
        prefs_path: None,
        now: Some(at),
        zone: Zone::utc(),
        sample: true,
        sync: None,
        transport: Some(Arc::new(OfflineTransport)),
    })
    .unwrap();
    s.prefs.theme = ThemePref::Light;
    s.open_at_start();
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
    h.run_steps(2);
}

#[test]
fn today_renders_and_items_navigate() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    assert_eq!(h.state().session.module, Module::Today, "usuario nuevo: abre en «Mi día»");
    assert!(h.query_all_by_label_contains("Buenos días, Laura").next().is_some(), "saludo");
    assert!(h.query_all_by_label_contains("Día despejado").next().is_none(), "el ejemplo tiene citas hoy");

    // UX-6: tarea → su editor, encima de «Mi día»; «Todas» lleva al módulo de Tareas.
    h.get_by_label("Presentar escrito Arrendamiento Altamira").click();
    settle(&mut h);
    assert!(h.state().task_editor_open());
    assert_eq!(h.state().session.module, Module::Today);
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    assert!(!h.state().task_editor_open());

    // Volver con el riel y abrir un correo que espera respuesta.
    h.state_mut().session.run("nav.today").unwrap();
    settle(&mut h);
    assert_eq!(h.state().session.module, Module::Today);
    let day = h.state_mut().session.day_summary().unwrap();
    let m = day.needs_reply.first().unwrap().clone();
    // UX-14: la caja «Pregúntale a…» empuja el correo hacia abajo; se desplaza como el usuario.
    let label = format!("{} — {}", m.from, m.subject);
    h.get_by_label(&label).scroll_to_me();
    settle(&mut h);
    h.get_by_label(&label).click();
    settle(&mut h);
    assert_eq!(h.state().session.module, Module::Mail);
    assert_eq!(h.state().session.focused(), Some(m.id), "el mensaje queda abierto");
}

#[test]
fn completing_a_task_from_today() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    h.get_by_label("Terminar Presentar escrito Arrendamiento Altamira").click();
    settle(&mut h);
    assert!(h.query_all_by_label_contains("Tarea terminada").next().is_some(), "aviso con deshacer");
    let day = h.state_mut().session.day_summary().unwrap();
    assert!(!day.tasks.today.iter().any(|t| t.task.summary == "Presentar escrito Arrendamiento Altamira"));
    assert_eq!(h.state().session.module, Module::Today, "terminar no navega");
}

/// UX-2: el botón «Unirse · Teams» de la próxima cita abre solo la reunión; antes la tarjeta
/// (que escuchaba el clic encima del botón) abría además el editor del evento y ganaba.
#[test]
fn join_button_opens_only_the_meeting() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    // Busca la reunión de Teams del ejemplo y vuelve a abrir «Mi día» poco antes de que empiece.
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    let day = h.state_mut().session.day_summary().unwrap();
    let teams = day.events.iter().find(|e| e.join.as_ref().is_some_and(|j| j.provider == "Teams")).unwrap().clone();
    let url = teams.join.as_ref().unwrap().url.clone();
    drop(h);
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app_at(dir.path(), teams.occ.start - chrono::Duration::minutes(5)));
    settle(&mut h);
    let day = h.state_mut().session.day_summary().unwrap();
    let (i, _) = day.next_event(h.state().session.now()).unwrap();
    assert_eq!(day.events.get(i).map(|e| e.occ.id), Some(teams.occ.id), "la de Teams es la siguiente");

    iuredesk_ui::capture_opened_urls();
    h.get_by_label("Unirse a la reunión").click();
    settle(&mut h);
    assert_eq!(iuredesk_ui::opened_urls(), vec![url], "se abre el enlace de la reunión");
    assert_eq!(h.state().session.module, Module::Today, "y no se abre el evento");

    // Un clic en el resto de la tarjeta (el título, en el centro) sí abre el evento.
    h.query_all_by_role_and_label(egui::accesskit::Role::Button, &teams.occ.summary).next().unwrap().click();
    settle(&mut h);
    assert_eq!(h.state().session.module, Module::Calendar);
    assert_eq!(iuredesk_ui::opened_urls().len(), 1, "sin abrir la reunión otra vez");
}

/// Anota lo que se lanzaría (S1.6).
#[derive(Default)]
struct Launches(Mutex<Vec<String>>);

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

/// S1.6: la tarjeta «Proyectos» muestra avance, lo atrasado y el próximo hito de cada plan; un
/// plan que no se pudo leer dice por qué; pulsar un plan lo abre en IureProject. Sin IureProject
/// no hay tarjeta.
#[test]
fn projects_card_shows_plans_and_opens_them() {
    use iuredesk_engine::iuredesk_integrations::{ProjectOverview, ProjectStatus, ProjectTask, SisterApp, Suite, Text};
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let plans = tempfile::tempdir().unwrap();
    let plan = plans.path().join("Caso Ramírez.iurp");
    std::fs::write(&plan, b"plan").unwrap();
    let broken = plans.path().join("Otro.iurp");
    let launches = Arc::new(Launches::default());
    let mut a = app(dir.path());
    a.set_suite(
        Suite::empty().with_app(SisterApp::Project, "/opt/iureproject/iureproject").with_opener(launches.clone()),
    );
    let task = |name: &str, finish: &str| ProjectTask {
        uid: 1,
        name: name.into(),
        finish: Some(finish.into()),
        percent_complete: 30,
        critical: true,
    };
    let ok = ProjectOverview {
        name: "Caso Ramírez".into(),
        as_of: "2026-10-21".into(),
        percent_complete: 42.5,
        tasks: 12,
        done: 4,
        late: 1,
        late_tasks: vec![task("Peritaje", "2026-10-10T17:00:00")],
        upcoming_milestones: vec![task("Audiencia", "2026-11-30T17:00:00")],
        ..Default::default()
    };
    a.set_projects_for_tests(
        vec![plan.to_string_lossy().into_owned(), broken.to_string_lossy().into_owned()],
        vec![
            ProjectStatus { file: plan.clone(), overview: Ok(ok) },
            ProjectStatus { file: broken.clone(), overview: Err(Text::new("gone", "El archivo ya no está.")) },
        ],
    );
    let mut h = harness(a);
    settle(&mut h);
    assert!(h.query_by_label("Caso Ramírez").is_some(), "el plan");
    assert!(h.query_by_label("43 %").is_some() || h.query_by_label("42 %").is_some(), "el avance");
    assert!(h.query_by_label("Peritaje — vencía 10 oct").is_some(), "lo atrasado");
    assert!(h.query_by_label("Audiencia — 30 nov").is_some(), "el próximo hito");
    // La del plan y la vencida del caso «Amparo Ruiz Mendoza» del ejemplo (S1.37).
    assert!(h.query_by_label("2 atrasadas").is_some(), "la cifra");
    assert!(h.query_by_label("El archivo ya no está.").is_some(), "el que no se pudo leer");
    h.get_by_label("Caso Ramírez").click();
    settle(&mut h);
    let start = Instant::now();
    while launches.0.lock().unwrap().is_empty() && start.elapsed() < Duration::from_secs(10) {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(20));
    }
    let l = launches.0.lock().unwrap().join("\n");
    assert!(l.contains("/opt/iureproject/iureproject") && l.contains("Caso Ramírez.iurp"), "{l}");
}

/// S1.6: sin IureProject no hay tarjeta de proyectos; con IureProject y sin planes, invita a seguir uno.
#[test]
fn projects_card_only_with_iureproject() {
    use iuredesk_engine::iuredesk_integrations::{SisterApp, Suite};
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut a = app(dir.path());
    a.set_suite(Suite::empty());
    let mut h = harness(a);
    settle(&mut h);
    assert!(h.query_by_label("Seguir un plan…").is_none());
    let dir2 = tempfile::tempdir().unwrap();
    let mut a = app(dir2.path());
    a.set_suite(Suite::empty().with_app(SisterApp::Project, "/opt/iureproject/iureproject"));
    let mut h = harness(a);
    settle(&mut h);
    assert!(h.query_by_label("Seguir un plan…").is_some(), "invita a seguir un plan");
}

/// Los hitos próximos de los planes seguidos se ven en el calendario como días completos de solo
/// lectura; pulsar uno abre su plan en IureProject.
#[test]
fn project_milestones_in_the_calendar() {
    use iuredesk_engine::iuredesk_integrations::{ProjectOverview, ProjectStatus, ProjectTask, SisterApp, Suite};
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let plans = tempfile::tempdir().unwrap();
    let plan = plans.path().join("Caso Ramírez.iurp");
    std::fs::write(&plan, b"plan").unwrap();
    let launches = Arc::new(Launches::default());
    let mut a = app(dir.path());
    a.set_suite(
        Suite::empty().with_app(SisterApp::Project, "/opt/iureproject/iureproject").with_opener(launches.clone()),
    );
    let hito = ProjectTask {
        uid: 9,
        name: "Audiencia".into(),
        finish: Some("2026-10-22T17:00:00".into()),
        percent_complete: 0,
        critical: true,
    };
    let o = ProjectOverview { name: "Caso Ramírez".into(), upcoming_milestones: vec![hito], ..Default::default() };
    a.set_projects_for_tests(
        vec![plan.to_string_lossy().into_owned()],
        vec![ProjectStatus { file: plan.clone(), overview: Ok(o) }],
    );
    a.session.module = Module::Calendar;
    let mut h = harness(a);
    settle(&mut h);
    let label = "Hito · Audiencia — Caso Ramírez";
    assert!(h.query_by_label(label).is_some(), "el hito en la semana");
    h.get_by_label(label).click();
    settle(&mut h);
    assert!(h.query_by_label("Guardar").is_none(), "no abre el editor de eventos");
    let start = Instant::now();
    while launches.0.lock().unwrap().is_empty() && start.elapsed() < Duration::from_secs(10) {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(20));
    }
    let l = launches.0.lock().unwrap().join("\n");
    assert!(l.contains("Caso Ramírez.iurp"), "{l}");
}

/// UX-19: al personalizar, una tarjeta se arrastra por su asa a otra columna y su color se cambia
/// o se quita desde la paleta.
#[test]
fn cards_are_dragged_and_recolored() {
    use iuredesk_engine::today_layout::{CardTint, TodayCard};
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut a = app(dir.path());
    a.demo_today_edit();
    let mut h = harness(a);
    settle(&mut h);
    assert_eq!(h.state().session.prefs.today_layout.tint(TodayCard::Agenda), Some(0x3B82F6), "azul del calendario");

    // Correo (izquierda) arriba de Tareas (derecha).
    let from = h.get_by_label("Arrastra para mover Correo").rect().center();
    let to = h.get_by_label("Arrastra para mover Tareas").rect().center() + egui::vec2(-200.0, -10.0);
    let press = |h: &mut Harness<'_, DeskApp>, pos: egui::Pos2, pressed: bool| {
        h.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        h.run_steps(1);
    };
    h.hover_at(from);
    h.run_steps(1);
    press(&mut h, from, true);
    for k in 1..=10 {
        let t = k as f32 / 10.0;
        h.event(egui::Event::PointerMoved(from + (to - from) * t));
        h.run_steps(1);
    }
    assert_eq!(h.state().today_dragging(), Some(TodayCard::Mail));
    press(&mut h, to, false);
    settle(&mut h);
    let l = h.state().session.prefs.today_layout.normalized();
    assert_eq!(l.left, vec![TodayCard::Agenda]);
    assert_eq!(l.right.first(), Some(&TodayCard::Mail), "{l:?}");
    assert_eq!(h.state().today_dragging(), None);

    // Color: rojo, luego sin color. Tareas quedó abajo (las noticias ocupan la cabecera): se
    // desplaza hasta ella como el usuario.
    h.get_by_label("Color de Tareas").scroll_to_me();
    settle(&mut h);
    h.get_by_label("Color de Tareas").click();
    settle(&mut h);
    h.get_by_label("#EF4444").click();
    settle(&mut h);
    assert_eq!(h.state().session.prefs.today_layout.tints.get(&TodayCard::Tasks), Some(&CardTint::Color(0xEF4444)));
    h.get_by_label("Color de Tareas").click();
    settle(&mut h);
    h.get_by_label("Sin color").click();
    settle(&mut h);
    assert_eq!(h.state().session.prefs.today_layout.tint(TodayCard::Tasks), None);
}

/// S1.36: noticias en la cabecera: de cada fuente su último artículo (▶ si tiene audio); ▶ pide el
/// audio (el ejemplo no lo descarga y lo dice); en «Fuentes de noticias» se eligen 1 a 3 por
/// fuente, se quita, agrega y restaura.
#[test]
fn news_with_play_and_sources() {
    use iuredesk_engine::news::{Topic, default_sources};
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    let play = "Escuchar: Resumen de noticias – miércoles, 21 de octubre";
    assert_eq!(h.query_all_by_label(play).count(), 2, "un ▶ por fuente");
    let other = "Manejar la casa desde una sola pantalla";
    assert!(h.query_by_label(other).is_none(), "por omisión, solo el último de cada fuente");
    assert!(h.query_by_label_contains("Noticias generales").is_none(), "solo el título, sin la leyenda del tema");
    h.query_all_by_label(play).next().unwrap().click();
    settle(&mut h);
    assert_eq!(h.state().news_error().as_deref(), Some("El buzón de ejemplo no descarga audio."));
    assert!(h.query_by_label_contains("no descarga audio").is_some(), "el error se ve junto a la noticia");
    // El comando de la paleta hace lo mismo con la primera noticia.
    h.state_mut().session.run("news.play_pause").unwrap();

    // Dos por fuente: aparece el otro artículo, sin ▶ (no tiene audio).
    h.get_by_label("Fuentes de noticias…").click();
    settle(&mut h);
    h.get_by_label("2 por fuente").click();
    settle(&mut h);
    assert_eq!(h.state().session.prefs.news.per_source, 2);
    assert!(h.query_by_label(other).is_some());
    assert!(h.query_by_label(&format!("Escuchar: {other}")).is_none());

    // El orden: por omisión, los artículos de Iurefficient, SesoLibre y el blog. Subir SesoLibre
    // la pone primero, en las preferencias y en la pantalla.
    let order = ["Iurefficient", "SesoLibre", "Blog Iurefficient"];
    assert_eq!(h.state().today_news_order(), order);
    h.get_by_label("Subir Iurefficient").click();
    settle(&mut h);
    assert_eq!(h.state().today_news_order(), order, "la primera no sube");
    h.get_by_label("Subir SesoLibre").click();
    settle(&mut h);
    let names: Vec<_> = h.state().session.prefs.news.sources.iter().map(|s| s.name.clone()).collect();
    assert_eq!(names, ["SesoLibre", "Iurefficient", "Blog Iurefficient"]);
    assert_eq!(h.state().today_news_order(), names, "se reacomoda sin volver a pedir");
    h.get_by_label("Bajar SesoLibre").click();
    settle(&mut h);
    assert_eq!(h.state().today_news_order(), order);

    // Fuentes: quitar SesoLibre, una dirección inválida, una válida y restaurar.
    h.get_by_label("Quitar SesoLibre").click();
    settle(&mut h);
    assert_eq!(h.state().session.prefs.news.sources.len(), 2);
    h.get_by_label("Dirección de la fuente").type_text("ftp://x");
    h.get_by_label("Agregar").click();
    settle(&mut h);
    assert!(h.query_by_label_contains("empezando por https://").is_some());
    // La dirección inválida se queda para corregirla: se selecciona y se reemplaza.
    h.get_by_label("Dirección de la fuente").click();
    h.run_steps(1);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run_steps(1);
    h.get_by_label("Dirección de la fuente").type_text("https://www.example.org/feed/");
    h.get_by_label("Agregar").click();
    settle(&mut h);
    let s = h.state().session.prefs.news.sources.clone();
    assert_eq!(s.len(), 3);
    assert_eq!((s[2].name.as_str(), s[2].topic), ("example.org", Topic::Other));
    h.get_by_label("Restaurar las de omisión").click();
    settle(&mut h);
    assert_eq!(h.state().session.prefs.news.sources, default_sources());
    h.get_by_label("Mostrar noticias").click();
    h.get_by_label("Listo").click();
    settle(&mut h);
    assert!(!h.state().session.prefs.news.enabled);
    assert_eq!(h.query_all_by_label(play).count(), 0, "apagadas no se ven");
}

/// S1.37: con Iurefficient, la tarjeta «Proyectos» muestra tus casos (los de tus tareas
/// pendientes) con sus cifras; picar uno lleva a Tareas → «Por caso»; «IureProject», sin plan
/// seguido, abre el expediente (`--iurefficient-case`). Sin IureProject, los casos se ven igual.
#[test]
fn projects_card_shows_your_cases() {
    use iuredesk_engine::iuredesk_integrations::{SisterApp, Suite};
    use iuredesk_engine::tasks_view::TaskView;
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let launches = Arc::new(Launches::default());
    let mut a = app(dir.path());
    a.set_suite(
        Suite::empty().with_app(SisterApp::Project, "/opt/iureproject/iureproject").with_opener(launches.clone()),
    );
    let mut h = harness(a);
    settle(&mut h);
    assert!(h.query_by_label("2 pendientes · 1 vencida").is_some(), "el caso con vencidas, primero");
    assert!(h.query_by_label("2 pendientes").is_some());
    // «IureProject» del primer caso (sin plan seguido): el expediente.
    h.get_by_label("Amparo Ruiz Mendoza").scroll_to_me();
    settle(&mut h);
    h.query_all_by_label("IureProject").next().unwrap().click();
    settle(&mut h);
    let start = Instant::now();
    while launches.0.lock().unwrap().is_empty() && start.elapsed() < Duration::from_secs(10) {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(20));
    }
    let l = launches.0.lock().unwrap().join("\n");
    assert!(l.contains("--iurefficient-case") && l.contains("--case-title Amparo Ruiz Mendoza"), "{l}");
    // Picar el caso: Tareas → «Por caso».
    h.get_by_label("Amparo Ruiz Mendoza").click();
    settle(&mut h);
    assert_eq!(h.state().session.module, Module::Tasks);
    assert_eq!(h.state().session.task_view(), TaskView::ByCase);
    assert!(h.query_all_by_label("Por caso").next().is_some(), "la vista «Por caso»");

    // Sin IureProject: los casos se ven, sin botón.
    let dir2 = tempfile::tempdir().unwrap();
    let mut a = app(dir2.path());
    a.set_suite(Suite::empty());
    let mut h = harness(a);
    settle(&mut h);
    assert!(h.query_by_label("2 pendientes · 1 vencida").is_some());
    assert!(h.query_by_label("IureProject").is_none());
}

/// UX-29: el símbolo de Iurefficient al inicio de cada caso de «Proyectos» abre su página en la
/// web de la instancia.
#[test]
fn projects_card_opens_a_case_on_the_web() {
    use iuredesk_engine::iuredesk_integrations::Suite;
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    iuredesk_ui::capture_opened_urls();
    let dir = tempfile::tempdir().unwrap();
    let mut a = app(dir.path());
    a.set_suite(Suite::empty());
    let mut h = harness(a);
    settle(&mut h);
    let n = h.query_all_by_label("Abrir en Iurefficient (web)").count();
    assert!(n >= 2, "un símbolo por caso: {n}");
    h.get_by_label("Amparo Ruiz Mendoza").scroll_to_me();
    settle(&mut h);
    h.query_all_by_label("Abrir en Iurefficient (web)").next().unwrap().click();
    settle(&mut h);
    let urls = iuredesk_ui::opened_urls();
    assert!(
        urls.iter().any(|u| u.starts_with("https://demo.iurefficient.com/dashboard/cases/")),
        "la página del caso: {urls:?}"
    );
}
