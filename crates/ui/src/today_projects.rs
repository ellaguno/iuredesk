//! S1.6: tarjeta «Proyectos» de «Mi día»: los planes de IureProject que el usuario sigue, con su
//! avance, lo atrasado y el próximo hito, leídos por MCP (`project_overview`) en segundo plano y
//! sin abrir IureProject. Pulsar un plan lo abre en IureProject. Solo con IureProject instalado.
//!
//! Los planes seguidos viven en `Prefs::projects`; se eligen aquí o en Ajustes → Suite. El
//! resumen se vuelve a pedir cada [`REFRESH`] o cuando cambia la lista.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use egui::{Align, Layout, RichText, Ui, vec2};
use iuredesk_engine::iuredesk_integrations::{ProjectStatus, ProjectTask, SisterApp, project::MAX_PROJECTS};
use iuredesk_engine::suite::SuiteAction;
use iurefficient_ui::{Icon, pick, theme};

use crate::app::DeskApp;

/// Cada cuánto se vuelve a leer el estado de los planes.
pub const REFRESH: Duration = Duration::from_secs(10 * 60);
/// Una lectura que no contesta se da por perdida tras esto (IureProject no arrancó, etc.).
const GIVE_UP: Duration = Duration::from_secs(90);
/// Tareas atrasadas por plan en la tarjeta.
const LATE_ROWS: usize = 3;

/// Estado de la tarjeta.
#[derive(Default)]
pub struct ProjectsUi {
    data: Vec<ProjectStatus>,
    /// Lista de planes para la que se pidió (o se tiene) el estado.
    key: Vec<String>,
    fetched: Option<Instant>,
    asked: Option<Instant>,
    /// Plan elegido en el diálogo (en otro hilo).
    picking: Option<Receiver<Option<PathBuf>>>,
}

/// Llegó el estado de los planes.
pub fn on_projects(app: &mut DeskApp, v: Vec<ProjectStatus>) {
    let p = &mut app.today.projects;
    p.data = v;
    p.fetched = Some(Instant::now());
    p.asked = None;
}

fn installed(app: &DeskApp) -> bool {
    app.suite.hub.installed(SisterApp::Project)
}

/// Pide el estado si hace falta (lista nueva, nunca leído o viejo). También desde el calendario
/// (los hitos se ven ahí).
pub(crate) fn ensure(app: &mut DeskApp) {
    let files = app.session.prefs.projects.clone();
    let p = &mut app.today.projects;
    if files.is_empty() || !installed_flag(&app.suite) {
        return;
    }
    if p.asked.is_some_and(|t| t.elapsed() < GIVE_UP) {
        return;
    }
    let stale = p.fetched.is_none_or(|t| t.elapsed() >= REFRESH);
    if p.key != files || stale {
        p.key = files.clone();
        p.asked = Some(Instant::now());
        app.suite.hub.run(SuiteAction::ProjectOverviews(files.into_iter().map(PathBuf::from).collect()));
    }
}

fn installed_flag(suite: &crate::suite_ui::SuiteUi) -> bool {
    suite.hub.installed(SisterApp::Project)
}

/// Abre el diálogo para elegir un plan (en otro hilo; el resultado se recoge cada cuadro).
pub fn pick_plan(app: &mut DeskApp, ctx: &egui::Context) {
    let (tx, rx) = std::sync::mpsc::channel();
    let ctx = ctx.clone();
    let spawned = std::thread::Builder::new().name("project-pick".into()).spawn(move || {
        let f = rfd::FileDialog::new()
            .set_title(pick("Follow an IureProject plan", "Seguir un plan de IureProject"))
            .add_filter("IureProject", &["iurp", "mpp", "xml", "mpx"])
            .pick_file();
        let _ = tx.send(f);
        ctx.request_repaint();
    });
    if spawned.is_ok() {
        app.today.projects.picking = Some(rx);
    }
}

/// Recoge el plan elegido, si llegó.
pub fn poll_pick(app: &mut DeskApp) {
    let Some(rx) = &app.today.projects.picking else { return };
    let Ok(choice) = rx.try_recv() else { return };
    app.today.projects.picking = None;
    if let Some(path) = choice {
        follow(app, &path.to_string_lossy());
    }
}

