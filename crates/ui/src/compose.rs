//! Redactor: De (identidad), Para/CC/CCO con burbujas y autocompletado de contactos, asunto,
//! el editor de la suite (S1.27: `iure-doc-ui`, lo de IureEditor 2.0; [`crate::md_body`]), adjuntos (selector de archivos del portal
//! y arrastrar y soltar), firma, envío con «Deshacer», envío programado y guardado automático en
//! «Borradores».

use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use egui::{Align, Context, Key, Layout, Modifiers, RichText, Sense, Stroke, Ui, vec2};
use iuredesk_engine::compose::{ComposeKind, parse_schedule, schedule_presets};
use iuredesk_engine::iuredesk_compose::{Attachment, parse_address, signature};
use iuredesk_engine::{ComposeDraft, JobOutput, Notice, Outcome, Text};
use iuredesk_model::account::Identity;
use iuredesk_model::{AccountId, Mailbox};
use iurefficient_ui::{Icon, icons, pick, primary_button, theme};

use crate::app::DeskApp;
use crate::md_body::MdBody;
use crate::widgets::human_size;

/// Guardado automático tras este tiempo sin escribir.
const AUTOSAVE_AFTER: Duration = Duration::from_secs(4);
/// Sugerencias de contactos que se muestran.
const SUGGESTIONS: usize = 6;

/// Archivos elegidos en el selector (resultado de fondo).
pub(crate) struct PickedFiles {
    pub window: u64,
    pub paths: Vec<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    To,
    Cc,
    Bcc,
}

#[derive(Default)]
struct Suggest {
    field: Option<Field>,
    items: Vec<Mailbox>,
    idx: usize,
    /// Texto de la sugerencia y generación de la libreta global con la que se calculó (si llega
    /// algo nuevo del directorio, se recalcula).
    query: String,
    generation: u64,
}

/// Una ventana de redacción.
pub struct ComposeWindow {
    pub id: u64,
    pub cd: ComposeDraft,
    pub(crate) editor: MdBody,
    /// Panel ✦ (F4.2).
    pub(crate) ai: crate::assistant::ComposeAi,
    inputs: [String; 3],
    show_cc: bool,
    link_url: Option<String>,
    sched: Option<(String, String)>,
    dirty_since: Option<Instant>,
    pub open: bool,
    suggest: Suggest,
    want_suggest: Option<(Field, String)>,
    error: Option<String>,
    /// El cursor del cuerpo ya se puso al principio.
    cursor_init: bool,
    /// Plantillas (F5.3).
    pub(crate) templates: crate::rules_ui::ComposeTemplates,
    /// F5.4: «Firmar» / «Cifrar».
    pub(crate) security: crate::security_ui::ComposeSecurity,
    /// En su propia ventana del sistema, maximizada (para escribir a sus anchas, fuera de los
    /// límites de IureDesk).
    pub(crate) own_window: bool,
}

enum Act {
    Send,
    Schedule(DateTime<Utc>),
    SaveDraft,
    Close,
    Discard,
    Attach,
    Signature,
    Identity(AccountId, Identity),
}

impl ComposeWindow {
    pub fn new(id: u64, cd: ComposeDraft) -> Self {
        let doc = MdBody::from_rich(&cd.draft.body);
        let show_cc = !cd.draft.cc.is_empty() || !cd.draft.bcc.is_empty();
        ComposeWindow {
            id,
            cd,
            editor: doc,
            ai: Default::default(),
            inputs: Default::default(),
            show_cc,
            link_url: None,
            sched: None,
            dirty_since: None,
            open: true,
            suggest: Suggest::default(),
            want_suggest: None,
            error: None,
            cursor_init: false,
            templates: Default::default(),
            security: Default::default(),
            own_window: false,
        }
    }

    fn title(&self) -> String {
        let base = match self.cd.kind {
            ComposeKind::New => pick("New message", "Nuevo mensaje"),
            ComposeKind::Reply => pick("Reply", "Responder"),
            ComposeKind::ReplyAll => pick("Reply all", "Responder a todos"),
            ComposeKind::Forward => pick("Forward", "Reenviar"),
        };
        let s = self.cd.draft.subject.trim();
        if s.is_empty() { base.to_string() } else { format!("{base}: {s}") }
    }

    fn list(&mut self, f: Field) -> &mut Vec<Mailbox> {
        match f {
            Field::To => &mut self.cd.draft.to,
            Field::Cc => &mut self.cd.draft.cc,
            Field::Bcc => &mut self.cd.draft.bcc,
        }
    }

    fn input(&mut self, f: Field) -> &mut String {
        let [to, cc, bcc] = &mut self.inputs;
        match f {
            Field::To => to,
            Field::Cc => cc,
            Field::Bcc => bcc,
        }
    }

