//! UX-3: un mensaje en su propia ventana del sistema («Abrir en ventana», doble clic o Mayús+Intro
//! en la lista). Usa las ventanas múltiples de egui (`show_viewport_immediate`): con winit son
//! ventanas reales en Wayland, X11, Windows y macOS; donde el backend no las admite (pruebas,
//! capturas) egui la dibuja como ventana flotante dentro de la principal. Puede haber varias;
//! cada una lleva su barra (responder, archivar, imprimir, anterior/siguiente, zoom) y se recuerda
//! el tamaño de la última que se cerró.

use egui::{Context, Key, Modifiers, RichText, Ui, ViewportBuilder, ViewportClass, ViewportId, vec2};
use iuredesk_engine::command;
use iuredesk_model::{MessageEnvelope, MessageId};
use iurefficient_ui::{Icon, PaneKind, icons, pick, split, theme};

use crate::app::DeskApp;
use crate::widgets::hairline;

/// Ventanas de lectura abiertas a la vez (la más vieja cede su lugar).
const MAX_READERS: usize = 8;
/// Tamaño inicial si no hay uno recordado.
const DEFAULT_SIZE: [f32; 2] = [900.0, 760.0];

/// Una ventana de lectura.
pub(crate) struct ReaderWindow {
    id: u64,
    pub(crate) msg: MessageId,
    /// Tamaño con el que se creó (no cambia: egui redimensionaría la ventana si cambiara).
    init: egui::Vec2,
    /// Último tamaño medido (se recuerda al cerrar).
    size: Option<egui::Vec2>,
    close: bool,
    /// Traer al frente en el próximo cuadro.
    raise: bool,
    /// Conversación (generación de la lista, mensaje, mensajes).
    conv: Option<(u64, MessageId, Vec<MessageEnvelope>)>,
}

/// Abre `msg` en una ventana (o trae al frente la que ya lo muestra).
pub(crate) fn open(app: &mut DeskApp, msg: MessageId) {
    if let Some(w) = app.mail.readers.iter_mut().find(|w| w.msg == msg) {
        w.raise = true;
        return;
    }
    if app.mail.readers.len() >= MAX_READERS {
        app.mail.readers.remove(0);
    }
    app.mail.next_reader = app.mail.next_reader.wrapping_add(1);
    let init = app
        .session
        .prefs
        .reader_size
        .map(|[w, h]| vec2(w as f32, h as f32))
        .unwrap_or(vec2(DEFAULT_SIZE[0], DEFAULT_SIZE[1]))
        .clamp(vec2(420.0, 320.0), vec2(4000.0, 3000.0));
    app.mail.readers.push(ReaderWindow {
        id: app.mail.next_reader,
        msg,
        init,
        size: None,
        close: false,
        raise: false,
        conv: None,
    });
    app.session.request_body(msg);
}

/// Mensajes que muestran las ventanas abiertas (pruebas).
pub(crate) fn open_messages(app: &DeskApp) -> Vec<MessageId> {
    app.mail.readers.iter().map(|w| w.msg).collect()
}

/// Cada cuadro: dibuja las ventanas y quita las cerradas (recordando el tamaño).
pub(crate) fn show_all(app: &mut DeskApp, ctx: &Context) {
    let ids: Vec<u64> = app.mail.readers.iter().map(|w| w.id).collect();
    for id in ids {
        show_one(app, ctx, id);
    }
    let mut remember: Option<egui::Vec2> = None;
    app.mail.readers.retain(|w| {
        if w.close {
            remember = w.size.or(remember);
        }
        !w.close
    });
    if let Some(s) = remember {
        let v = [s.x.round().max(1.0) as u32, s.y.round().max(1.0) as u32];
        if app.session.prefs.reader_size != Some(v) {
            app.session.prefs.reader_size = Some(v);
            app.session.save_prefs();
        }
    }
}