/// Sigue un plan (sin repetir; como mucho [`MAX_PROJECTS`]).
pub fn follow(app: &mut DeskApp, path: &str) {
    let list = &mut app.session.prefs.projects;
    if list.iter().any(|p| p == path) {
        return;
    }
    if list.len() >= MAX_PROJECTS {
        app.notice(iuredesk_engine::Notice::info(
            format!("You can follow up to {MAX_PROJECTS} plans."),
            format!("Puedes seguir hasta {MAX_PROJECTS} planes."),
        ));
        return;
    }
    list.push(path.to_string());
    app.session.save_prefs();
}

/// Deja de seguir un plan.
pub fn unfollow(app: &mut DeskApp, path: &str) {
    app.session.prefs.projects.retain(|p| p != path);
    app.today.projects.data.retain(|s| s.file.to_string_lossy() != path);
    app.session.save_prefs();
}

/// Los planes seguidos que pertenecen al expediente `case_id` de Iurefficient (IureProject lo
/// guarda en el plan): archivo y una línea de estado («Caso Ramírez · 43 % · 1 atrasada · próximo
/// hito: Audiencia, 30 nov»), para la tarjeta del caso en el panel de contexto.
pub(crate) fn plans_for_case(app: &DeskApp, case_id: &str) -> Vec<(PathBuf, String)> {
    if case_id.is_empty() || !installed(app) {
        return Vec::new();
    }
    app.today
        .projects
        .data
        .iter()
        .filter_map(|s| {
            let o = s.overview.as_ref().ok()?;
            (o.case.as_ref()?.id == case_id).then(|| (s.file.clone(), status_line(o)))
        })
        .take(3)
        .collect()
}

/// «Caso Ramírez · 43 % · 1 atrasada · próximo hito: Audiencia, 30 nov».
fn status_line(o: &iuredesk_engine::iuredesk_integrations::ProjectOverview) -> String {
    let mut line = format!("{} · {:.0} %", o.name, o.percent_complete);
    if o.late > 0 {
        line.push_str(&format!(" · {} {}", o.late, pick("late", "atrasadas")));
    }
    if let Some(m) = o.upcoming_milestones.first() {
        let when = m.finish.as_deref().map(short_date).unwrap_or_default();
        line.push_str(&format!(
            " · {} {}{}",
            pick("next milestone:", "próximo hito:"),
            m.name,
            if when.is_empty() { String::new() } else { format!(", {when}") }
        ));
    }
    line
}

/// Calendario ficticio de los hitos de IureProject (no existe: nadie lo edita ni lo arrastra).
pub(crate) const MILESTONES_CALENDAR: iuredesk_model::CalendarId = iuredesk_model::CalendarId(u64::MAX);
/// Ids de los hitos: arriba de cualquier evento real.
const MILESTONE_ID: u64 = 0xFFFF_0000_0000_0000;
/// Color de los hitos en el calendario (violeta, distinto de los calendarios).
const MILESTONE_COLOR: u32 = 0x7c4dff;

/// Los hitos próximos de los planes que se siguen, como días completos de solo lectura en el
/// calendario («Hito · Audiencia — Caso Ramírez»). Pulsar uno abre el plan en IureProject.
pub(crate) fn with_milestones(
    app: &DeskApp,
    mut occ: Vec<iuredesk_engine::calendar::Occurrence>,
    from: chrono::DateTime<chrono::Utc>,
    to: chrono::DateTime<chrono::Utc>,
) -> Vec<iuredesk_engine::calendar::Occurrence> {
    if !installed(app) {
        return occ;
    }
    let zone = app.session.zone();
    for (pi, st) in app.today.projects.data.iter().enumerate().take(MAX_PROJECTS) {
        let Ok(o) = &st.overview else { continue };
        for (mi, m) in o.upcoming_milestones.iter().enumerate().take(64) {
            let Some(day) =
                m.finish.as_deref().and_then(|f| f.get(..10)).and_then(|d| d.parse::<chrono::NaiveDate>().ok())
            else {
                continue;
            };
            let Some(next) = day.succ_opt() else { continue };
            let (start, end) = (zone.day_start(day), zone.day_start(next));
            if end <= from || start >= to {
                continue;
            }
            occ.push(iuredesk_engine::calendar::Occurrence {
                id: iuredesk_model::EventId(MILESTONE_ID | ((pi as u64) << 16) | mi as u64),
                summary: format!("{} · {} — {}", pick("Milestone", "Hito"), m.name, o.name),
                location: String::new(),
                start,
                end,
                all_day: true,
                days: Some((day, next)),
                color: MILESTONE_COLOR,
                tentative: false,
                recurring: false,
                calendar: MILESTONES_CALENDAR,
                recurrence_id: None,
                busy: false,
                cancelled: false,
            });
        }
    }
    occ
}