    pub(crate) fn touch(&mut self) {
        self.dirty_since = Some(Instant::now());
        self.error = None;
    }

    /// Pasa a burbujas lo escrito en un campo de direcciones.
    fn commit_field(&mut self, f: Field) {
        let text = std::mem::take(self.input(f));
        for token in text.split([',', ';']) {
            let t = token.trim();
            if t.is_empty() {
                continue;
            }
            let m = parse_address(t).unwrap_or_else(|_| Mailbox::addr(t));
            let list = self.list(f);
            if !list.iter().any(|x| x.addr.eq_ignore_ascii_case(&m.addr)) && list.len() < iuredesk_model::MAX_RECIPIENTS
            {
                list.push(m);
            }
        }
    }

    /// Lleva lo que hay en pantalla al borrador (destinatarios a medio escribir y el cuerpo).
    pub(crate) fn commit(&mut self) {
        for f in [Field::To, Field::Cc, Field::Bcc] {
            self.commit_field(f);
        }
        self.cd.draft.body = self.editor.to_rich();
        // Los dibujos de un envío anterior (si falló o se deshizo) no se quedan como adjuntos.
        self.cd.draft.attachments.retain(|a| {
            !matches!(&a.kind, iuredesk_engine::iuredesk_compose::AttachmentKind::Inline { content_id }
                if content_id.starts_with(crate::md_body::DRAW_CID))
        });
    }

    /// Al enviar: los diagramas y fórmulas como imágenes en línea (el borrador guarda su código).
    fn embed_drawings(&mut self) {
        let (md, drawings) = crate::md_body::drawings_to_images(&self.editor.text);
        if drawings.is_empty() {
            return;
        }
        let sizes: std::collections::HashMap<String, (u32, u32)> =
            drawings.iter().map(|d| (d.cid.clone(), d.size)).collect();
        let mut body = crate::md_body::md_to_rich(&md);
        crate::md_body::size_drawings(&mut body.blocks, &sizes);
        self.cd.draft.body = body;
        for d in drawings {
            self.cd.draft.attachments.push(Attachment::inline_image(
                format!("{}.png", d.cid.split('@').next().unwrap_or("dibujo")),
                "image/png",
                d.cid,
                d.png,
            ));
        }
    }

    /// Añade archivos como adjuntos.
    pub fn attach(&mut self, paths: &[PathBuf]) {
        for p in paths.iter().take(100) {
            if p.is_file()
                && !self
                    .cd
                    .draft
                    .attachments
                    .iter()
                    .any(|a| a.source == iuredesk_engine::iuredesk_compose::AttachmentSource::Path(p.clone()))
            {
                self.cd.draft.attachments.push(Attachment::file(p.clone()));
            }
        }
        self.touch();
    }
}

fn is_valid(m: &Mailbox) -> bool {
    iuredesk_engine::iuredesk_compose::address::validate_address(&m.addr).is_ok()
}