fn show_one(app: &mut DeskApp, ctx: &Context, id: u64) {
    let Some(w) = app.mail.readers.iter().find(|w| w.id == id) else { return };
    let (msg, init) = (w.msg, w.init);
    let title = app
        .session
        .envelope(msg)
        .map(|e| {
            if e.subject.trim().is_empty() {
                pick("(no subject)", "(sin asunto)").to_string()
            } else {
                e.subject.clone()
            }
        })
        .unwrap_or_default();
    let builder = ViewportBuilder::default()
        .with_title(format!("{title} — IureDesk"))
        .with_app_id("iuredesk")
        .with_inner_size(init)
        .with_min_inner_size([420.0, 320.0])
        .with_resizable(true);
    let vid = ViewportId::from_hash_of(("iuredesk-reader", id));
    ctx.show_viewport_immediate(vid, builder, |ui, class| {
        let vctx = ui.ctx().clone();
        let native = class != ViewportClass::EmbeddedWindow;
        if native {
            let (close, size) =
                vctx.input(|i| (i.viewport().close_requested(), i.viewport().inner_rect.map(|r| r.size())));
            if let Some(w) = app.mail.readers.iter_mut().find(|w| w.id == id) {
                w.close |= close;
                if size.is_some() {
                    w.size = size;
                }
                if std::mem::take(&mut w.raise) {
                    vctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
            }
            if let Some(cmd) = keys(app, &vctx) {
                act(app, &vctx, id, cmd);
            }
            split::top_bar(ui, "reader-bar", |ui| bar(app, ui, id, false));
            split::center(ui, PaneKind::Detail, |ui| body(app, ui, id));
        } else {
            if let Some(w) = app.mail.readers.iter_mut().find(|w| w.id == id) {
                w.size = Some(ui.available_size());
                w.raise = false;
            }
            ui.set_min_size(vec2(init.x.min(900.0), init.y.min(640.0)));
            bar(app, ui, id, true);
            hairline(ui);
            body(app, ui, id);
        }
    });
}

/// Lo que se puede pedir desde la barra o el teclado de una ventana.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Act {
    Run(&'static str),
    Prev,
    Next,
    Close,
}

/// Atajos dentro de la ventana (la principal no recibe sus teclas).
fn keys(app: &DeskApp, ctx: &Context) -> Option<Act> {
    const ALLOWED: &[&str] = &[
        "mail.reply",
        "mail.reply_all",
        "mail.forward",
        "mail.archive",
        "mail.delete",
        "mail.toggle_flag",
        "mail.toggle_read",
        "print.message",
        "view.zoom_in",
        "view.zoom_out",
        "view.zoom_reset",
    ];
    let scheme = app.session.scheme();
    let typing = ctx.egui_wants_keyboard_input();
    let mut out = None;
    ctx.input_mut(|i| {
        let mut consumed = None;
        for (idx, e) in i.events.iter().enumerate() {
            let egui::Event::Key { key, pressed: true, modifiers, .. } = e else { continue };
            if *key == Key::Escape && *modifiers == Modifiers::NONE && !typing {
                out = Some(Act::Close);
                consumed = Some(idx);
                break;
            }
            let key = if *key == Key::Equals { Key::Plus } else { *key };
            for (cmd, chord) in command::bindings(scheme) {
                if !ALLOWED.contains(&cmd) || (typing && !chord.has_modifier()) {
                    continue;
                }
                if crate::app::chord_matches(&chord, key, *modifiers) {
                    out = Some(Act::Run(cmd));
                    consumed = Some(idx);
                    break;
                }
            }
            if out.is_some() {
                break;
            }
        }
        if let Some(idx) = consumed {
            i.events.remove(idx);
        }
    });
    out
}

/// Vecino en la lista (`-1` anterior, `+1` siguiente).
fn neighbor(app: &DeskApp, msg: MessageId, dir: i32) -> Option<MessageId> {
    let rows = app.session.rows();
    let pos = rows.iter().position(|r| r.id == msg)?;
    let next = if dir < 0 { pos.checked_sub(1)? } else { pos.checked_add(1)? };
    rows.get(next).map(|r| r.id)
}

fn act(app: &mut DeskApp, ctx: &Context, id: u64, a: Act) {
    let Some(msg) = app.mail.readers.iter().find(|w| w.id == id).map(|w| w.msg) else { return };
    let set = |app: &mut DeskApp, m: Option<MessageId>| {
        if let Some(w) = app.mail.readers.iter_mut().find(|w| w.id == id) {
            match m {
                Some(m) => {
                    w.msg = m;
                    w.conv = None;
                    app.session.request_body(m);
                }
                None => w.close = true,
            }
        }
    };
    match a {
        Act::Close => set(app, None),
        Act::Prev | Act::Next => {
            if let Some(m) = neighbor(app, msg, if a == Act::Prev { -1 } else { 1 }) {
                set(app, Some(m));
            }
        }
        Act::Run(cmd) => {
            // Lo que se mueve fuera de la carpeta deja paso al siguiente (o cierra la ventana).
            let leaves = matches!(cmd, "mail.archive" | "mail.delete");
            let after = if leaves { neighbor(app, msg, 1).or_else(|| neighbor(app, msg, -1)) } else { Some(msg) };
            // Los comandos actúan sobre el mensaje enfocado: el de esta ventana.
            app.session.set_selection(vec![msg], Some(msg));
            app.session.request_focus(msg);
            app.run(ctx, cmd);
            if leaves {
                set(app, after);
            }
        }
    }
}

fn bar(app: &mut DeskApp, ui: &mut Ui, id: u64, embedded: bool) {
    let ctx = ui.ctx().clone();
    let Some(msg) = app.mail.readers.iter().find(|w| w.id == id).map(|w| w.msg) else { return };
    let scheme = app.session.scheme();
    let mac = cfg!(target_os = "macos");
    let tip = |label: &str, cmd: &str| match command::shortcut(cmd, scheme) {
        Some(k) => format!("{label}  ({})", k.label(mac)),
        None => label.to_string(),
    };
    let (has_prev, has_next) = (neighbor(app, msg, -1).is_some(), neighbor(app, msg, 1).is_some());
    let mut fire: Option<Act> = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let narrow = ui.available_width() < 640.0;
        for (cmd, ic, en, es) in [
            ("mail.reply", Icon::Reply, "Reply", "Responder"),
            ("mail.reply_all", Icon::ReplyAll, "Reply all", "Responder a todos"),
            ("mail.forward", Icon::Forward, "Forward", "Reenviar"),
        ] {
            let label = pick(en, es);
            let r = if narrow {
                icons::icon_button(ui, ic, &tip(label, cmd), true)
            } else {
                crate::widgets::tool_button(ui, ic, label, &tip(label, cmd), true)
            };
            if r.clicked() {
                fire = Some(Act::Run(cmd));
            }
        }
        ui.add_space(6.0);
        for (cmd, ic, en, es) in [
            ("mail.archive", Icon::Archive, "Archive", "Archivar"),
            ("mail.delete", Icon::Trash, "Delete", "Eliminar"),
            ("print.message", Icon::Print, "Print…", "Imprimir…"),
            ("print.message_pdf", Icon::Export, "Save as PDF…", "Guardar como PDF…"),
        ] {
            if icons::icon_button(ui, ic, &tip(pick(en, es), cmd), true).clicked() {
                fire = Some(Act::Run(cmd));
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if embedded
                && icons::icon_button(ui, Icon::Close, pick("Close window (Esc)", "Cerrar ventana (Esc)"), true)
                    .clicked()
            {
                fire = Some(Act::Close);
            }
            if icons::icon_button(ui, Icon::ChevronRight, pick("Next message", "Mensaje siguiente"), has_next).clicked()
            {
                fire = Some(Act::Next);
            }
            if icons::icon_button(ui, Icon::ChevronLeft, pick("Previous message", "Mensaje anterior"), has_prev)
                .clicked()
            {
                fire = Some(Act::Prev);
            }
            ui.add_space(6.0);
            zoom_buttons(app, ui, &mut fire);
        });
    });
    if let Some(a) = fire {
        act(app, &ctx, id, a);
    }
}