/// El plan de un hito del calendario (si `o` es uno).
pub(crate) fn milestone_plan(app: &DeskApp, o: &iuredesk_engine::calendar::Occurrence) -> Option<PathBuf> {
    if o.calendar != MILESTONES_CALENDAR {
        return None;
    }
    let pi = usize::try_from((o.id.0 >> 16) & 0xFFFF).ok()?;
    app.today.projects.data.get(pi).map(|s| s.file.clone())
}

/// `AAAA-MM-DD…` → «10 nov».
fn short_date(iso: &str) -> String {
    let months = [
        pick("Jan", "ene"),
        pick("Feb", "feb"),
        pick("Mar", "mar"),
        pick("Apr", "abr"),
        pick("May", "may"),
        pick("Jun", "jun"),
        pick("Jul", "jul"),
        pick("Aug", "ago"),
        pick("Sep", "sep"),
        pick("Oct", "oct"),
        pick("Nov", "nov"),
        pick("Dec", "dic"),
    ];
    let m: Option<usize> = iso.get(5..7).and_then(|m| m.parse().ok());
    let d: Option<u32> = iso.get(8..10).and_then(|d| d.parse().ok());
    match (m.and_then(|m| months.get(m.wrapping_sub(1))), d) {
        (Some(mon), Some(d)) => format!("{d} {mon}"),
        _ => String::new(),
    }
}

/// Qué se pulsó en la tarjeta.
enum Act {
    Open(PathBuf),
    Pick,
    Launch,
}

