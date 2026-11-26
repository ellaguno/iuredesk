//! Las personas como objetos (UX-24): remitente y destinatarios del lector son nombres que se
//! pueden tocar. Al pasar el ratón sale una tarjeta flotante (como en Gmail) con sus datos de
//! Contactos y un botón para copiar cada uno; con un clic (o Intro) la misma tarjeta queda
//! abierta, y el clic derecho da las acciones en un menú. Las acciones van por comandos
//! (`mail.from_sender`, `mail.select_sender`) para que la paleta, la CLI y MCP hagan lo mismo.

use egui::{Rect, Response, RichText, Sense, Stroke, Ui, vec2};
use iuredesk_engine::Notice;
use iuredesk_engine::session::contact_display_name;
use iuredesk_model::{Contact, ContactId, Mailbox};
use iurefficient_ui::{Icon, avatar, icons, pick, theme};

use crate::app::DeskApp;

/// Ancho de la tarjeta.
const CARD_WIDTH: f32 = 300.0;
/// Destinatarios que se muestran antes de «+N».
pub(crate) const MAX_CHIPS: usize = 6;
/// Datos que se listan por tipo (correos, teléfonos).
const MAX_ROWS: usize = 6;

/// Lo que se pidió desde la tarjeta o el menú.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Action {
    /// Copiar este texto.
    Copy(String),
    /// Escribirle un correo.
    Write(Mailbox),
    /// Ver sus correos (búsqueda `de:`).
    Messages(String),
    /// Seleccionar sus correos en la lista actual.
    Select(String),
    /// Agregarla a Contactos.
    AddContact(Mailbox),
    /// Editar su contacto.
    EditContact(ContactId),
}

/// Nombre de una persona que abre su tarjeta: al pasar el ratón, con un clic y con el menú
/// contextual. `strong` para el remitente (en negrita, sin marco).
pub(crate) fn name(app: &mut DeskApp, ui: &mut Ui, m: &Mailbox, strong: bool) -> Response {
    let ctx = ui.ctx().clone();
    let c = theme::colors(ui);
    let d = theme::density(&ctx);
    let text = RichText::new(m.display());
    let resp = if strong {
        ui.add(
            egui::Label::new(text.font(theme::bold_font(&ctx, d.body_size())).color(c.text))
                .truncate()
                .sense(Sense::click()),
        )
    } else {
        ui.add(
            egui::Button::new(text.color(c.text).size(d.small_size()))
                .fill(c.surface)
                .stroke(Stroke::new(1.0, c.separator))
                .corner_radius(10)
                .min_size(vec2(0.0, d.small_size() + 6.0)),
        )
    };
    attach(app, &resp, m);
    resp
}

/// Abre la tarjeta de `m` desde cualquier respuesta (nombre, avatar): flotante al pasar el ratón,
/// fija con un clic, y menú con el clic derecho.
pub(crate) fn attach(app: &mut DeskApp, resp: &Response, m: &Mailbox) {
    let ctx = resp.ctx.clone();
    let resp = resp.clone().on_hover_cursor(egui::CursorIcon::PointingHand);
    let who = format!("{} {}", pick("Contact card for", "Tarjeta de contacto de"), m.display());
    crate::widgets::named(&resp, egui::Role::Button, &who);
    let mut out: Option<Action> = None;
    // Id propio: el menú contextual usa el de la respuesta y lo cierra con cada clic.
    let popup = egui::Popup::from_toggle_button_response(&resp)
        .id(resp.id.with("tarjeta"))
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .width(CARD_WIDTH);
    let pinned = egui::Popup::is_id_open(&ctx, popup.get_id());
    popup.show(|ui| {
        let data = Data::of(app, m);
        card(ui, &data, &mut out);
    });
    if !pinned {
        egui::Tooltip::for_enabled(&resp).width(CARD_WIDTH).show(|ui| {
            let data = Data::of(app, m);
            card(ui, &data, &mut out);
        });
    }
    egui::Popup::context_menu(&resp).show(|ui| {
        let data = Data::of(app, m);
        menu(ui, &data, &mut out);
    });
    if let Some(a) = out {
        egui::Popup::close_all(&ctx);
        apply(app, &ctx, a);
    }
}

/// Lo que se sabe de una persona: su buzón y su contacto, si lo hay.
struct Data {
    mailbox: Mailbox,
    contact: Option<Contact>,
    /// ¿Se puede editar su contacto (libreta con escritura)?
    editable: bool,
}

impl Data {
    fn of(app: &mut DeskApp, m: &Mailbox) -> Data {
        let contact = app.session.contact_by_email(&m.addr);
        let editable = contact.as_ref().is_some_and(|c| app.session.contact_deletable(c.id));
        Data { mailbox: m.clone(), contact, editable }
    }