/// Dibuja todas las ventanas de redacción abiertas y atiende lo que pidan.
pub fn show_all(app: &mut DeskApp, ctx: &Context) {
    if app.composers.is_empty() {
        return;
    }
    let identities = app.session.identities();
    let presets: Vec<(Text, DateTime<Utc>)> = schedule_presets(app.session.now(), app.session.zone());
    let mut acts: Vec<(usize, Act)> = Vec::new();
    crate::assistant::compose_sync(app);
    let env = BodyEnv {
        store: app.session.store().clone(),
        speller: app
            .pim
            .speller
            .get_or_insert_with(|| iure_doc_ui::Speller::system(crate::pim::personal_dictionary()))
            .clone(),
        layout: crate::note_preview::layout_of(app.session.prefs.mindmap_layout),
    };
    for (i, w) in app.composers.iter_mut().enumerate() {
        let mut open = w.open;
        let mut mine = Vec::new();
        if w.own_window {
            // Ventana propia del sistema: se abre maximizada y se mueve, cambia de tamaño o va a
            // otra pantalla como cualquier ventana. Cerrarla es cerrar el redactor (guarda el
            // borrador).
            let builder = egui::ViewportBuilder::default()
                .with_title(format!("{} — IureDesk", w.title()))
                .with_app_id("iuredesk")
                .with_inner_size([1000.0, 760.0])
                .with_min_inner_size([520.0, 420.0])
                .with_maximized(true)
                .with_resizable(true);
            let vid = egui::ViewportId::from_hash_of(("iuredesk-compose", w.id));
            ctx.show_viewport_immediate(vid, builder, |ui, class| {
                if class == egui::ViewportClass::EmbeddedWindow {
                    // Sin ventanas del sistema (pruebas): dentro de IureDesk.
                    ui.set_min_size(vec2(720.0, 560.0));
                    form(ui, w, &identities, &presets, &env, &mut mine);
                    return;
                }
                if ui.ctx().input(|i| i.viewport().close_requested()) {
                    open = false;
                }
                egui::CentralPanel::default().show(ui, |ui| {
                    form(ui, w, &identities, &presets, &env, &mut mine);
                });
            });
        } else {
            egui::Window::new(w.title())
                .id(egui::Id::unique(("compose", w.id)))
                .open(&mut open)
                .min_size([420.0, 340.0])
                .collapsible(false)
                // En pantallas anchas no crece a todo lo ancho: los campos piden ancho infinito.
                .default_size([720.0, 600.0])
                .max_width(compose_max_width(ctx))
                .show(ctx, |ui| form(ui, w, &identities, &presets, &env, &mut mine));
        }
        if !open {
            mine.push(Act::Close);
        }
        acts.extend(mine.into_iter().map(|a| (i, a)));
    }
    // Sugerencias pedidas en este cuadro (se ven en el siguiente).
    for w in &mut app.composers {
        if let Some((f, q)) = w.want_suggest.take() {
            app.session.directory_lookup(&q);
            w.suggest = Suggest {
                field: Some(f),
                items: app.session.suggest_addresses(&q, SUGGESTIONS),
                idx: 0,
                generation: app.session.directory_generation(),
                query: q,
            };
            ctx.request_repaint();
        } else if w.suggest.field.is_some() && w.suggest.generation != app.session.directory_generation() {
            // Llegó la libreta global: se suman sus nombres sin mover la selección.
            w.suggest.items = app.session.suggest_addresses(&w.suggest.query, SUGGESTIONS);
            w.suggest.idx = w.suggest.idx.min(w.suggest.items.len().saturating_sub(1));
            w.suggest.generation = app.session.directory_generation();
            ctx.request_repaint();
        }
    }
    for (i, act) in acts {
        run_act(app, ctx, i, act);
    }
    crate::assistant::compose_pump(app);
    crate::rules_ui::template_pump(app);
    // Guardado automático (solo local) tras unos segundos sin escribir.
    let mut autosave = Vec::new();
    for (i, w) in app.composers.iter().enumerate() {
        if let Some(t) = w.dirty_since {
            if t.elapsed() >= AUTOSAVE_AFTER {
                autosave.push(i);
            } else {
                ctx.request_repaint_after(AUTOSAVE_AFTER.saturating_sub(t.elapsed()));
            }
        }
    }
    for i in autosave {
        if let Some(w) = app.composers.get_mut(i) {
            w.dirty_since = None;
            w.commit();
            if !w.cd.is_blank() {
                let _ = app.session.save_draft(&mut w.cd, false);
            }
        }
    }
    app.composers.retain(|w| w.open);
}

fn run_act(app: &mut DeskApp, ctx: &Context, i: usize, act: Act) {
    let Some(w) = app.composers.get_mut(i) else { return };
    match act {
        Act::Send | Act::Schedule(_) => {
            w.commit();
            w.embed_drawings();
            w.cd.draft.send_at = match act {
                Act::Schedule(t) => Some(t),
                _ => None,
            };
            if let Some(bad) = w.cd.draft.all_recipients().into_iter().find(|m| !is_valid(m)) {
                w.error = Some(format!(
                    "{} «{}»",
                    pick("This address doesn't look right:", "Esta dirección no parece correcta:"),
                    bad.addr
                ));
                return;
            }
            let cd = w.cd.clone();
            match app.session.send(&cd) {
                Ok(out) => {
                    w.open = false;
                    app.outcome(ctx, out);
                }
                Err(e) => {
                    if let Some(w) = app.composers.get_mut(i) {
                        w.error = Some(e.text().get().to_string());
                    }
                }
            }
        }
        Act::SaveDraft | Act::Close => {
            w.commit();
            let explicit = matches!(act, Act::SaveDraft);
            if w.cd.is_blank() {
                if let Some(saved) = w.cd.saved.take() {
                    app.session.discard_message(saved);
                }
                w.open = false;
                return;
            }
            let mut cd = w.cd.clone();
            match app.session.save_draft(&mut cd, true) {
                Ok(_) => {
                    w.open = false;
                    app.notice(Notice::success("Draft saved in Drafts", "Borrador guardado en Borradores"));
                }
                Err(e) => {
                    if explicit {
                        w.error = Some(e.text().get().to_string());
                    } else {
                        w.open = false;
                        app.notice(Notice::error(e.text()));
                    }
                }
            }
        }
        Act::Discard => {
            if let Some(saved) = w.cd.saved.take() {
                app.session.discard_message(saved);
            }
            w.open = false;
            app.notice(Notice::info("Message discarded", "Mensaje descartado"));
        }
        Act::Attach => {
            let window = w.id;
            app.session.spawn_blocking(move || {
                let paths = rfd::FileDialog::new()
                    .set_title(pick("Attach files", "Adjuntar archivos"))
                    .pick_files()
                    .unwrap_or_default();
                JobOutput::Custom(Box::new(PickedFiles { window, paths }))
            });
        }
        Act::Signature => {
            w.commit();
            let reply = w.cd.kind != ComposeKind::New;
            let sig = app.session.signature_for(&w.cd.draft.identity, reply);
            if sig.is_none() {
                app.notice(Notice::info(
                    "This identity has no signature yet. Add one in Settings → Accounts.",
                    "Esta identidad aún no tiene firma. Agrégala en Ajustes → Cuentas.",
                ));
                return;
            }
            signature::apply(&mut w.cd.draft.body, sig.as_ref(), app.session.signature_placement());
            w.editor.set_rich(&w.cd.draft.body);
            w.touch();
        }
        Act::Identity(aid, ident) => {
            w.commit();
            let mut cd = w.cd.clone();
            app.session.switch_identity(&mut cd, aid, ident);
            if let Some(w) = app.composers.get_mut(i) {
                w.cd = cd;
                w.editor.set_rich(&w.cd.draft.body);
                w.touch();
            }
        }
    }
}