/// La tarjeta. Sin IureProject instalado no se dibuja nada.
pub fn card(app: &mut DeskApp, ui: &mut Ui) {
    if !installed(app) {
        return;
    }
    poll_pick(app);
    ensure(app);
    let late: usize = app.today.projects.data.iter().filter_map(|s| s.overview.as_ref().ok()).map(|o| o.late).sum();
    let detail = if late > 0 { format!("{late} {}", pick("late", "atrasadas")) } else { String::new() };
    let mut act = None;
    let follows = !app.session.prefs.projects.is_empty();
    let action = follows.then(|| pick("Open IureProject", "Abrir IureProject"));
    let es = iuredesk_engine::lang::spanish();
    let waiting = app.today.projects.data.is_empty() && app.today.projects.asked.is_some();
    let data = app.today.projects.data.clone();
    let clicked = crate::today::card(ui, Icon::Gantt, pick("Projects", "Proyectos"), &detail, action, |ui| {
        let c = theme::colors(ui);
        if !follows {
            crate::today::muted(
                ui,
                pick(
                    "Follow an IureProject plan to see its progress and what's late here.",
                    "Sigue un plan de IureProject para ver aquí su avance y lo atrasado.",
                ),
            );
            ui.add_space(6.0);
            if ui.button(pick("Follow a plan…", "Seguir un plan…")).clicked() {
                act = Some(Act::Pick);
            }
            return;
        }
        if waiting {
            crate::today::muted(ui, pick("Reading the plans…", "Leyendo los planes…"));
            return;
        }
        for s in &data {
            match &s.overview {
                Ok(o) => {
                    let label = if o.name.trim().is_empty() {
                        s.file.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
                    } else {
                        o.name.clone()
                    };
                    ui.horizontal(|ui| {
                        let name = ui
                            .add(egui::Button::new(RichText::new(&label).strong().color(c.text)).frame(false))
                            .on_hover_text(pick("Open this plan in IureProject", "Abrir este plan en IureProject"));
                        if name.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if name.clicked() {
                            act = Some(Act::Open(s.file.clone()));
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(RichText::new(format!("{:.0} %", o.percent_complete)).color(c.text_muted));
                        });
                    });
                    let frac = (o.percent_complete / 100.0) as f32;
                    ui.add(
                        egui::ProgressBar::new(frac.clamp(0.0, 1.0))
                            .desired_height(6.0)
                            .desired_width(ui.available_width()),
                    );
                    for t in o.late_tasks.iter().take(LATE_ROWS) {
                        task_line(ui, t, true);
                    }
                    if o.late > LATE_ROWS {
                        crate::today::muted(
                            ui,
                            &format!("+{} {}", o.late - LATE_ROWS, pick("more late", "atrasadas más")),
                        );
                    }
                    if let Some(m) = o.upcoming_milestones.first() {
                        task_line(ui, m, false);
                    }
                }
                Err(t) => {
                    let name = s.file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    ui.label(RichText::new(name).strong().color(c.text));
                    ui.label(RichText::new(t.get(es)).color(c.danger).size(theme::density(ui.ctx()).small_size()));
                }
            }
            ui.add_space(8.0);
        }
    });
    if clicked {
        act = Some(Act::Launch);
    }
    match act {
        Some(Act::Open(p)) => app.suite.hub.run(SuiteAction::OpenProject(p)),
        Some(Act::Pick) => pick_plan(app, ui.ctx()),
        Some(Act::Launch) => app.suite.hub.run(SuiteAction::Launch(SisterApp::Project)),
        None => {}
    }
}

/// Una tarea atrasada (en rojo, con la fecha en que vencía) o el próximo hito.
fn task_line(ui: &mut Ui, t: &ProjectTask, late: bool) {
    let c = theme::colors(ui);
    let when = t.finish.as_deref().map(short_date).unwrap_or_default();
    let (mark, col, text) = if late {
        ("⚠", c.danger, format!("{} — {} {when}", t.name, pick("was due", "vencía")))
    } else {
        ("◆", c.accent, format!("{} — {when}", t.name))
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.add_sized(vec2(14.0, 16.0), egui::Label::new(RichText::new(mark).color(col)));
        ui.label(RichText::new(text).color(if late { col } else { c.text }));
    });
}

/// Ajustes → Suite: los planes que se siguen.
pub fn settings(app: &mut DeskApp, ui: &mut Ui) {
    if !installed(app) {
        return;
    }
    poll_pick(app);
    let c = theme::colors(ui);
    ui.add_space(8.0);
    ui.label(RichText::new(pick("Plans you follow in Today", "Planes que sigues en Hoy")).strong().color(c.text));
    let mut remove = None;
    for p in app.session.prefs.projects.clone() {
        ui.horizontal(|ui| {
            ui.label(RichText::new(&p).color(c.text_muted));
            if ui.small_button(pick("Stop following", "Dejar de seguir")).clicked() {
                remove = Some(p.clone());
            }
        });
    }
    if let Some(p) = remove {
        unfollow(app, &p);
    }
    if ui.button(pick("Follow a plan…", "Seguir un plan…")).clicked() {
        pick_plan(app, ui.ctx());
    }
}

/// Pruebas: el estado de los planes como si hubiera llegado de IureProject.
pub fn set_for_tests(app: &mut DeskApp, v: Vec<ProjectStatus>) {
    app.today.projects.key = app.session.prefs.projects.clone();
    on_projects(app, v);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_dates() {
        iurefficient_ui::lang::set_lang(iurefficient_ui::lang::Lang::Es);
        assert_eq!(short_date("2026-11-10T17:00:00"), "10 nov");
        assert_eq!(short_date("2026-13-10"), "");
        assert_eq!(short_date("x"), "");
    }
}
