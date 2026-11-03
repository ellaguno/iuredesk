//! UX-10: editor de contactos (doble clic o Intro en la lista, «Editar» en la ficha o el menú,
//! `contacts.edit`) y «Nuevo contacto» (`+` de la lista, Ctrl+N en Contactos, `contacts.new`):
//! nombre, apellidos, «mostrar como», apodo, empresa, cargo, correos, teléfonos, direcciones,
//! cumpleaños y notas. Guarda local primero por `pim-sync`, con deshacer; el motor valida y limpia
//! (`clean_contact`). Ctrl+Intro guarda; Esc cierra.

use chrono::NaiveDate;
use egui::{Align, Context, Layout, RichText, Ui, vec2};
use iuredesk_engine::Notice;
use iuredesk_model::{AddressBook, AddressBookId, Contact, ContactId, Labeled, PostalAddress};
use iurefficient_ui::{Icon, icons, pick, primary_button, theme};

use crate::app::DeskApp;

/// Etiquetas que se ofrecen (las de vCard `TYPE=`); una ajena se conserva tal cual.
const LABELS: [&str; 4] = ["work", "home", "mobile", "other"];

fn label_name(l: &str) -> String {
    match l {
        "work" => pick("Work", "Trabajo").into(),
        "home" => pick("Home", "Casa").into(),
        "mobile" | "cell" => pick("Mobile", "Móvil").into(),
        "other" => pick("Other", "Otro").into(),
        "" => pick("(none)", "(ninguna)").into(),
        other => other.to_string(),
    }
}

/// Estado del editor abierto.
pub(crate) struct ContactEditor {
    original: Contact,
    /// Libretas en las que puede quedar un contacto nuevo (al editar, solo la suya).
    books: Vec<AddressBook>,
    given: String,
    family: String,
    /// «Mostrar como»; `None` = sigue a nombre y apellidos.
    display: Option<String>,
    nickname: String,
    org: String,
    title: String,
    emails: Vec<Labeled>,
    phones: Vec<Labeled>,
    addresses: Vec<PostalAddress>,
    birthday_text: String,
    notes: String,
    book: AddressBookId,
    focused: bool,
}