    fn name(&self) -> String {
        self.contact
            .as_ref()
            .and_then(contact_display_name)
            .or_else(|| self.mailbox.name.clone().filter(|n| !n.trim().is_empty()))
            .unwrap_or_else(|| self.mailbox.addr.clone())
    }

    /// «Nombre <correo>», para pegar en un campo de destinatarios.
    fn full_address(&self) -> String {
        let n = self.name();
        if n == self.mailbox.addr { n } else { format!("{} <{}>", n.replace(['<', '>', '"'], ""), self.mailbox.addr) }
    }

    /// Correos: el del mensaje primero y luego los demás del contacto, sin repetir.
    fn emails(&self) -> Vec<String> {
        let mut v = vec![self.mailbox.addr.clone()];
        if let Some(c) = &self.contact {
            for e in &c.emails {
                let e = e.value.trim();
                if !e.is_empty() && !v.iter().any(|x| x.eq_ignore_ascii_case(e)) {
                    v.push(e.to_string());
                }
            }
        }
        v.truncate(MAX_ROWS);
        v
    }

    fn phones(&self) -> Vec<String> {
        self.contact
            .as_ref()
            .map(|c| {
                c.phones.iter().map(|p| p.value.trim().to_string()).filter(|p| !p.is_empty()).take(MAX_ROWS).collect()
            })
            .unwrap_or_default()
    }

    /// «Cargo · Empresa».
    fn role(&self) -> String {
        let Some(c) = &self.contact else { return String::new() };
        [c.title.trim(), c.org.trim()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ")
    }
}

/// La tarjeta: avatar, nombre, cargo, correos y teléfonos con «copiar», y las acciones.
fn card(ui: &mut Ui, d: &Data, out: &mut Option<Action>) {
    let c = theme::colors(ui);
    let ctx = ui.ctx().clone();
    let dens = theme::density(&ctx);
    let name = d.name();
    ui.set_width(CARD_WIDTH - 16.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        avatar(ui, &name, &d.mailbox.addr, 40.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.add(
                egui::Label::new(
                    RichText::new(&name).font(theme::bold_font(&ctx, dens.body_size() + 1.0)).color(c.text),
                )
                .truncate(),
            );
            let role = d.role();
            if !role.is_empty() {
                ui.add(egui::Label::new(RichText::new(role).color(c.text_muted).size(dens.small_size())).truncate());
            }
        });
    });
    ui.add_space(6.0);
    for e in d.emails() {
        value_row(ui, Icon::Mail, &e, out);
    }
    for p in d.phones() {
        value_row(ui, Icon::Phone, &p, out);
    }
    ui.add_space(4.0);
    ui.separator();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
        let addr = d.mailbox.addr.clone();
        for (ic, en, es, a) in [
            (Icon::Send, "Write", "Escribir", Action::Write(Mailbox { name: Some(name.clone()), addr: addr.clone() })),
            (Icon::Search, "Messages", "Sus correos", Action::Messages(addr.clone())),
            (Icon::Check, "Select", "Seleccionar", Action::Select(addr.clone())),
        ] {
            if small_button(ui, ic, pick(en, es)).clicked() {
                *out = Some(a);
            }
        }
        let (ic, en, es, a) = contact_action(d);
        if let Some(a) = a
            && small_button(ui, ic, pick(en, es)).clicked()
        {
            *out = Some(a);
        }
    });
}

/// «Agregar a contactos» o «Editar contacto» (si su libreta lo permite).
fn contact_action(d: &Data) -> (Icon, &'static str, &'static str, Option<Action>) {
    match &d.contact {
        Some(ct) if d.editable => (Icon::Person, "Edit contact", "Editar contacto", Some(Action::EditContact(ct.id))),
        Some(_) => (Icon::Person, "", "", None),
        None => (Icon::Plus, "Add to contacts", "Agregar a contactos", Some(Action::AddContact(d.mailbox.clone()))),
    }
}

/// Menú contextual del nombre (lo mismo que la tarjeta, en lista).
fn menu(ui: &mut Ui, d: &Data, out: &mut Option<Action>) {
    let addr = d.mailbox.addr.clone();
    let name = d.name();
    let items = [
        (Icon::Send, "Write to", "Escribir a", Action::Write(Mailbox { name: Some(name), addr: addr.clone() })),
        (Icon::Search, "Messages from this person", "Correos de esta persona", Action::Messages(addr.clone())),
        (
            Icon::Check,
            "Select their messages in this list",
            "Seleccionar sus correos en esta lista",
            Action::Select(addr.clone()),
        ),
        (Icon::Mail, "Copy address", "Copiar dirección", Action::Copy(addr.clone())),
        (Icon::Person, "Copy name and address", "Copiar nombre y dirección", Action::Copy(d.full_address())),
    ];
    for (ic, en, es, a) in items {
        if icons::menu_item(ui, Some(ic), pick(en, es)).clicked() {
            *out = Some(a);
            ui.close();
        }
    }
    for p in d.phones() {
        let label = format!("{} {p}", pick("Copy", "Copiar"));
        if icons::menu_item(ui, Some(Icon::Phone), &label).clicked() {
            *out = Some(Action::Copy(p));
            ui.close();
        }
    }
    let (ic, en, es, a) = contact_action(d);
    if let Some(a) = a {
        ui.separator();
        if icons::menu_item(ui, Some(ic), pick(en, es)).clicked() {
            *out = Some(a);
            ui.close();
        }
    }
}

