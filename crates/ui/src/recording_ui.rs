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
use iuredesk_engine::calendar::Occurrence;
use iuredesk_engine::iuredesk_integrations::MeetingMeta;
use iuredesk_engine::iuredesk_integrations::SisterApp;
use iuredesk_engine::suite::{EventRef, MeetingInfo, MeetingResult, RecordingState, SuiteAction};
use iurefficient_ui::{pick, theme};

use crate::app::DeskApp;

/// ¿Se puede grabar? (IureTranscribe instalado y nada grabándose).
pub fn can_record(app: &DeskApp) -> bool {
    app.suite.hub.installed(SisterApp::Transcribe) && app.suite.hub.recording().is_none()
}

/// Empieza a grabar con este título (sin cita).
pub fn start(app: &mut DeskApp, title: &str) {
    let title = if title.trim().is_empty() { untitled(app) } else { title.trim().to_string() };
    let meeting = MeetingInfo { meta: MeetingMeta { title: title.clone(), ..Default::default() }, event: None };
    app.suite.hub.run(SuiteAction::StartRecording { title, meeting });
}

/// UX-31: empieza a grabar la reunión de una cita: IureTranscribe recibe título, fecha, lugar y
/// participantes (los usa en la minuta) y la grabación queda ligada a la cita.
pub fn start_for(app: &mut DeskApp, occ: &Occurrence) {
    let meeting = meeting_for(app, occ);
    let title = if meeting.meta.title.trim().is_empty() { untitled(app) } else { meeting.meta.title.clone() };
    app.suite.hub.run(SuiteAction::StartRecording { title, meeting });
}

/// `suite.record`: empieza o detiene.
pub fn toggle(app: &mut DeskApp) {
    if app.suite.hub.recording().is_some() {
        app.suite.hub.run(SuiteAction::StopRecording);
    } else {
        match current_meeting(app) {
            Some(occ) => start_for(app, &occ),
            None => {
                let title = untitled(app);
                start(app, &title);
            }
        }
    }
}

/// La cita en curso o la que empieza en los próximos 15 minutos.
fn current_meeting(app: &mut DeskApp) -> Option<Occurrence> {
    let now = app.session.now();
    let soon = now + chrono::Duration::minutes(15);
    app.session
        .day_summary()
        .and_then(|s| s.events.iter().find(|e| e.occ.start <= soon && e.occ.end >= now).map(|e| e.occ.clone()))
}

/// «Reunión HH:MM».
fn untitled(app: &DeskApp) -> String {
    let local = app.session.zone().local(app.session.now());
    format!("{} {}", pick("Meeting", "Reunión"), local.format("%H:%M"))
}

/// UX-31: los datos de la reunión de una cita.
pub(crate) fn meeting_for(app: &DeskApp, occ: &Occurrence) -> MeetingInfo {
    let ev = app.session.calendar_event(occ.id);
    let local = app.session.zone().local(occ.start);
    let mut participants: Vec<String> = Vec::new();
    if let Some(e) = &ev {
        let people = e.organizer.iter().chain(e.attendees.iter().map(|a| &a.mailbox));
        for m in people {
            let who = match m.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                Some(n) => format!("{n} <{}>", m.addr),
                None => m.addr.clone(),
            };
            if !participants.contains(&who) {
                participants.push(who);
            }
        }
    }
    MeetingInfo {
        meta: MeetingMeta {
            title: occ.summary.trim().to_string(),
            date: local.format("%Y-%m-%d %H:%M").to_string(),
            place: occ.location.trim().to_string(),
            participants,
            notes: String::new(),
        },
        event: ev.map(|e| EventRef { id: occ.id, start: occ.start, uid: e.uid }),
    }
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
    let Some(st) = app.suite.hub.recording() else {
        work_pill(app, ctx);
        return;
    };
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