impl ContactEditor {
    fn new(c: Contact, books: Vec<AddressBook>) -> ContactEditor {
        let auto = [c.given_name.trim(), c.family_name.trim()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let display = (!c.full_name.trim().is_empty() && c.full_name.trim() != auto).then(|| c.full_name.clone());
        let mut emails = c.emails.clone();
        if emails.is_empty() {
            emails.push(Labeled::new("work", ""));
        }
        let mut phones = c.phones.clone();
        if phones.is_empty() {
            phones.push(Labeled::new("mobile", ""));
        }
        ContactEditor {
            books,
            given: c.given_name.clone(),
            family: c.family_name.clone(),
            display,
            nickname: c.nickname.clone(),
            org: c.org.clone(),
            title: c.title.clone(),
            emails,
            phones,
            addresses: c.addresses.clone(),
            birthday_text: c.birthday.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default(),
            notes: c.notes.clone(),
            book: c.address_book,
            original: c,
            focused: false,
        }
    }

    fn is_new(&self) -> bool {
        self.original.id == ContactId::UNASSIGNED
    }

    fn auto_name(&self) -> String {
        [self.given.trim(), self.family.trim()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ")
    }

    fn birthday(&self) -> Result<Option<NaiveDate>, ()> {
        let t = self.birthday_text.trim();
        if t.is_empty() {
            return Ok(None);
        }
        NaiveDate::parse_from_str(t, "%Y-%m-%d").map(Some).map_err(|_| ())
    }

    /// El contacto con lo editado (sin limpiar: eso lo hace el motor).
    fn to_contact(&self) -> Contact {
        let mut c = self.original.clone();
        c.given_name = self.given.clone();
        c.family_name = self.family.clone();
        c.full_name = self.display.clone().filter(|d| !d.trim().is_empty()).unwrap_or_else(|| self.auto_name());
        c.nickname = self.nickname.clone();
        c.org = self.org.clone();
        c.title = self.title.clone();
        c.emails = self.emails.clone();
        c.phones = self.phones.clone();
        c.addresses = self.addresses.clone();
        c.notes = self.notes.clone();
        c.birthday = self.birthday().unwrap_or(self.original.birthday);
        c.address_book = self.book;
        c
    }
}

/// Abre el editor de un contacto existente.
pub(crate) fn open(app: &mut DeskApp, id: ContactId) {
    let Some(c) = app.session.contact(id) else {
        app.notice(Notice::info("That contact no longer exists.", "Ese contacto ya no existe."));
        return;
    };
    match app.session.address_book_of(c.address_book) {
        Some(b) if b.read_only => app.notice(Notice::info(
            format!("«{}» is read-only: change this contact there.", b.name),
            format!("«{}» es de solo lectura: cambia este contacto allá.", b.name),
        )),
        b => {
            let books = b.into_iter().collect();
            app.pim.contact_editor = Some(ContactEditor::new(c, books));
        }
    }
}

/// Abre el editor vacío para un contacto nuevo (en la primera libreta en la que se puede escribir;
/// si no hay ninguna, el motor crea una local).
pub(crate) fn open_new(app: &mut DeskApp) {
    let books = app.session.writable_address_books();
    let c = Contact { address_book: books.first().map(|b| b.id).unwrap_or_default(), ..Default::default() };
    app.pim.contact_editor = Some(ContactEditor::new(c, books));
}

/// ¿Está abierto? (pruebas)
pub(crate) fn is_open(app: &DeskApp) -> bool {
    app.pim.contact_editor.is_some()
}

fn field(ui: &mut Ui, label: &str, text: &mut String, hint: &str) -> egui::Response {
    ui.horizontal(|ui| {
        let c = theme::colors(ui);
        ui.add_sized(vec2(110.0, 22.0), egui::Label::new(RichText::new(label).color(c.text_muted)));
        let r = ui.add(egui::TextEdit::singleline(text).hint_text(hint).desired_width(f32::INFINITY));
        crate::widgets::text_label(&r, label);
        r
    })
    .inner
}

fn small_icon(ui: &mut Ui, icon: Icon) {
    let (rect, _) = ui.allocate_exact_size(vec2(16.0, 16.0), egui::Sense::hover());
    icons::paint_themed(ui, rect, icon);
}

fn label_combo(ui: &mut Ui, id: (&str, usize), label: &mut String) {
    let mut v = label.clone();
    let combo = egui::ComboBox::from_id_salt(id).width(96.0).selected_text(label_name(&v)).show_ui(ui, |ui| {
        for l in LABELS {
            ui.selectable_value(&mut v, l.to_string(), label_name(l));
        }
    });
    crate::widgets::named(&combo.response, egui::Role::ComboBox, pick("Label", "Etiqueta"));
    *label = v;
}

/// Lista de correos o teléfonos: etiqueta, valor, quitar; «Agregar» al pie.
fn labeled_list(ui: &mut Ui, key: &str, icon: Icon, items: &mut Vec<Labeled>, hint: &str, add_label: &str) {
    let mut remove = None;
    for (i, it) in items.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            small_icon(ui, icon);
            label_combo(ui, (key, i), &mut it.label);
            let r = ui.add(
                egui::TextEdit::singleline(&mut it.value).hint_text(hint).desired_width(ui.available_width() - 34.0),
            );
            crate::widgets::text_label(&r, hint);
            if icons::icon_button(ui, Icon::Close, pick("Remove", "Quitar"), true).clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        items.remove(i);
    }
    if ui.button(format!("+ {add_label}")).clicked() {
        let label = if key == "phone" { "mobile" } else { "work" };
        items.push(Labeled::new(label, ""));
    }
}

fn addresses(ui: &mut Ui, items: &mut Vec<PostalAddress>) {
    let mut remove = None;
    for (i, a) in items.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            small_icon(ui, Icon::Pin);
            label_combo(ui, ("addr", i), &mut a.label);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icons::icon_button(ui, Icon::Close, pick("Remove address", "Quitar dirección"), true).clicked() {
                    remove = Some(i);
                }
            });
        });
        ui.indent(("addr-body", i), |ui| {
            let one = |ui: &mut Ui, text: &mut String, hint: &str, w: f32| {
                let r = ui.add(egui::TextEdit::singleline(text).hint_text(hint).desired_width(w));
                crate::widgets::text_label(&r, hint);
            };
            one(ui, &mut a.street, pick("Street and number", "Calle y número"), f32::INFINITY);
            ui.horizontal(|ui| {
                one(ui, &mut a.city, pick("City", "Ciudad"), 150.0);
                one(ui, &mut a.region, pick("State / region", "Estado / región"), 130.0);
                one(ui, &mut a.postal_code, pick("Postal code", "C. P."), 70.0);
            });
            one(ui, &mut a.country, pick("Country", "País"), f32::INFINITY);
        });
    }
    if let Some(i) = remove {
        items.remove(i);
    }
    if ui.button(format!("+ {}", pick("Add address", "Agregar dirección"))).clicked() {
        items.push(PostalAddress { label: "home".into(), ..Default::default() });
    }
}