/// Abre el redactor (o lo enfoca) con archivos soltados sobre la ventana.
pub fn on_dropped(app: &mut DeskApp, paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    if app.composers.is_empty()
        && let Ok(Outcome::Compose(d)) = app.session.run("mail.new")
    {
        app.open_composer(*d);
    }
    if let Some(w) = app.composers.last_mut() {
        w.attach(&paths);
    }
}

/// Archivos elegidos en el selector.
pub fn on_picked(app: &mut DeskApp, picked: PickedFiles) {
    if let Some(w) = app.composers.iter_mut().find(|w| w.id == picked.window) {
        w.attach(&picked.paths);
    }
}

fn field_label(ui: &mut Ui, text: &str) -> egui::Response {
    let c = theme::colors(ui);
    ui.add_sized(vec2(52.0, 22.0), egui::Label::new(RichText::new(text).color(c.text_muted)))
}

fn chip(ui: &mut Ui, m: &Mailbox) -> bool {
    let c = theme::colors(ui);
    let ok = is_valid(m);
    let mut remove = false;
    egui::Frame::new()
        .fill(if ok { c.surface_alt } else { iurefficient_ui::theme::colors(ui).danger.gamma_multiply(0.12) })
        .stroke(Stroke::new(1.0, if ok { c.separator } else { c.danger }))
        .corner_radius(10)
        .inner_margin(egui::Margin { left: 8, right: 4, top: 2, bottom: 2 })
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.horizontal(|ui| {
                let label = m.name.as_deref().filter(|n| !n.trim().is_empty()).unwrap_or(&m.addr);
                let r = ui.label(RichText::new(label).color(if ok { c.text } else { c.danger }));
                let tip = if ok {
                    m.addr.clone()
                } else {
                    format!("{} — {}", m.addr, pick("not a valid address", "no es una dirección válida"))
                };
                r.on_hover_text(tip);
                if icons::icon_button(ui, Icon::Close, &format!("{} {}", pick("Remove", "Quitar"), m.addr), true)
                    .clicked()
                {
                    remove = true;
                }
            });
        });
    remove
}

