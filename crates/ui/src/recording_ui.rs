//! S1.4: grabar una reunión sin abrir IureTranscribe.
//!
//! - Se empieza con «● Grabar» en la próxima cita de Mi día o con el comando `suite.record`
//!   (paleta): el título es el de la cita en curso o la próxima, o «Reunión» y la hora.
//! - Mientras se graba, una píldora siempre visible arriba («● Grabando · Comité · 12:04») con
//!   pausar/seguir y detener. Grabar sin indicador sería un problema legal y de confianza.
//! - Al detener, IureTranscribe guarda el audio y lo transcribe; la transcripción llega como una
//!   **nota de minuta** (si hay Notas) con la ruta del audio; si no, va al asistente.

use egui::{Align2, Context, RichText, vec2};
use iuredesk_engine::Notice;
use iuredesk_engine::iuredesk_integrations::SisterApp;
use iuredesk_engine::suite::{RecordingState, SuiteAction};
use iurefficient_ui::{pick, theme};

use crate::app::DeskApp;

/// ¿Se puede grabar? (IureTranscribe instalado y nada grabándose).
pub fn can_record(app: &DeskApp) -> bool {
    app.suite.hub.installed(SisterApp::Transcribe) && app.suite.hub.recording().is_none()
}

/// Empieza a grabar con este título.
pub fn start(app: &mut DeskApp, title: &str) {
    let title = if title.trim().is_empty() { default_title(app) } else { title.trim().to_string() };
    app.suite.hub.run(SuiteAction::StartRecording { title });
}

/// `suite.record`: empieza o detiene.
pub fn toggle(app: &mut DeskApp) {
    if app.suite.hub.recording().is_some() {
        app.suite.hub.run(SuiteAction::StopRecording);
    } else {
        let title = default_title(app);
        start(app, &title);
    }
}

/// La cita en curso o la que empieza en los próximos 15 minutos; si no, «Reunión HH:MM».
fn default_title(app: &mut DeskApp) -> String {
    let now = app.session.now();
    let soon = now + chrono::Duration::minutes(15);
    let found = app
        .session
        .day_summary()
        .and_then(|s| s.events.iter().find(|e| e.occ.start <= soon && e.occ.end >= now).map(|e| e.occ.summary.clone()));
    found.filter(|t| !t.trim().is_empty()).unwrap_or_else(|| {
        let local = app.session.zone().local(now);
        format!("{} {}", pick("Meeting", "Reunión"), local.format("%H:%M"))
    })
}

fn clock(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

/// La píldora de arriba mientras se graba (cada cuadro).
pub fn pill(app: &mut DeskApp, ctx: &Context) {
    let Some(st) = app.suite.hub.recording() else { return };
    let mut act = None;
    egui::Area::new(egui::Id::unique("s1-4-recording-pill"))
        .order(egui::Order::Foreground)
        .anchor(Align2::CENTER_TOP, vec2(0.0, 44.0))
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).corner_radius(20.0).inner_margin(egui::Margin::symmetric(14, 6)).show(
                ui,
                |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        act = pill_body(ui, &st);
                    });
                },
            );
        });
    // Mientras se graba, el reloj avanza.
    ctx.request_repaint_after(std::time::Duration::from_millis(500));
    match act {
        Some(PillAct::Pause(p)) => app.suite.hub.run(SuiteAction::PauseRecording(p)),
        Some(PillAct::Stop) => app.suite.hub.run(SuiteAction::StopRecording),
        None => {}
    }
}

enum PillAct {
    Pause(bool),
    Stop,
}

fn pill_body(ui: &mut egui::Ui, st: &RecordingState) -> Option<PillAct> {
    let c = theme::colors(ui);
    let mut act = None;
    let dot = if st.paused { c.text_muted } else { c.danger };
    ui.label(RichText::new("●").color(dot));
    let state = if st.stopping {
        pick("Saving the recording…", "Guardando la grabación…")
    } else if st.paused {
        pick("Paused", "En pausa")
    } else {
        pick("Recording", "Grabando")
    };
    ui.label(RichText::new(format!("{state} · {} · {}", st.title, clock(st.elapsed_secs))).strong().color(c.text));
    if !st.stopping {
        // Nivel de entrada: que se vea que el micrófono oye.
        let level = st.mic_level.max(st.sys_level).clamp(0.0, 1.0);
        ui.add(egui::ProgressBar::new(level).desired_width(48.0).desired_height(6.0));
        let pause = if st.paused { pick("Resume", "Seguir") } else { pick("Pause", "Pausar") };
        if ui.small_button(pause).clicked() {
            act = Some(PillAct::Pause(!st.paused));
        }
        if ui.small_button(pick("Stop", "Detener")).clicked() {
            act = Some(PillAct::Stop);
        }
    }
    if let Some(e) = &st.error {
        ui.label(RichText::new(e).small().color(c.warning));
    }
    act
}

/// La transcripción de la reunión: como nota de minuta (o, sin Notas, al asistente).
pub fn on_transcript(app: &mut DeskApp, title: &str, text: &str, audio: &std::path::Path) {
    let note_title = format!("{} — {title}", pick("Minutes", "Minuta"));
    let body = format!(
        "{}: `{}`\n\n## {}\n\n{}\n",
        pick("Recording", "Grabación"),
        audio.display(),
        pick("Transcript", "Transcripción"),
        text.trim()
    );
    if app.session.notes_available()
        && app.session.new_note().is_ok()
        && let Some(id) = app.session.note_focus()
        && app.session.save_note(id, &note_title, &body).is_ok()
    {
        app.session.module = iuredesk_engine::Module::Notes;
        app.notice(Notice::success(
            "The meeting transcript is saved as a minutes note.",
            "La transcripción de la reunión quedó guardada como nota de minuta.",
        ));
        return;
    }
    app.ai.set_transcript(title, text);
    app.session.module = iuredesk_engine::Module::Assistant;
    app.notice(Notice::success(
        "Transcript ready: review it and write the minutes.",
        "Transcripción lista: revísala y redacta la minuta.",
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_formats() {
        assert_eq!(clock(0.0), "00:00");
        assert_eq!(clock(724.4), "12:04");
        assert_eq!(clock(3_725.0), "1:02:05");
        assert_eq!(clock(-5.0), "00:00");
    }
}
