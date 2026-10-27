//! Captura sin pantalla de IureDesk con el buzón de ejemplo y un «ahora» fijo (determinista).
//!
//! `cargo run -p iuredesk-ui --example ui_shot -- /tmp/shot.png [--dark] [--density dense|compact|comfortable]
//!  [--module mail|calendar|contacts|notes|tasks|assistant|onboarding|provider] [--palette[=texto]]
//!  [--html] [--month] [--compose] [--lang en|es] [--size 1440x900] [--ppp 1]`

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use egui::vec2;
use egui_kittest::Harness;
use iuredesk_accounts::MemoryStore;
use iuredesk_engine::{DensityPref, Module, Session, SessionConfig, ThemePref, Zone};
use iuredesk_store::{Store, sample};
use iuredesk_ui::{DeskApp, Options};

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
            .or_else(|| args.iter().find_map(|a| a.strip_prefix(&format!("{name}="))).map(str::to_string))
    };
    let out = args
        .iter()
        .find(|a| a.ends_with(".png"))
        .cloned()
        .ok_or("uso: ui_shot RUTA.png [--dark] [--density …] [--module …] [--palette]")?;
    let module = flag("--module").unwrap_or_else(|| "mail".into());
    let dark = args.iter().any(|a| a == "--dark");
    let density = flag("--density").and_then(|d| DensityPref::from_key(&d)).unwrap_or(DensityPref::Compact);
    if let Some(l) = flag("--lang") {
        iuredesk_ui::iurefficient_ui::lang::set_lang(iuredesk_ui::iurefficient_ui::lang::from_tag(&l));
        iuredesk_engine::lang::set_lang(iuredesk_engine::lang::from_tag(&l));
    } else {
        iuredesk_ui::iurefficient_ui::lang::set_lang(iuredesk_ui::iurefficient_ui::lang::Lang::Es);
        iuredesk_engine::lang::set_lang(iuredesk_engine::lang::Lang::Es);
    }
    let (w, h) = flag("--size")
        .and_then(|s| s.split_once('x').and_then(|(a, b)| Some((a.parse::<f32>().ok()?, b.parse::<f32>().ok()?))))
        .unwrap_or((1440.0, 900.0));
    let ppp = flag("--ppp").and_then(|p| p.parse::<f32>().ok()).unwrap_or(1.0).clamp(0.5, 3.0);

    // Miércoles 21 de octubre de 2026, 11:20 UTC.
    let now = Utc.with_ymd_and_hms(2026, 10, 21, 11, 20, 0).single().ok_or("fecha")?;
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let store = Store::open(dir.path()).map_err(|e| e.to_string())?;
    let onboarding = module == "onboarding" || module == "provider";
    if !onboarding {
        sample::populate(&store, sample::DEFAULT_SEED, now).map_err(|e| e.to_string())?;
    }
    let mut session = Session::new(SessionConfig {
        store: Arc::new(store),
        accounts: None,
        secrets: Arc::new(MemoryStore::default()),
        prefs_path: None,
        now: Some(now),
        zone: Zone::utc(),
        sample: true,
        sync: None,
        transport: Some(Arc::new(iuredesk_engine::OfflineTransport)),
    })
    .map_err(|e| e.text().en)?;
    session.prefs.theme = if dark { ThemePref::Dark } else { ThemePref::Light };
    session.prefs.density = density;
    session.module = match module.as_str() {
        "calendar" => Module::Calendar,
        "contacts" => Module::Contacts,
        "notes" => Module::Notes,
        "tasks" => Module::Tasks,
        "assistant" => Module::Assistant,
        _ => Module::Mail,
    };
    // Esperar a que la lista esté lista para elegir el mensaje que se muestra.
    let start = Instant::now();
    while !session.is_idle() && start.elapsed() < Duration::from_secs(30) {
        session.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
    session.poll();
    if session.module == Module::Mail && !onboarding {
        let html = args.iter().any(|a| a == "--html");
        let rows: Vec<_> = session.rows().iter().take(400).copied().collect();
        let pick = rows
            .iter()
            .find(|r| {
                session.envelope(r.id).is_some_and(|e| {
                    if html { e.subject.starts_with("Boletín") } else { e.subject.contains("241/2026") && r.count > 1 }
                })
            })
            .or(rows.get(2))
            .map(|r| r.id);
        if let Some(id) = pick {
            session.request_focus(id);
        }
    }
    let mut app = DeskApp::new(session, Options { system_fonts: true, no_theme_watch: true, ..Default::default() });
    if args.iter().any(|a| a == "--compose") {
        app.demo_compose();
    }
    if module == "provider" {
        app.demo_provider_step("laura.mendez@gmail.com");
    } else if module == "onboarding" {
        app.demo_email_step("laura@despacho.example.org", "Laura Méndez");
    }
    if args.iter().any(|a| a == "--month") {
        app.set_calendar_month();
    }
    if let Some(q) = args.iter().find_map(|a| a.strip_prefix("--palette")) {
        app.palette.open_with(q.strip_prefix('=').unwrap_or(""));
    }

    let mut harness = Harness::builder()
        .with_size(vec2(w, h))
        .with_pixels_per_point(ppp)
        .with_accessibility_check(false)
        .allow_missing_glyphs()
        .build_ui_state(
            |ui, app: &mut DeskApp| {
                let full = ui.ctx().content_rect();
                ui.scope_builder(egui::UiBuilder::new().max_rect(full), |ui| {
                    ui.set_clip_rect(full);
                    app.ui(ui);
                });
            },
            app,
        );
    // Hasta que no quede trabajo de fondo (cuerpos, conversaciones, HTML).
    let start = Instant::now();
    let mut quiet = 0;
    while start.elapsed() < Duration::from_secs(90) {
        harness.run_steps(1);
        if harness.state().is_settled() {
            quiet += 1;
            if quiet > 6 {
                break;
            }
        } else {
            quiet = 0;
            std::thread::sleep(Duration::from_millis(15));
        }
    }
    harness.run_steps(2);
    let image = harness.render()?;
    image.save(&out).map_err(|e| e.to_string())?;
    println!("{out}");
    Ok(())
}