fn body(ui: &mut Ui, e: &mut ContactEditor) {
    let c = theme::colors(ui);
    ui.spacing_mut().item_spacing.y = 6.0;
    let r = field(ui, pick("First name", "Nombre"), &mut e.given, "");
    if !e.focused {
        e.focused = true;
        r.request_focus();
    }
    field(ui, pick("Last name", "Apellidos"), &mut e.family, "");
    let auto = e.auto_name();
    let mut shown = e.display.clone().unwrap_or_else(|| auto.clone());
    let r = field(ui, pick("Display as", "Mostrar como"), &mut shown, pick("Name and last name", "Nombre y apellidos"));
    if r.changed() {
        e.display = (shown.trim() != auto && !shown.trim().is_empty()).then_some(shown);
    }
    field(ui, pick("Nickname", "Apodo"), &mut e.nickname, "");
    field(ui, pick("Company", "Empresa"), &mut e.org, "");
    field(ui, pick("Job title", "Cargo"), &mut e.title, "");
    if e.is_new() && e.books.len() > 1 {
        ui.horizontal(|ui| {
            ui.add_sized(
                vec2(110.0, 22.0),
                egui::Label::new(RichText::new(pick("Save in", "Guardar en")).color(c.text_muted)),
            );
            let current = e.books.iter().find(|b| b.id == e.book).map(|b| b.name.clone()).unwrap_or_default();
            let books = e.books.clone();
            let combo =
                egui::ComboBox::from_id_salt("contact-book").width(220.0).selected_text(current).show_ui(ui, |ui| {
                    for b in &books {
                        ui.selectable_value(&mut e.book, b.id, &b.name);
                    }
                });
            crate::widgets::named(&combo.response, egui::Role::ComboBox, pick("Address book", "Libreta"));
        });
    }
    ui.add_space(6.0);
    crate::widgets::hairline(ui);
    ui.add_space(4.0);
    labeled_list(ui, "email", Icon::Mail, &mut e.emails, pick("Email", "Correo"), pick("Add email", "Agregar correo"));
    ui.add_space(4.0);
    labeled_list(
        ui,
        "phone",
        Icon::Phone,
        &mut e.phones,
        pick("Phone", "Teléfono"),
        pick("Add phone", "Agregar teléfono"),
    );
    ui.add_space(4.0);
    addresses(ui, &mut e.addresses);
    ui.add_space(6.0);
    crate::widgets::hairline(ui);
    ui.add_space(4.0);
    let bad_birthday = e.birthday().is_err();
    let r = field(ui, pick("Birthday", "Cumpleaños"), &mut e.birthday_text, pick("YYYY-MM-DD", "AAAA-MM-DD"));
    if bad_birthday && !r.has_focus() {
        ui.label(
            RichText::new(pick("Write the date as 1990-05-31.", "Escribe la fecha como 1990-05-31.")).color(c.danger),
        );
    }
    ui.label(RichText::new(pick("Notes", "Notas")).color(c.text_muted));
    let n = ui.add(egui::TextEdit::multiline(&mut e.notes).desired_rows(4).desired_width(f32::INFINITY));
    crate::widgets::text_label(&n, pick("Contact notes", "Notas del contacto"));
}

/// Dibuja el editor si está abierto.
pub(crate) fn show(app: &mut DeskApp, ctx: &Context) {
    let Some(mut e) = app.pim.contact_editor.take() else { return };
    let mut open = true;
    let mut action: Option<&'static str> = None;
    let title = if e.is_new() { pick("New contact", "Nuevo contacto") } else { pick("Contact", "Contacto") };
    egui::Window::new(title)
        .id(egui::Id::unique("contact-editor"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(540.0)
        .max_height(ctx.content_rect().height() - 40.0)
        .default_pos(ctx.content_rect().center() - vec2(270.0, 300.0))
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, true])
                .max_height(ctx.content_rect().height() - 160.0)
                .show(ui, |ui| body(ui, &mut e));
            ui.separator();
            ui.horizontal(|ui| {
                if !e.is_new() {
                    let c = theme::colors(ui);
                    if ui.button(RichText::new(pick("Delete", "Eliminar")).color(c.danger)).clicked() {
                        action = Some("delete");
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if primary_button(ui, pick("Save", "Guardar")).clicked() {
                        action = Some("save");
                    }
                    if ui.button(pick("Cancel", "Cancelar")).clicked() {
                        action = Some("cancel");
                    }
                });
            });
            if ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command) {
                action = Some("save");
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                action = Some("cancel");
            }
        });
    if !open {
        action = Some("cancel");
    }
    match action {
        Some("cancel") => {}
        Some("save") => {
            if e.birthday().is_err() {
                app.notice(Notice::error(iuredesk_engine::Text::new(
                    "Write the birthday as 1990-05-31, or leave it empty.",
                    "Escribe el cumpleaños como 1990-05-31, o déjalo vacío.",
                )));
                app.pim.contact_editor = Some(e);
                return;
            }
            match app.session.save_contact_edit(e.to_contact()) {
                Ok(iuredesk_engine::Outcome::Notice(n)) if n.kind == iuredesk_engine::NoticeKind::Error => {
                    app.notice(n);
                    app.pim.contact_editor = Some(e);
                }
                Ok(out) => {
                    // El motor deja seleccionado el guardado: que la lista lo muestre.
                    if let Some(&id) = app.session.contact_selection().first() {
                        if e.is_new() {
                            app.pim.contact_filter.clear();
                        }
                        app.pim.reveal_contact = Some(id);
                    }
                    app.outcome(ctx, out);
                }
                Err(err) => {
                    app.notice(Notice::error(err.text()));
                    app.pim.contact_editor = Some(e);
                }
            }
        }
        Some("delete") => {
            app.session.set_contact_selection(vec![e.original.id]);
            app.run(ctx, "contacts.delete");
        }
        _ => app.pim.contact_editor = Some(e),
    }
}