/// Acciones sobre el remitente en el menú contextual de la lista (con su nombre).
pub(crate) fn sender_items(ui: &mut Ui, m: &Mailbox, out: &mut Option<Action>) {
    let who = m.display();
    let items = [
        (Icon::Search, format!("{} {who}", pick("Messages from", "Correos de")), Action::Messages(m.addr.clone())),
        (
            Icon::Check,
            format!("{} {who}", pick("Select all from", "Seleccionar todos los de")),
            Action::Select(m.addr.clone()),
        ),
        (Icon::Mail, format!("{} {}", pick("Copy", "Copiar"), m.addr), Action::Copy(m.addr.clone())),
    ];
    for (ic, label, a) in items {
        if icons::menu_item(ui, Some(ic), &label).clicked() {
            *out = Some(a);
            ui.close();
        }
    }
}

/// Un dato (correo o teléfono) con su botón de copiar.
fn value_row(ui: &mut Ui, ic: Icon, value: &str, out: &mut Option<Action>) {
    let c = theme::colors(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
        icons::paint(ui.painter(), r, ic, c.text_muted, c.accent);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let tip = format!("{} {value}", pick("Copy", "Copiar"));
            if copy_button(ui, &tip).clicked() {
                *out = Some(Action::Copy(value.to_string()));
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add(egui::Label::new(RichText::new(value).color(c.text)).truncate().selectable(true));
            });
        });
    });
}

/// Botón con el icono de copiar (dos hojas), dibujado aquí porque el kit no lo tiene.
fn copy_button(ui: &mut Ui, tip: &str) -> Response {
    let id = egui::IdSalt::new("copiar");
    let side = 14.0;
    let out = egui::Button::new(egui::Atom::custom(id, vec2(side, side))).frame_when_inactive(false).atom_ui(ui);
    if let Some(r) = out.rect(id) {
        let c = theme::colors(ui);
        let ink = if out.response.hovered() { c.accent } else { c.text_muted };
        paint_copy(ui.painter(), r, ink, c.surface);
    }
    iurefficient_ui::a11y::button(&out.response, tip);
    out.response.on_hover_text(tip)
}

/// Dos rectángulos redondeados superpuestos.
fn paint_copy(p: &egui::Painter, r: Rect, ink: egui::Color32, bg: egui::Color32) {
    let s = r.width().min(r.height());
    let w = s * 0.62;
    let stroke = Stroke::new((s / 12.0).max(1.2), ink);
    let back = Rect::from_min_size(r.min, vec2(w, w));
    let front = Rect::from_min_size(r.max - vec2(w, w), vec2(w, w));
    p.rect_stroke(back, s * 0.12, stroke, egui::StrokeKind::Middle);
    p.rect_filled(front, s * 0.12, bg);
    p.rect_stroke(front, s * 0.12, stroke, egui::StrokeKind::Middle);
}

/// Botón pequeño con icono y texto (acciones de la tarjeta).
fn small_button(ui: &mut Ui, ic: Icon, label: &str) -> Response {
    let id = egui::IdSalt::new("icono");
    let c = theme::colors(ui);
    let out = egui::Button::new((egui::Atom::custom(id, vec2(14.0, 14.0)), RichText::new(label).small())).atom_ui(ui);
    if let Some(r) = out.rect(id) {
        icons::paint(ui.painter(), r, ic, c.text, c.accent);
    }
    out.response
}

/// Hace lo pedido.
pub(crate) fn apply(app: &mut DeskApp, ctx: &egui::Context, a: Action) {
    match a {
        Action::Copy(text) => {
            ctx.copy_text(text.clone());
            app.notice(Notice::success(format!("Copied: {text}"), format!("Copiado: {text}")));
        }
        Action::Write(m) => {
            let mut d = app.session.compose_new(None);
            d.draft.to = vec![m];
            app.open_composer(d);
        }
        Action::Messages(addr) => {
            app.run(ctx, &format!("mail.from_sender:{addr}"));
            app.mail.search = app.session.search_query().to_string();
        }
        Action::Select(addr) => app.run(ctx, &format!("mail.select_sender:{addr}")),
        Action::AddContact(m) => crate::contact_editor::open_new_with(app, &m),
        Action::EditContact(id) => crate::contact_editor::open(app, id),
    }
}