/// UX-31: tras detener, la misma barra dice qué se está haciendo (transcribir, redactar la minuta y
/// el resumen): puede tardar minutos y no debe parecer que se perdió.
fn work_pill(app: &DeskApp, ctx: &Context) {
    let Some(p) = app.suite.hub.transcribing() else { return };
    let text = match p.stage.as_str() {
        "minutes" => {
            pick("IureTranscribe is writing the minutes…", "IureTranscribe está redactando la minuta…").to_string()
        }
        "summary" => {
            pick("IureTranscribe is writing the summary…", "IureTranscribe está redactando el resumen…").to_string()
        }
        _ => format!("{} {} %", pick("Transcribing the meeting…", "Transcribiendo la reunión…"), p.percent),
    };
    egui::Area::new(egui::Id::unique("ux31-meeting-work-pill"))
        .order(egui::Order::Foreground)
        .anchor(Align2::CENTER_TOP, vec2(0.0, 44.0))
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).corner_radius(20.0).inner_margin(egui::Margin::symmetric(14, 6)).show(
                ui,
                |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        ui.add(egui::Spinner::new().size(12.0));
                        ui.label(RichText::new(text).color(theme::colors(ui).text));
                    });
                },
            );
        });
    ctx.request_repaint_after(std::time::Duration::from_millis(500));
}

/// El punto de «grabando», dibujado: el carácter «●» no está en todas las letras del sistema y
/// salía como un cuadro (lo vio el dueño 2026-11-25).
fn rec_dot(ui: &mut egui::Ui, color: egui::Color32) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(r.center(), 5.0, color);
}

/// Botón «● Grabar» con el punto dibujado (no como carácter).
pub(crate) fn record_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let c = theme::colors(ui);
    let id = egui::IdSalt::new("rec-dot");
    let out = egui::Button::new((egui::Atom::custom(id, egui::vec2(10.0, 10.0)), RichText::new(text).color(c.danger)))
        .atom_ui(ui);
    if let Some(r) = out.rect(id) {
        ui.painter().circle_filled(r.center(), 5.0, c.danger);
    }
    iurefficient_ui::a11y::button(&out.response, text);
    out.response
}

enum PillAct {
    Pause(bool),
    Stop,
}