/// Fila de destinatarios con burbujas, campo de texto y sugerencias.
fn address_row(ui: &mut Ui, w: &mut ComposeWindow, f: Field, label: &str) {
    let id = egui::Id::unique(("compose-addr", w.id, f as u8));
    let has_focus = ui.memory(|m| m.has_focus(id));
    let open_suggest =
        has_focus && w.suggest.field == Some(f) && !w.suggest.items.is_empty() && !w.input(f).trim().is_empty();
    // Teclas de la lista de sugerencias (antes de que el campo las use).
    let mut pick_idx: Option<usize> = None;
    if open_suggest {
        let n = w.suggest.items.len();
        ui.input_mut(|i| {
            if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                w.suggest.idx = (w.suggest.idx + 1) % n.max(1);
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                w.suggest.idx = (w.suggest.idx + n.max(1) - 1) % n.max(1);
            }
            if i.consume_key(Modifiers::NONE, Key::Enter) || i.consume_key(Modifiers::NONE, Key::Tab) {
                pick_idx = Some(w.suggest.idx);
            }
        });
    }
    let backspace_on_empty = has_focus && w.input(f).is_empty() && ui.input(|i| i.key_pressed(Key::Backspace));
    let mut text_rect = None;
    ui.horizontal(|ui| {
        let lr = field_label(ui, label);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
            let mut remove = None;
            for (k, m) in w.list(f).clone().iter().enumerate() {
                if chip(ui, m) {
                    remove = Some(k);
                }
            }
            if let Some(k) = remove {
                if k < w.list(f).len() {
                    w.list(f).remove(k);
                }
                w.touch();
            }
            let hint = if w.list(f).is_empty() { pick("Name or address", "Nombre o dirección") } else { "" };
            let width = (ui.available_size_before_wrap().x - 2.0).max(120.0);
            let input = w.input(f);
            let r = ui.add(
                egui::TextEdit::singleline(input).id(id).hint_text(hint).frame(egui::Frame::NONE).desired_width(width),
            );
            let r = r.labelled_by(lr.id);
            text_rect = Some(r.rect);
            if r.changed() {
                let now = w.input(f).clone();
                if now.contains([',', ';']) {
                    w.commit_field(f);
                } else if now.trim().chars().count() >= 1 {
                    w.want_suggest = Some((f, now.trim().to_string()));
                }
                w.touch();
            }
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if (enter || (r.lost_focus() && pick_idx.is_none())) && !w.input(f).trim().is_empty() && !open_suggest {
                w.commit_field(f);
                w.touch();
            }
        });
    });
    if backspace_on_empty && !w.list(f).is_empty() {
        w.list(f).pop();
        w.touch();
    }
    if let Some(k) = pick_idx
        && let Some(m) = w.suggest.items.get(k).cloned()
    {
        w.input(f).clear();
        if !w.list(f).iter().any(|x| x.addr.eq_ignore_ascii_case(&m.addr)) {
            w.list(f).push(m);
        }
        w.suggest = Suggest::default();
        ui.memory_mut(|mem| mem.request_focus(id));
        w.touch();
        return;
    }
    if open_suggest && let Some(rect) = text_rect {
        let c = theme::colors(ui);
        let mut chosen = None;
        egui::Area::new(id.with("suggest"))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.left_bottom() + vec2(0.0, 2.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(rect.width().clamp(220.0, 420.0));
                    for (k, m) in w.suggest.items.iter().enumerate() {
                        let text = match &m.name {
                            Some(n) if !n.is_empty() => format!("{n}  <{}>", m.addr),
                            _ => m.addr.clone(),
                        };
                        let r = ui.add(egui::Button::selectable(k == w.suggest.idx, RichText::new(text).color(c.text)));
                        if r.clicked() {
                            chosen = Some(k);
                        }
                    }
                });
            });
        if let Some(k) = chosen
            && let Some(m) = w.suggest.items.get(k).cloned()
        {
            w.input(f).clear();
            w.list(f).push(m);
            w.suggest = Suggest::default();
            w.touch();
        }
    }
}

/// Lo que el editor del cuerpo toma de la app: el almacén (imágenes en línea) y el corrector.
struct BodyEnv {
    store: std::sync::Arc<iuredesk_store::Store>,
    speller: std::sync::Arc<iure_doc_ui::Speller>,
    /// Distribución de los mapas mentales (Preferencias).
    layout: iuredesk_html_view::mindmap::Layout,
}

