//! R4.1: accesibilidad. Cada pantalla se recorre en el árbol de AccessKit (lo que leen Orca, NVDA
//! y VoiceOver) y todo lo que se puede pulsar o escribir debe tener nombre. Además, contraste de
//! las combinaciones de colores que la app usa más allá de las del ui-kit (texto de error sobre
//! las superficies, botón de peligro, anillo de foco).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use egui::Color32;
use egui::accesskit::Role;
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use iuredesk_accounts::MemoryStore;
use iuredesk_engine::{OfflineTransport, Session, SessionConfig, ThemePref, Zone};
use iuredesk_store::{Store, sample};
use iuredesk_ui::iurefficient_ui::tokens;
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
    DeskApp::new(s, Options { system_fonts: false, no_theme_watch: true, ..Default::default() })
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
    h.run();
}

/// Papeles que el usuario acciona.
const INTERACTIVE: [Role; 12] = [
    Role::Button,
    Role::CheckBox,
    Role::RadioButton,
    Role::Switch,
    Role::Link,
    Role::TextInput,
    Role::MultilineTextInput,
    Role::SearchInput,
    Role::ComboBox,
    Role::Slider,
    Role::Tab,
    Role::MenuItem,
];

/// Lo interactivo sin nombre en la pantalla actual (rol y un poco de contexto).
fn unnamed(h: &Harness<'_, DeskApp>) -> Vec<String> {
    h.query_all_by(|n| {
        INTERACTIVE.contains(&n.role())
            && !n.is_hidden()
            && n.label().is_none_or(|l| l.trim().is_empty())
            && n.value().is_none_or(|v| v.trim().is_empty())
            && n.description().is_none_or(|d| d.trim().is_empty())
    })
    .map(|n| {
        format!(
            "{:?} sin nombre (padre: {:?})",
            n.accesskit_node().role(),
            n.parent().map(|p| p.accesskit_node().label())
        )
    })
    .collect()
}

#[test]
fn every_interactive_element_has_a_name() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    let mut problems = Vec::new();
    for (screen, cmd) in [
        ("Mi día", "nav.today"),
        ("Correo", "nav.mail"),
        ("Calendario", "nav.calendar"),
        ("Contactos", "nav.contacts"),
        ("Tareas", "nav.tasks"),
    ] {
        let _ = h.state_mut().session.run(cmd);
        settle(&mut h);
        problems.extend(unnamed(&h).into_iter().map(|p| format!("{screen}: {p}")));
    }
    // Lectura de un mensaje.
    let _ = h.state_mut().session.run("nav.mail");
    let first = h.state().session.rows().first().map(|r| r.id);
    if let Some(id) = first {
        h.state_mut().session.request_focus(id);
    }
    settle(&mut h);
    problems.extend(unnamed(&h).into_iter().map(|p| format!("Lectura: {p}")));
    // Redactor.
    h.get_by_label("Nuevo mensaje").click();
    settle(&mut h);
    problems.extend(unnamed(&h).into_iter().map(|p| format!("Redactor: {p}")));
    // La paleta de comandos (`iurefficient-ui`) se revisa cuando el ui-kit publique el nombre de
    // su campo (pendiente de R4.1 en `docs/gaps.md`).
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    // Ajustes (cada sección).
    h.get_by_label("Ajustes").click();
    settle(&mut h);
    for section in ["General", "Apariencia", "Correo", "Privacidad", "Reglas", "Etiquetas", "Integraciones", "Avanzado"]
    {
        if let Some(tab) = h.query_all_by_label(section).last() {
            tab.click();
        }
        settle(&mut h);
        problems.extend(unnamed(&h).into_iter().map(|p| format!("Ajustes → {section}: {p}")));
    }
    assert!(problems.is_empty(), "elementos sin nombre para el lector de pantalla:\n{}", problems.join("\n"));
}

/// Contraste WCAG 2 de `fg` sobre `bg` (opacos).
fn ratio(fg: Color32, bg: Color32) -> f32 {
    fn lum(c: Color32) -> f32 {
        let ch = |v: u8| {
            let s = f32::from(v) / 255.0;
            if s <= 0.039_28 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
    }
    let (x, y) = (lum(fg), lum(bg));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

#[test]
fn app_specific_color_pairs_meet_wcag_aa() {
    let mut low = Vec::new();
    for (name, p) in [("claro", tokens::LIGHT), ("oscuro", tokens::DARK), ("alto contraste", tokens::HIGH_CONTRAST)] {
        // Texto de error y de enlaces escrito directamente sobre las superficies: 4.5:1.
        for (what, fg) in [("danger", p.danger), ("accent (enlaces)", p.accent)] {
            for (on, bg) in [("surface", p.surface), ("bg", p.bg), ("surface_alt", p.surface_alt)] {
                let r = ratio(fg, bg);
                if r < 4.5 {
                    low.push(format!("{name}: texto {what} sobre {on}: {r:.2}"));
                }
            }
        }
        // Botón de peligro (confirmar «Eliminar»): texto on_accent sobre danger.
        let r = ratio(p.on_accent, p.danger);
        if r < 4.5 {
            low.push(format!("{name}: on_accent sobre danger: {r:.2}"));
        }
        // Anillo de foco del teclado (WCAG 2.4.11/1.4.11): 3:1 contra lo que rodea.
        for (on, bg) in [("surface", p.surface), ("bg", p.bg), ("selection", p.selection)] {
            let r = ratio(p.focus, bg);
            if r < 3.0 {
                low.push(format!("{name}: foco sobre {on}: {r:.2}"));
            }
        }
    }
    assert!(low.is_empty(), "contraste bajo:\n{}", low.join("\n"));
}