fn pill_body(ui: &mut egui::Ui, st: &RecordingState) -> Option<PillAct> {
    let c = theme::colors(ui);
    let mut act = None;
    let dot = if st.paused { c.text_muted } else { c.danger };
    rec_dot(ui, dot);
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

/// UX-31: la reunión grabada. Con Notas: una carpeta «Reuniones/AAAA-MM-DD Título» con la minuta,
/// el resumen (si IureTranscribe los redactó) y la transcripción, cada una con las ligas a la
/// grabación en IureTranscribe y a la cita; la grabación queda en la cita (y, si no tiene
/// invitados, sus ligas en la descripción). Sin Notas, la transcripción va al asistente.
pub fn on_transcript(app: &mut DeskApp, m: &MeetingResult) {
    let title = m.title.trim();
    if !app.session.notes_available() {
        app.ai.set_transcript(title, &m.text);
        app.session.module = iuredesk_engine::Module::Assistant;
        app.notice(Notice::success(
            "Transcript ready: review it and write the minutes.",
            "Transcripción lista: revísala y redacta la minuta.",
        ));
        return;
    }
    let folder = meeting_folder(app, m);
    let links = note_links(m);
    let mut notes = vec![(pick("Transcript", "Transcripción"), m.text.trim().to_string())];
    if let Some(d) = &m.summary {
        notes.push((pick("Summary", "Resumen"), d.content.trim().to_string()));
    }
    if let Some(d) = &m.minutes {
        notes.push((pick("Minutes", "Minuta"), d.content.trim().to_string()));
    }
    // La última creada queda abierta: la minuta si la hay.
    let mut saved = 0;
    for (kind, text) in &notes {
        let body = format!("{links}\n\n{text}\n");
        if app.session.new_note_in(&folder).is_ok()
            && let Some(id) = app.session.note_focus()
            && app.session.save_note(id, &format!("{kind} — {title}"), &body).is_ok()
        {
            saved += 1;
        }
    }
    app.session.set_notes_folder_view(&folder);
    app.session.module = iuredesk_engine::Module::Notes;
    let mut in_description = false;
    if let Some(ev) = &m.meeting.event {
        let rec = iuredesk_engine::EventRecording {
            title: title.to_string(),
            audio: m.audio.to_string_lossy().into_owned(),
            open_link: m.open_link.clone(),
            notes_folder: Some(folder.clone()),
            created: app.session.now(),
        };
        in_description = app.session.record_meeting_on_event(ev, &rec, &description_links(m, &folder)).unwrap_or(false);
    }
    if saved == 0 {
        app.notice(Notice::error(iuredesk_engine::lang::Text::new(
            "The meeting notes could not be saved.",
            "No se pudieron guardar las notas de la reunión.",
        )));
        return;
    }
    let (mut en, mut es) = match (&m.minutes, &m.summary) {
        (Some(_), Some(_)) => (
            format!("Minutes, summary and transcript saved in {folder}."),
            format!("Minuta, resumen y transcripción guardados en {folder}."),
        ),
        (Some(_), None) => (
            format!("Minutes and transcript saved in {folder}."),
            format!("Minuta y transcripción guardadas en {folder}."),
        ),
        _ => (format!("Transcript saved in {folder}."), format!("Transcripción guardada en {folder}.")),
    };
    if in_description {
        en.push_str(" The links are in the event's description.");
        es.push_str(" Las ligas quedaron en la descripción de la cita.");
    }
    if let Some(p) = &m.docs_problem {
        en.push_str(&format!(" No minutes: {}", p.en));
        es.push_str(&format!(" Sin minuta: {}", p.es));
    }
    let n = if m.docs_problem.is_some() { Notice::info(en, es) } else { Notice::success(en, es) };
    app.notice(n);
}

/// «Reuniones/2026-11-27 Junta con Altamira» (la fecha de la cita o la de hoy).
fn meeting_folder(app: &DeskApp, m: &MeetingResult) -> String {
    let date = m
        .meeting
        .meta
        .date
        .get(..10)
        .filter(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").is_ok())
        .map(str::to_string)
        .unwrap_or_else(|| app.session.zone().local(app.session.now()).format("%Y-%m-%d").to_string());
    let base = pick("Meetings", "Reuniones");
    iuredesk_engine::notes_fs::clean_folder(&format!(
        "{base}/{date} {}",
        iuredesk_engine::notes_fs::file_stem(&m.title)
    ))
}

/// Encabezado de cada nota: grabación, liga a IureTranscribe y a la cita.
fn note_links(m: &MeetingResult) -> String {
    let mut v = vec![format!("**{}:** `{}`", pick("Recording", "Grabación"), m.audio.display())];
    if let Some(l) = &m.open_link {
        v.push(format!("[{}]({l})", pick("Open in IureTranscribe", "Abrir en IureTranscribe")));
    }
    if let Some(ev) = &m.meeting.event {
        let link = iuredesk_engine::open::event_link(&ev.uid);
        v.push(format!("**{}:** [{}]({link})", pick("Event", "Cita"), md_text(&m.title)));
    }
    if !m.meeting.meta.participants.is_empty() {
        v.push(format!(
            "**{}:** {}",
            pick("Participants", "Participantes"),
            md_text(&m.meeting.meta.participants.join(", "))
        ));
    }
    v.join("  \n")
}

/// Lo que se añade a la descripción de una cita sin invitados.
fn description_links(m: &MeetingResult, folder: &str) -> String {
    let mut v = vec![format!("**{}**", pick("Meeting recording (IureDesk)", "Grabación de la reunión (IureDesk)"))];
    match &m.open_link {
        Some(l) => v.push(format!(
            "- [{}]({l})",
            pick(
                "Transcript, summary and minutes in IureTranscribe",
                "Transcripción, resumen y minuta en IureTranscribe"
            )
        )),
        None => v.push(format!("- {}: `{}`", pick("Recording", "Grabación"), m.audio.display())),
    }
    v.push(format!("- {}: {folder}", pick("Notes", "Notas")));
    v.join("\n")
}

/// Texto que no rompe el Markdown de un enlace o una lista.
fn md_text(s: &str) -> String {
    s.chars().map(|c| if matches!(c, '[' | ']' | '\n' | '\r') { ' ' } else { c }).collect()
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