fn form(
    ui: &mut Ui,
    w: &mut ComposeWindow,
    identities: &[(AccountId, Identity)],
    presets: &[(Text, DateTime<Utc>)],
    env: &BodyEnv,
    acts: &mut Vec<Act>,
) {
    let ctx = ui.ctx().clone();
    let c = theme::colors(ui);
    ui.spacing_mut().item_spacing.y = 4.0;
    // De:
    if identities.len() > 1 {
        ui.horizontal(|ui| {
            let lr = field_label(ui, pick("From", "De"));
            let cur = format!("{} <{}>", w.cd.draft.identity.name, w.cd.draft.identity.email);
            let r = egui::ComboBox::from_id_salt(("compose-from", w.id))
                .selected_text(cur)
                .width(ui.available_width().min(420.0))
                .show_ui(ui, |ui| {
                    for (aid, ident) in identities {
                        let label = format!("{} <{}>", ident.name, ident.email);
                        let on = ident.email.eq_ignore_ascii_case(&w.cd.draft.identity.email);
                        if ui.selectable_label(on, label).clicked() && !on {
                            acts.push(Act::Identity(*aid, ident.clone()));
                        }
                    }
                });
            r.response.labelled_by(lr.id);
        });
    }
    address_row(ui, w, Field::To, pick("To", "Para"));
    if w.show_cc {
        address_row(ui, w, Field::Cc, "CC");
        address_row(ui, w, Field::Bcc, pick("Bcc", "CCO"));
    }
    ui.horizontal(|ui| {
        let lr = field_label(ui, pick("Subject", "Asunto"));
        // Lo que ocupa el botón CC/CCO, medido: si se calcula de menos, la ventana se ensancha un
        // poco en cada cuadro (se veía como una animación al abrir).
        let cc_width = if w.show_cc {
            0.0
        } else {
            let font = egui::TextStyle::Button.resolve(ui.style());
            let text = ui.fonts_mut(|f| f.layout_no_wrap(pick("Cc/Bcc", "CC/CCO").into(), font, c.text).size().x);
            text + 2.0 * ui.spacing().button_padding.x + ui.spacing().item_spacing.x + 2.0
        };
        let r = ui.add(
            egui::TextEdit::singleline(&mut w.cd.draft.subject)
                .id(egui::Id::unique(("compose-subject", w.id)))
                .hint_text(pick("Subject", "Asunto"))
                .desired_width((ui.available_width() - cc_width).max(80.0)),
        );
        let r = r.labelled_by(lr.id);
        if r.changed() {
            w.touch();
        }
        if !w.show_cc && ui.small_button(pick("Cc/Bcc", "CC/CCO")).clicked() {
            w.show_cc = true;
        }
    });
    ui.add_space(2.0);
    crate::widgets::hairline(ui);
    // Barra de formato: acciones del editor sobre la selección (Markdown).
    let body_id = egui::Id::unique(("compose-body", w.id));
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 1.0;
        use iure_doc_ui::ExternalEdit as E;
        use iure_doc_ui::toolbar::{self, Fmt, Glyph, Pressed, Tool};
        // La misma barra que IureEditor (iconos de la suite), con lo que un correo necesita.
        const MAIL: &[Tool] = &[
            Tool::Undo,
            Tool::Redo,
            Tool::Separator,
            Tool::Bold,
            Tool::Italic,
            Tool::Underline,
            Tool::Strike,
            Tool::TextColor,
            Tool::Highlight,
            Tool::Separator,
            Tool::Bullet,
            Tool::Ordered,
            Tool::Quote,
            Tool::Separator,
            Tool::Link,
            Tool::Diagram,
        ];
        let edit = match toolbar::tools(ui, MAIL) {
            // Enlace: se pide la dirección en la barra de abajo.
            Some(Pressed::Edit(E::Link)) => {
                w.link_url = Some("https://".into());
                None
            }
            Some(Pressed::Edit(e)) => Some(e),
            // Subrayado: `<u>…</u>` (Markdown no lo tiene; el correo sí).
            Some(Pressed::Other(Tool::Underline)) => {
                let (a, b) = (w.editor.sel.0.min(w.editor.sel.1), w.editor.sel.0.max(w.editor.sel.1));
                let sel: String = w.editor.text.chars().skip(a).take(b - a).collect();
                (!sel.is_empty()).then(|| E::Replace(format!("<u>{sel}</u>")))
            }
            Some(Pressed::Other(_)) | None => None,
        };
        if let Some(e) = edit
            && iure_doc_ui::apply_external(&ctx, body_id, &mut w.editor.text, e)
        {
            w.editor.changed = true;
            w.touch();
            ui.memory_mut(|m| m.request_focus(body_id));
        }
        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);
        if toolbar::fmt_button(ui, Fmt::Icon(Icon::Attachment), pick("Attach files", "Adjuntar archivos")) {
            acts.push(Act::Attach);
        }
        if toolbar::fmt_button(ui, Fmt::Draw(Glyph::Signature), pick("Insert my signature", "Insertar mi firma")) {
            acts.push(Act::Signature);
        }
        crate::assistant::compose_buttons(ui, &mut w.ai, w.id);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let (icon, tip) = if w.own_window {
                (Icon::Restore, pick("Back into the IureDesk window", "Volver a la ventana de IureDesk"))
            } else {
                (Icon::Maximize, pick("Open in its own maximized window", "Abrir en ventana propia maximizada"))
            };
            if icons::icon_button(ui, icon, tip, true).clicked() {
                w.own_window = !w.own_window;
            }
        });
    });
    if let Some(url) = w.link_url.as_mut() {
        let mut done = None;
        let mut cancel = false;
        ui.horizontal(|ui| {
            let lr = field_label(ui, pick("Link", "Enlace"));
            let r = ui.add(egui::TextEdit::singleline(url).desired_width(260.0).hint_text("https://…"));
            let r = r.labelled_by(lr.id);
            if ui.button(pick("Apply", "Aplicar")).clicked()
                || (r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)))
            {
                done = Some(true);
            }
            if ui.button(pick("Remove link", "Quitar enlace")).clicked() {
                done = Some(false);
            }
            if ui.button(pick("Cancel", "Cancelar")).clicked() {
                cancel = true;
            }
        });
        if cancel {
            w.link_url = None;
        }
        match done {
            Some(true) => {
                let href = w.link_url.take().unwrap_or_default();
                if let Some(safe) = iuredesk_engine::iuredesk_compose::rich::html::safe_href(href.trim()) {
                    let (a, b) = (w.editor.sel.0.min(w.editor.sel.1), w.editor.sel.0.max(w.editor.sel.1));
                    let sel: String = w.editor.text.chars().skip(a).take(b - a).collect();
                    let label = if sel.trim().is_empty() { safe.clone() } else { sel };
                    let md = format!("[{label}]({safe})");
                    if iure_doc_ui::apply_external(
                        &ctx,
                        body_id,
                        &mut w.editor.text,
                        iure_doc_ui::ExternalEdit::Replace(md),
                    ) {
                        w.editor.changed = true;
                    }
                    w.touch();
                } else {
                    w.error = Some(pick("That link isn't valid.", "Ese enlace no es válido.").to_string());
                }
            }
            Some(false) => {
                // Quitar el enlace: queda su texto.
                w.link_url = None;
                let (a, b) = (w.editor.sel.0.min(w.editor.sel.1), w.editor.sel.0.max(w.editor.sel.1));
                let sel: String = w.editor.text.chars().skip(a).take(b - a).collect();
                if let Some(label) = sel.strip_prefix('[').and_then(|r| r.split_once("](")).map(|(l, _)| l.to_string())
                    && iure_doc_ui::apply_external(
                        &ctx,
                        body_id,
                        &mut w.editor.text,
                        iure_doc_ui::ExternalEdit::Replace(label),
                    )
                {
                    w.editor.changed = true;
                }
                w.touch();
            }
            None => {}
        }
    }
    crate::assistant::compose_panel(ui, &mut w.ai, &w.editor);
    ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
        ui.add_space(2.0);
        bottom_bar(ui, w, presets, acts);
        if let Some(e) = &w.error {
            ui.label(RichText::new(e).color(c.danger));
        }
        if !w.cd.draft.attachments.is_empty() {
            let mut remove = None;
            ui.horizontal_wrapped(|ui| {
                for (k, a) in w.cd.draft.attachments.iter().enumerate() {
                    egui::Frame::new()
                        .fill(c.surface_alt)
                        .stroke(Stroke::new(1.0, c.separator))
                        .corner_radius(6)
                        .inner_margin(egui::Margin::symmetric(8, 3))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                                icons::paint(ui.painter(), r, Icon::Attachment, c.text_muted, c.accent);
                                ui.label(RichText::new(&a.filename).color(c.text));
                                ui.label(
                                    RichText::new(human_size(usize::try_from(a.size).unwrap_or(usize::MAX)))
                                        .color(c.text_muted)
                                        .size(theme::density(ui.ctx()).small_size()),
                                );
                                if icons::icon_button(
                                    ui,
                                    Icon::Close,
                                    &format!("{} {}", pick("Remove", "Quitar"), a.filename),
                                    true,
                                )
                                .clicked()
                                {
                                    remove = Some(k);
                                }
                            });
                        });
                }
            });
            if let Some(k) = remove {
                if k < w.cd.draft.attachments.len() {
                    w.cd.draft.attachments.remove(k);
                }
                w.touch();
            }
        }
        ui.add_space(4.0);
        ui.with_layout(Layout::top_down(Align::Min), |ui| {
            if !w.cursor_init {
                // Al abrir, el cursor va al principio (encima de la cita y la firma).
                w.cursor_init = true;
                iure_doc_ui::focus_offset(ui.ctx(), body_id, &w.editor.text, 0);
            }
            // Imágenes en línea, diagramas y fórmulas dibujados en su lugar (al enviar van como
            // imágenes).
            let images: std::sync::Arc<dyn iure_doc_ui::DiagramRenderer> =
                std::sync::Arc::new(iure_doc_ui::Renderers(vec![
                    std::sync::Arc::new(crate::md_body::CidImages::new(env.store.clone(), &w.cd.draft.attachments)),
                    std::sync::Arc::new(crate::note_diagrams::NoteDiagrams::new(ui, env.layout)),
                    std::sync::Arc::new(crate::note_diagrams::NoteFormulas::new(ui, false)),
                ]));
            let hooks = iure_doc_ui::Hooks {
                diagrams: Some(images),
                math: Some(std::sync::Arc::new(crate::note_diagrams::NoteFormulas::new(ui, true))),
                paste: Some(std::sync::Arc::new(iure_doc_ui::SystemClipboard)),
                spell: Some(env.speller.clone()),
                ..Default::default()
            };
            // Por bloques (con su propio desplazamiento): formato al escribir, `/`, buscar…
            let out = iure_doc_ui::show_blocks_hooks(
                ui,
                body_id,
                &mut w.editor.text,
                pick("Write your message", "Escribe tu mensaje"),
                hooks,
            );
            out.response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::MultilineTextInput, true, pick("Message", "Mensaje"))
            });
            if out.changed {
                w.editor.changed = true;
            }
            if let Some(l) = out.link {
                let lower = l.to_ascii_lowercase();
                if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:") {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(l));
                }
            }
            // Lo seleccionado (o el cursor), para la barra, el asistente y el dictado.
            w.editor.sel = iure_doc_ui::selection(ui.ctx(), body_id, &w.editor.text).unwrap_or_else(|| {
                let c = iure_doc_ui::caret_offset(ui.ctx(), body_id, &w.editor.text).unwrap_or(0);
                (c, c)
            });
            if std::mem::take(&mut w.editor.changed) {
                w.touch();
            }
        });
    });
}