/// − 100 % + del zoom del cuerpo.
fn zoom_buttons(app: &DeskApp, ui: &mut Ui, fire: &mut Option<Act>) {
    let c = theme::colors(ui);
    if icons::icon_button(ui, Icon::ZoomIn, pick("Zoom in (Ctrl +)", "Acercar (Ctrl +)"), true).clicked() {
        *fire = Some(Act::Run("view.zoom_in"));
    }
    let pct = (app.session.prefs.reading_zoom() * 100.0).round() as u32;
    let r = ui
        .add(egui::Label::new(RichText::new(format!("{pct} %")).color(c.text_muted)).sense(egui::Sense::click()))
        .on_hover_text(pick("Reset zoom (Ctrl 0)", "Restablecer el zoom (Ctrl 0)"));
    iurefficient_ui::a11y::button(&r, pick("Reset zoom", "Restablecer el zoom"));
    if r.clicked() {
        *fire = Some(Act::Run("view.zoom_reset"));
    }
    if icons::icon_button(ui, Icon::ZoomOut, pick("Zoom out (Ctrl −)", "Alejar (Ctrl −)"), true).clicked() {
        *fire = Some(Act::Run("view.zoom_out"));
    }
}

fn body(app: &mut DeskApp, ui: &mut Ui, id: u64) {
    let c = theme::colors(ui);
    let Some(msg) = app.mail.readers.iter().find(|w| w.id == id).map(|w| w.msg) else { return };
    let generation = app.session.list_generation();
    let cached = app
        .mail
        .readers
        .iter()
        .find(|w| w.id == id)
        .and_then(|w| w.conv.as_ref())
        .filter(|(g, m, _)| *g == generation && *m == msg)
        .map(|(_, _, v)| v.clone());
    let conv = match cached {
        Some(v) => v,
        None => {
            let v = app.session.conversation(msg);
            if let Some(w) = app.mail.readers.iter_mut().find(|w| w.id == id) {
                w.conv = Some((generation, msg, v.clone()));
            }
            v
        }
    };
    let subject = conv.last().map(|e| e.subject.clone()).unwrap_or_default();
    let (base, _) = iuredesk_model::normalize_subject(&subject);
    ui.add(
        egui::Label::new(
            RichText::new(if base.is_empty() { subject.clone() } else { base })
                .font(theme::bold_font(ui.ctx(), 19.0))
                .color(c.text),
        )
        .wrap(),
    );
    ui.add_space(6.0);
    let last = conv.last().map(|e| e.id);
    let n = conv.len();
    egui::ScrollArea::both().auto_shrink([false, false]).id_salt(("reader-scroll", id)).show(ui, |ui| {
        ui.set_max_width(ui.available_width());
        for (k, env) in conv.iter().enumerate() {
            let expanded = env.id == msg || Some(env.id) == last || n == 1;
            crate::mail::message_card(app, ui, env, expanded);
            if k + 1 < n {
                ui.add_space(6.0);
                hairline(ui);
                ui.add_space(6.0);
            }
        }
        ui.add_space(24.0);
    });
}