fn bottom_bar(ui: &mut Ui, w: &mut ComposeWindow, presets: &[(Text, DateTime<Utc>)], acts: &mut Vec<Act>) {
    if let Some((date, time)) = w.sched.as_mut() {
        let mut close = false;
        ui.horizontal(|ui| {
            let lr = field_label(ui, pick("Send on", "Enviar el"));
            let r = ui
                .add(egui::TextEdit::singleline(date).desired_width(96.0).hint_text(pick("YYYY-MM-DD", "AAAA-MM-DD")));
            r.labelled_by(lr.id);
            let rt = ui.add(egui::TextEdit::singleline(time).desired_width(54.0).hint_text("HH:MM"));
            rt.on_hover_text(pick("Time (24 h)", "Hora (24 h)"));
            if ui.button(pick("Schedule", "Programar")).clicked() {
                match parse_schedule(date, time, Utc::now(), iuredesk_engine::Zone::Local) {
                    Some(t) => {
                        acts.push(Act::Schedule(t));
                        close = true;
                    }
                    None => {
                        w.error = Some(
                            pick(
                                "Write a future date and time, like 2026-10-27 and 09:30.",
                                "Escribe una fecha y hora futuras, como 2026-10-27 y 09:30.",
                            )
                            .to_string(),
                        );
                    }
                }
            }
            if ui.button(pick("Cancel", "Cancelar")).clicked() {
                close = true;
            }
        });
        if close {
            w.sched = None;
        }
    }
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        if primary_button(ui, pick("Send", "Enviar"))
            .on_hover_text(pick("Send (Ctrl+Enter)", "Enviar (Ctrl+Intro)"))
            .clicked()
        {
            acts.push(Act::Send);
        }
        let more = icons::icon_button(ui, Icon::Clock, pick("Schedule send", "Programar envío"), true);
        egui::Popup::menu(&more).show(|ui| {
            for (label, at) in presets {
                if icons::menu_item(ui, Some(Icon::Clock), label.get()).clicked() {
                    acts.push(Act::Schedule(*at));
                    ui.close();
                }
            }
            if icons::menu_item(ui, Some(Icon::Calendar), pick("Pick date and time…", "Elegir fecha y hora…")).clicked()
            {
                let tomorrow = chrono::Local::now().date_naive() + chrono::Duration::days(1);
                w.sched = Some((tomorrow.format("%Y-%m-%d").to_string(), "09:00".into()));
                ui.close();
            }
        });
        if ui.button(pick("Save draft", "Guardar borrador")).clicked() {
            acts.push(Act::SaveDraft);
        }
        crate::rules_ui::template_button(ui, &mut w.templates);
        crate::security_ui::compose_toggles(ui, &mut w.cd, &mut w.security);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button(pick("Discard", "Descartar")).clicked() {
                acts.push(Act::Discard);
            }
            if w.cd.saved.is_some() && w.dirty_since.is_none() {
                ui.label(
                    RichText::new(pick("Draft saved", "Borrador guardado"))
                        .color(theme::colors(ui).text_muted)
                        .size(theme::density(ui.ctx()).small_size()),
                );
            }
        });
    });
    // Ctrl+Intro envía (solo la ventana en la que se está escribiendo).
    let mine = ui.memory(|m| {
        m.has_focus(egui::Id::unique(("compose-body", w.id)))
            || m.has_focus(egui::Id::unique(("compose-subject", w.id)))
    });
    if mine && ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Enter)) {
        acts.push(Act::Send);
    }
}
/// Ancho máximo del redactor: en pantallas anchas no crece a todo lo ancho (los campos piden
/// ancho infinito). Sin pantalla conocida (pruebas), un valor fijo.
fn compose_max_width(ctx: &egui::Context) -> f32 {
    let w = ctx.content_rect().width();
    if w.is_finite() && w > 0.0 { (w * 0.7).clamp(560.0, 960.0) } else { 900.0 }
}
