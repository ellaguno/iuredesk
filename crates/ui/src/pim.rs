//! Contactos (lista + ficha), notas (lista + texto con formato Markdown sencillo), tareas (listas
//! con casillas) y el asistente (llega en F3).

use egui::{Align, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use iuredesk_model::{ContactId, NoteId, Task, TaskListId};
use iurefficient_ui::list::{Column, ListState, ListView};
use iurefficient_ui::{
    BadgeKind, EmptyState, Icon, PaneKind, avatar, badge, fuzzy, icons, pick, section_label, sidebar_item, split, theme,
};

use crate::app::DeskApp;
use crate::widgets::{hairline, info_row, pane_title};

/// Estado de contactos, notas y tareas.
#[derive(Default)]
pub struct PimState {
    pub(crate) contact: Option<ContactId>,
    pub(crate) contact_filter: String,
    contact_list: ListState,
    contact_cols: Vec<Column>,
    pub(crate) note_search: String,
    pub(crate) note_edit: Option<NoteEdit>,
    pub(crate) note_preview: bool,
    note_focus_editor: bool,
    pub(crate) task_list: Option<TaskListId>,
    show_done: bool,
    /// F7.1: tarea nueva que se está escribiendo.
    new_task: String,
    /// UX-6: tarea elegida en la lista, título en edición (F2) y editor abierto.
    pub(crate) task_sel: Option<iuredesk_model::TaskId>,
    pub(crate) task_rename: Option<TaskRename>,
    pub(crate) task_editor: Option<crate::task_editor::TaskEditor>,
}

// ---------------------------------------------------------------------------------------------
// Contactos

/// Nombre de la fila: el del contacto o, si no tiene ninguno, «(Sin nombre)» (en cursiva atenuada).
fn display_name(c: &iuredesk_model::Contact) -> (String, bool) {
    match iuredesk_engine::session::contact_display_name(c) {
        Some(n) => (n, true),
        None => (pick("(No name)", "(Sin nombre)").to_string(), false),
    }
}

pub fn contacts(app: &mut DeskApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let everyone = app.session.contacts().to_vec();
    let show_empty = app.session.prefs.show_empty_contacts;
    let empty_count = everyone.iter().filter(|c| iuredesk_engine::session::contact_is_empty(c)).count();
    let all: Vec<iuredesk_model::Contact> = if show_empty {
        everyone
    } else {
        everyone.into_iter().filter(|c| !iuredesk_engine::session::contact_is_empty(c)).collect()
    };
    let filter = app.pim.contact_filter.trim().to_string();
    let shown: Vec<usize> = if filter.is_empty() {
        (0..all.len()).collect()
    } else {
        let mut v: Vec<(i32, usize)> = all
            .iter()
            .enumerate()
            .filter_map(|(i, c)| {
                let extra = format!(
                    "{} {} {} {}",
                    c.org,
                    c.emails.first().map(|e| e.value.as_str()).unwrap_or(""),
                    c.title,
                    c.phones.first().map(|p| p.value.as_str()).unwrap_or("")
                );
                fuzzy::score_with_extra(&filter, &display_name(c).0, &extra).map(|s| (s, i))
            })
            .collect();
        v.sort_by_key(|(s, i)| (std::cmp::Reverse(*s), *i));
        v.into_iter().map(|(_, i)| i).collect()
    };
    split::left(ui, "contacts-list", PaneKind::Sidebar, 320.0, 240.0..=480.0, |ui| {
        ui.add_space(4.0);
        let selected_n = app.pim.contact_list.selection.len();
        ui.horizontal(|ui| {
            pane_title(ui, pick("Contacts", "Contactos"), &all.len().to_string());
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                let tip = if selected_n > 1 {
                    format!("{} ({selected_n})", pick("Delete selected", "Eliminar seleccionados"))
                } else {
                    pick("Delete contact", "Eliminar contacto").to_string()
                };
                let r = icons::icon_button(ui, Icon::Trash, &tip, selected_n > 0);
                if r.clicked() {
                    app.run(&ctx, "contacts.delete");
                }
            });
        });
        ui.add_space(6.0);
        if app.pim.contact_cols.is_empty() {
            app.pim.contact_cols = vec![Column::new(pick("Name", "Nombre"), 200.0).grow()];
        }
        if shown.is_empty() {
            EmptyState::new(Icon::Contacts, pick("No contacts", "Sin contactos"))
                .body(pick(
                    "People you write to will appear here.",
                    "Las personas con quienes te escribes aparecerán aquí.",
                ))
                .show(ui);
            return;
        }
        if app.pim.contact_list.selection.is_empty()
            && let Some(ct) = shown.first().and_then(|&k| all.get(k))
        {
            app.pim.contact_list.selection.select_only(0, ct.id.get());
        }
        let selected = app.pim.contact;
        let shown_k = shown.clone();
        let all_k = all.clone();
        // Deja sitio al pie para «Mostrar contactos vacíos (N)».
        let footer = if empty_count > 0 { 30.0 } else { 0.0 };
        let list_h = (ui.available_height() - footer).max(60.0);
        let list_rect = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), list_h));
        let mut list_ui =
            ui.new_child(egui::UiBuilder::new().max_rect(list_rect).layout(egui::Layout::top_down(Align::Min)));
        let out = ListView::new("contacts", &mut app.pim.contact_cols, shown.len())
            .header(false)
            .row_height(46.0)
            .label(pick("Contacts", "Contactos"))
            .show(
                &mut list_ui,
                &mut app.pim.contact_list,
                move |i| shown_k.get(i).and_then(|&k| all_k.get(k)).map_or(u64::MAX, |c| c.id.get()),
                |row| {
                    let Some(ct) = shown.get(row.index).and_then(|&k| all.get(k)) else { return };
                    let col = *row.colors();
                    let r = row.rect;
                    iurefficient_ui::widgets::avatar::paint_avatar(
                        row.painter(),
                        pos2(r.left() + 24.0, r.center().y),
                        30.0,
                        &ct.full_name,
                        ct.primary_email().unwrap_or(""),
                    );
                    let x = r.left() + 48.0;
                    let l1 = Rect::from_min_max(pos2(x, r.top() + 4.0), pos2(r.right() - 8.0, r.center().y + 1.0));
                    let l2 = Rect::from_min_max(pos2(x, r.center().y - 1.0), pos2(r.right() - 8.0, r.bottom() - 4.0));
                    let (name, real) = display_name(ct);
                    if real {
                        let bold = theme::bold_font(&ctx, theme::density(&ctx).body_size());
                        row.text_in(l1, Align::Min, &name, bold, col.text);
                    } else {
                        let mut job = egui::text::LayoutJob::single_section(
                            name.clone(),
                            egui::TextFormat {
                                font_id: theme::body_font(&ctx),
                                color: col.text_muted,
                                italics: true,
                                ..Default::default()
                            },
                        );
                        job.wrap.max_width = l1.width();
                        job.wrap.max_rows = 1;
                        let g = row.painter().layout_job(job);
                        let y = l1.center().y - g.size().y / 2.0;
                        row.painter().galley(pos2(l1.left(), y), g, col.text_muted);
                        row.describe(&name);
                    }
                    // Segunda línea: empresa, correo o teléfono que no sea ya el nombre.
                    let phone = ct.phones.first().map_or("", |p| p.value.as_str());
                    let sub = [ct.org.as_str(), ct.primary_email().unwrap_or(""), phone]
                        .into_iter()
                        .map(str::trim)
                        .find(|s| !s.is_empty() && *s != name)
                        .unwrap_or("")
                        .to_string();
                    row.text_in(l2, Align::Min, &sub, theme::small_font(&ctx), col.text_muted);
                    let _ = selected;
                },
            );
        if out.selection_changed
            && let Some(i) = app.pim.contact_list.selection.cursor()
            && let Some(ct) = shown.get(i).and_then(|&k| all.get(k))
        {
            app.pim.contact = Some(ct.id);
        }
        // La selección (múltiple) va al motor: «Eliminar» y Supr actúan sobre ella.
        let ids: Vec<ContactId> = shown
            .iter()
            .filter_map(|&k| all.get(k))
            .filter(|c| app.pim.contact_list.selection.is_selected(c.id.get()))
            .map(|c| c.id)
            .collect();
        if ids.as_slice() != app.session.contact_selection() {
            app.session.set_contact_selection(ids);
        }
        if out.context.is_some() {
            contact_menu(app, &out.response);
        }
        // Supr (o Retroceso) con la lista enfocada: la lista se queda con el teclado, así que los
        // atajos de una tecla de la aplicación no llegan; se atiende aquí.
        if out.response.has_focus()
            && ui.input_mut(|i| {
                i.consume_key(egui::Modifiers::NONE, egui::Key::Delete)
                    || i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace)
            })
        {
            app.run(&ctx, "contacts.delete");
        }
        ui.advance_cursor_after_rect(list_rect);
        if empty_count > 0 {
            let c = theme::colors(ui);
            let label = if show_empty {
                format!("{} ({empty_count})", pick("Hide empty contacts", "Ocultar contactos vacíos"))
            } else {
                format!("{} ({empty_count})", pick("Show empty contacts", "Mostrar contactos vacíos"))
            };
            let r = ui.add(
                egui::Button::new(RichText::new(label).color(c.text_muted).size(theme::density(&ctx).small_size()))
                    .frame(false),
            );
            if r.clicked() {
                app.session.prefs.show_empty_contacts = !show_empty;
                app.session.save_prefs();
                app.pim.contact_list.selection.clear();
            }
        }
    });
    let current = app
        .pim
        .contact
        .and_then(|id| all.iter().find(|c| c.id == id))
        .or_else(|| shown.first().and_then(|&k| all.get(k)));
    split::center(ui, PaneKind::Detail, |ui| {
        let Some(ct) = current else {
            EmptyState::new(Icon::Person, pick("Select a contact", "Selecciona un contacto")).show(ui);
            return;
        };
        let c = theme::colors(ui);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width(640.0);
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 16.0;
                avatar(ui, &ct.full_name, ct.primary_email().unwrap_or(""), 72.0);
                ui.vertical(|ui| {
                    ui.add_space(6.0);
                    let (name, real) = display_name(ct);
                    let title = RichText::new(name).font(theme::bold_font(ui.ctx(), 22.0));
                    ui.label(if real { title.color(c.text) } else { title.italics().color(c.text_muted) });
                    let role = [ct.title.as_str(), ct.org.as_str()]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join(" · ");
                    if !role.is_empty() {
                        ui.label(RichText::new(role).color(c.text_muted));
                    }
                });
            });
            ui.add_space(14.0);
            let email = ct.primary_email().map(str::to_string);
            ui.horizontal(|ui| {
                if let Some(e) = &email
                    && iurefficient_ui::primary_button(ui, pick("Write", "Escribir")).clicked()
                {
                    {
                        let mut d = app.session.compose_new(None);
                        d.draft.to = iuredesk_engine::compose::lenient_addresses(e);
                        app.open_composer(d);
                    }
                }
                if let Some(e) = &email
                    && ui.button(pick("Show messages", "Ver mensajes")).clicked()
                {
                    let q = format!("de:{e}");
                    app.mail.search = q.clone();
                    app.session.set_search(&q);
                }
                if app.session.contact_deletable(ct.id)
                    && ui
                        .button(pick("Delete", "Eliminar"))
                        .on_hover_text(pick("Delete this contact", "Eliminar este contacto"))
                        .clicked()
                {
                    app.session.set_contact_selection(vec![ct.id]);
                    let ctx = ui.ctx().clone();
                    app.run(&ctx, "contacts.delete");
                }
            });
            ui.add_space(14.0);
            hairline(ui);
            ui.add_space(10.0);
            for e in &ct.emails {
                info_row(ui, Icon::Mail, &label_kind(&e.label), &e.value);
                ui.add_space(6.0);
            }
            for p in &ct.phones {
                info_row(ui, Icon::Phone, &label_kind(&p.label), &p.value);
                ui.add_space(6.0);
            }
            for a in &ct.addresses {
                let text = [a.street.as_str(), a.city.as_str(), a.region.as_str(), a.country.as_str()]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(", ");
                info_row(ui, Icon::Pin, &label_kind(&a.label), &text);
                ui.add_space(6.0);
            }
            if let Some(b) = ct.birthday {
                info_row(ui, Icon::Calendar, pick("Birthday", "Cumpleaños"), &b.format("%d/%m/%Y").to_string());
                ui.add_space(6.0);
            }
            if !ct.notes.is_empty() {
                info_row(ui, Icon::Notes, pick("Notes", "Notas"), &ct.notes);
            }
        });
    });
}

/// Menú contextual de la lista de contactos.
fn contact_menu(app: &mut DeskApp, resp: &egui::Response) {
    let ctx = resp.ctx.clone();
    let mut delete = false;
    let n = app.pim.contact_list.selection.len();
    egui::Popup::context_menu(resp).show(|ui| {
        let label = if n > 1 {
            format!("{} ({n})", pick("Delete", "Eliminar"))
        } else {
            pick("Delete", "Eliminar").to_string()
        };
        if icons::menu_item(ui, Some(Icon::Trash), &label).clicked() {
            delete = true;
            ui.close();
        }
    });
    if delete {
        app.run(&ctx, "contacts.delete");
    }
}

fn label_kind(l: &str) -> String {
    match l {
        "work" => pick("Work", "Trabajo").into(),
        "home" => pick("Home", "Casa").into(),
        "mobile" => pick("Mobile", "Móvil").into(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------------------------
// Notas

/// Nota abierta en el editor (F7.1).
#[derive(Clone, Debug)]
pub(crate) struct NoteEdit {
    id: NoteId,
    title: String,
    body: String,
    /// Momento (reloj de egui) del último cambio sin guardar.
    dirty_at: Option<f64>,
    /// UX-6: fecha de la nota al cargarla (si cambia por debajo, llegó algo de IureEditor).
    base: chrono::DateTime<chrono::Utc>,
}

impl NoteEdit {
    fn of(n: &iuredesk_model::Note) -> NoteEdit {
        NoteEdit { id: n.id, title: n.title.clone(), body: n.body.clone(), dirty_at: None, base: n.updated }
    }
}

/// Espera tras la última tecla antes del guardado automático.
const NOTE_AUTOSAVE_SECS: f64 = 0.7;

/// Guarda la nota abierta si tiene cambios (al cambiar de nota o de módulo, o al cumplirse la
/// espera).
pub(crate) fn flush_note(app: &mut DeskApp) {
    let Some(e) = app.pim.note_edit.as_mut() else { return };
    if e.dirty_at.take().is_none() {
        return;
    }
    let (id, title, body, base) = (e.id, e.title.clone(), e.body.clone(), e.base);
    match app.session.save_note_from(id, &title, &body, Some(base)) {
        Ok(iuredesk_engine::session::NoteSaved::Saved(at)) => {
            if let Some(e) = app.pim.note_edit.as_mut().filter(|e| e.id == id) {
                e.base = at;
            }
        }
        Ok(iuredesk_engine::session::NoteSaved::Copied(_)) => {
            // Lo de aquí quedó en la copia en conflicto; el editor muestra lo de IureEditor.
            if let Some(n) = app.session.notes().into_iter().find(|n| n.id == id) {
                app.pim.note_edit = Some(NoteEdit::of(&n));
            }
            app.notice(iuredesk_engine::Notice::info(
                "This note changed in IureEditor while you were typing here: what you wrote is in the conflicting copy.",
                "Esta nota cambió en IureEditor mientras escribías aquí: lo que escribiste está en la copia en conflicto.",
            ));
        }
        Err(err) => app.notice(iuredesk_engine::Notice::error(err.text())),
    }
}

/// UX-6: «Editar en IureEditor»: abre el archivo `.md` de la nota (`iureditor://open?path=`); lo
/// que se guarde allá vuelve solo. Sin carpeta de notas (o sin texto aún), un documento nuevo.
pub(crate) fn open_note_in_editor(app: &mut DeskApp) {
    flush_note(app);
    let Some(id) = app.session.note_focus() else { return };
    let Some(n) = app.session.notes().into_iter().find(|n| n.id == id) else { return };
    if !app.suite.hub.installed(iuredesk_engine::iuredesk_integrations::SisterApp::Editor) {
        // La invitación a instalarla (la da el propio `run`).
        app.suite.hub.run(iuredesk_engine::suite::SuiteAction::OpenFile(std::path::PathBuf::new()));
        return;
    }
    if let Some(path) = app.session.note_path(id) {
        app.suite.hub.run(iuredesk_engine::suite::SuiteAction::OpenFile(path));
        return;
    }
    if n.title.trim().is_empty() && n.body.trim().is_empty() {
        app.notice(iuredesk_engine::Notice::info(
            "Write a title or some text first.",
            "Primero escribe un título o algo de texto.",
        ));
        return;
    }
    let title = iuredesk_engine::session::note_title(&n);
    let name = iuredesk_engine::notes_fs::file_stem(&title);
    let markdown = iuredesk_engine::notes_fs::render(&n.title, &n.body, &n.tags, n.pinned);
    app.suite.hub.run(iuredesk_engine::suite::SuiteAction::NewDocument { name: format!("{name}.md"), markdown });
}

pub fn notes(app: &mut DeskApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let notes = app.session.notes_matching(&app.pim.note_search);
    let total = app.session.notes().len();
    let zone = app.session.zone();
    let now = app.session.now();
    let focus = app.session.note_focus().filter(|id| notes.iter().any(|n| n.id == *id)).or(notes.first().map(|n| n.id));
    let mut pick_note: Option<NoteId> = None;
    let mut fire: Option<&'static str> = None;
    let editor_installed = app.suite.hub.installed(iuredesk_engine::iuredesk_integrations::SisterApp::Editor);
    split::left(ui, "notes-list", PaneKind::Sidebar, 300.0, 220.0..=460.0, |ui| {
        let c = theme::colors(ui);
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            pane_title(ui, pick("Notes", "Notas"), &total.to_string());
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if icons::icon_button(ui, Icon::Plus, pick("New note", "Nueva nota"), true).clicked() {
                    fire = Some("notes.new");
                }
            });
        });
        ui.add_space(6.0);
        let r = ui.add(
            egui::TextEdit::singleline(&mut app.pim.note_search)
                .hint_text(pick("Search notes", "Buscar en las notas"))
                .desired_width(f32::INFINITY),
        );
        crate::widgets::text_label(&r, pick("Search notes", "Buscar en las notas"));
        ui.add_space(8.0);
        if notes.is_empty() {
            let (title, body) = if app.pim.note_search.trim().is_empty() {
                (
                    pick("No notes yet", "Aún no hay notas"),
                    pick("Write down ideas, minutes and checklists.", "Apunta ideas, minutas y listas."),
                )
            } else {
                (pick("Nothing found", "Sin resultados"), pick("Try other words.", "Prueba con otras palabras."))
            };
            EmptyState::new(Icon::Notes, title).body(body).show(ui);
            if app.pim.note_search.trim().is_empty() && ui.button(pick("New note", "Nueva nota")).clicked() {
                fire = Some("notes.new");
            }
            return;
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for n in &notes {
                let selected = focus == Some(n.id);
                // Lo que se está escribiendo se ve ya en la lista.
                let shown = match &app.pim.note_edit {
                    Some(e) if e.id == n.id => {
                        iuredesk_model::Note { title: e.title.clone(), body: e.body.clone(), ..n.clone() }
                    }
                    _ => n.clone(),
                };
                let h = 64.0;
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
                if selected {
                    ui.painter().rect_filled(rect, 8.0, c.selection);
                } else if resp.hovered() {
                    ui.painter().rect_filled(rect, 8.0, c.hover);
                }
                let inner = rect.shrink2(vec2(10.0, 8.0));
                let title_font = theme::bold_font(ui.ctx(), theme::density(ui.ctx()).body_size());
                let date = iuredesk_engine::time::list_date(n.updated, now, zone);
                let gd = ui.painter().layout_no_wrap(date, theme::small_font(ui.ctx()), c.text_muted);
                let dw = gd.size().x;
                ui.painter().galley(pos2(inner.right() - dw, inner.top() + 1.0), gd, c.text_muted);
                let mut title = iuredesk_engine::session::note_title(&shown);
                if n.pinned {
                    title = format!("{title}  ·  {}", pick("pinned", "fijada"));
                }
                let gt =
                    iurefficient_ui::widgets::text::elided(ui, &title, title_font, c.text, inner.width() - dw - 8.0);
                ui.painter().galley(inner.min, gt, c.text);
                let snippet: String = shown
                    .body
                    .lines()
                    .map(|l| l.trim_start_matches(['#', '-', '*', ' ', '>']).replace('*', ""))
                    .filter(|l| !l.is_empty())
                    .take(6)
                    .collect::<Vec<_>>()
                    .join(" ");
                let gs = iurefficient_ui::widgets::text::elided(
                    ui,
                    &snippet,
                    theme::body_font(ui.ctx()),
                    c.text_muted,
                    inner.width(),
                );
                ui.painter().galley(pos2(inner.left(), inner.top() + 24.0), gs, c.text_muted);
                iurefficient_ui::a11y::toggle(&resp, &title, selected);
                if resp.clicked() {
                    pick_note = Some(n.id);
                }
                // UX-6: doble clic = editar en IureEditor (si está instalado).
                if resp.double_clicked() && editor_installed {
                    pick_note = Some(n.id);
                    fire = Some("notes.open_editor");
                }
                egui::Popup::context_menu(&resp).show(|ui| {
                    let pin = if n.pinned { pick("Unpin", "Soltar") } else { pick("Pin to top", "Fijar arriba") };
                    if icons::menu_item(ui, Some(Icon::Pin), pin).clicked() {
                        let _ = app.session.toggle_note_pin(n.id);
                        ui.close();
                    }
                    if icons::menu_item(ui, Some(Icon::Trash), pick("Delete note", "Eliminar nota")).clicked() {
                        pick_note = Some(n.id);
                        fire = Some("notes.delete");
                        ui.close();
                    }
                });
                ui.add_space(2.0);
            }
        });
    });
    if let Some(id) = pick_note.or(focus)
        && app.pim.note_edit.as_ref().is_none_or(|e| e.id != id)
    {
        flush_note(app);
        app.session.set_note_focus(Some(id));
        app.pim.note_edit = notes.iter().find(|n| n.id == id).map(NoteEdit::of);
        // Nota nueva (vacía): se escribe enseguida.
        app.pim.note_focus_editor = notes.iter().any(|n| n.id == id && n.title.is_empty() && n.body.is_empty());
    }
    if focus.is_none() && pick_note.is_none() {
        flush_note(app);
        app.pim.note_edit = None;
    }
    if let Some(id) = fire {
        flush_note(app);
        app.run(&ctx, id);
        return;
    }
    // UX-6: la nota cambió por debajo (IureEditor, otra copia) y aquí no hay nada sin guardar:
    // se muestra lo nuevo.
    if let Some(e) = app.pim.note_edit.as_ref()
        && e.dirty_at.is_none()
        && let Some(n) = notes.iter().find(|n| n.id == e.id && n.updated != e.base)
    {
        app.pim.note_edit = Some(NoteEdit::of(n));
    }
    let updated = app.pim.note_edit.as_ref().and_then(|e| notes.iter().find(|n| n.id == e.id)).map(|n| n.updated);
    let file_name = app
        .pim
        .note_edit
        .as_ref()
        .and_then(|e| notes.iter().find(|n| n.id == e.id))
        .and_then(|n| n.file.as_ref().map(|f| f.name.clone()));
    let notes_dir = app.session.notes_dir();
    let dir_problem = app.session.notes_dir_problem().map(str::to_string);
    split::center(ui, PaneKind::Detail, |ui| {
        let c = theme::colors(ui);
        let Some(edit) = app.pim.note_edit.as_mut() else {
            EmptyState::new(Icon::Notes, pick("Select a note", "Selecciona una nota")).show(ui);
            return;
        };
        let mut act: Option<&'static str> = None;
        ui.set_max_width(820.0);
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let preview = app.pim.note_preview;
            if ui.selectable_label(!preview, pick("Edit", "Editar")).clicked() {
                app.pim.note_preview = false;
            }
            if ui.selectable_label(preview, pick("Preview", "Vista previa")).clicked() {
                app.pim.note_preview = true;
            }
            ui.add_space(8.0);
            let state = match (edit.dirty_at.is_some(), updated) {
                (true, _) => pick("Saving…", "Guardando…").to_string(),
                (false, Some(u)) => {
                    format!("{} {}", pick("Saved", "Guardada"), iuredesk_engine::time::list_date(u, now, zone))
                }
                (false, None) => String::new(),
            };
            ui.label(RichText::new(state).color(c.text_muted).size(theme::density(ui.ctx()).small_size()));
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if icons::icon_button(ui, Icon::Trash, pick("Delete note", "Eliminar nota"), true).clicked() {
                    act = Some("notes.delete");
                }
                // UX-6: la forma principal de editar una nota larga.
                let label = pick("Edit in IureEditor", "Editar en IureEditor");
                let tip = if editor_installed {
                    pick(
                        "Opens the note's Markdown file in IureEditor. What you save there comes back here.",
                        "Abre el archivo Markdown de la nota en IureEditor. Lo que guardes allá vuelve aquí.",
                    )
                } else {
                    pick(
                        "IureEditor is not installed: click to see how to get it. You can keep writing here.",
                        "IureEditor no está instalado: pulsa para ver cómo obtenerlo. Puedes seguir escribiendo aquí.",
                    )
                };
                if ui.button(label).on_hover_text(tip).clicked() {
                    act = Some("notes.open_editor");
                }
            });
        });
        // UX-6: dónde vive la nota.
        // (texto, ruta completa para la ayuda, ¿problema?)
        let where_ = match (&notes_dir, &file_name, &dir_problem) {
            (Some(_), _, Some(p)) => Some((
                format!(
                    "{} ({p})",
                    pick(
                        "Notes folder not available; the note is safe here",
                        "La carpeta de notas no está disponible; la nota está a salvo aquí"
                    )
                ),
                String::new(),
                true,
            )),
            (Some(d), Some(f), None) => {
                let folder = d.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                Some((format!("{} {folder}/{f}", pick("File:", "Archivo:")), d.join(f).display().to_string(), false))
            }
            _ => None,
        };
        if let Some((text, full, bad)) = where_ {
            let r = ui.label(
                RichText::new(text)
                    .color(if bad { c.danger } else { c.text_muted })
                    .size(theme::density(ui.ctx()).small_size()),
            );
            if !full.is_empty() {
                r.on_hover_text(full);
            }
        }
        ui.add_space(6.0);
        hairline(ui);
        ui.add_space(8.0);
        let mut changed = false;
        let title = ui.add(
            egui::TextEdit::singleline(&mut edit.title)
                .font(theme::bold_font(ui.ctx(), 24.0))
                .hint_text(pick("Title", "Título"))
                .frame(egui::Frame::NONE)
                .desired_width(f32::INFINITY),
        );
        crate::widgets::text_label(&title, pick("Note title", "Título de la nota"));
        changed |= title.changed();
        ui.add_space(8.0);
        if app.pim.note_preview {
            egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("note-preview").show(ui, |ui| {
                markdown(ui, &edit.body);
            });
        } else {
            egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("note-body").show(ui, |ui| {
                let body = ui.add(
                    egui::TextEdit::multiline(&mut edit.body)
                        .hint_text(pick(
                            "Write in Markdown: # heading, - list, - [ ] checkbox, **bold**…",
                            "Escribe en Markdown: # título, - lista, - [ ] casilla, **negrita**…",
                        ))
                        .font(theme::body_font(ui.ctx()))
                        .frame(egui::Frame::NONE)
                        .lock_focus(true)
                        .desired_rows(24)
                        .desired_width(f32::INFINITY),
                );
                crate::widgets::text_label(&body, pick("Note text", "Texto de la nota"));
                if app.pim.note_focus_editor {
                    app.pim.note_focus_editor = false;
                    title.request_focus();
                }
                changed |= body.changed();
            });
        }
        let t = ui.input(|i| i.time);
        if changed {
            edit.dirty_at = Some(t);
        }
        if let Some(at) = edit.dirty_at {
            if t - at >= NOTE_AUTOSAVE_SECS {
                flush_note(app);
            } else {
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(NOTE_AUTOSAVE_SECS));
            }
        }
        if let Some(id) = act {
            flush_note(app);
            app.run(&ctx, id);
        }
    });
}

/// Markdown sencillo: encabezados, viñetas, casillas, citas, negritas y párrafos.
fn markdown(ui: &mut Ui, text: &str) {
    let c = theme::colors(ui);
    let body = theme::density(ui.ctx()).body_size() + 1.0;
    for line in text.lines().take(5_000) {
        let t = line.trim_end();
        if t.trim().is_empty() {
            ui.add_space(6.0);
            continue;
        }
        if let Some(h) = t.strip_prefix("### ") {
            ui.add_space(6.0);
            ui.label(RichText::new(h).font(theme::bold_font(ui.ctx(), body + 2.0)).color(c.text));
        } else if let Some(h) = t.strip_prefix("## ") {
            ui.add_space(8.0);
            ui.label(RichText::new(h).font(theme::bold_font(ui.ctx(), body + 4.0)).color(c.text));
        } else if let Some(h) = t.strip_prefix("# ") {
            ui.add_space(10.0);
            ui.label(RichText::new(h).font(theme::bold_font(ui.ctx(), body + 7.0)).color(c.text));
        } else if let Some(rest) = t.strip_prefix("- [ ] ").or_else(|| t.strip_prefix("- [x] ")) {
            let done = t.starts_with("- [x]");
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                ui.painter().rect_stroke(r.shrink(1.5), 3.0, Stroke::new(1.3, c.text_muted), egui::StrokeKind::Inside);
                if done {
                    icons::paint(ui.painter(), r.shrink(2.0), Icon::Check, c.accent, c.accent);
                }
                inline(ui, rest, body, if done { c.text_muted } else { c.text });
            });
        } else if let Some(rest) = t.trim_start().strip_prefix("- ").or_else(|| t.trim_start().strip_prefix("* ")) {
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                let (r, _) = ui.allocate_exact_size(vec2(10.0, body), Sense::hover());
                ui.painter().circle_filled(r.center(), 2.5, c.text_muted);
                inline(ui, rest, body, c.text);
            });
        } else if let Some(q) = t.strip_prefix("> ") {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(3.0, body * 1.4), Sense::hover());
                ui.painter().rect_filled(r, 1.0, c.separator);
                ui.add_space(6.0);
                inline(ui, q, body, c.text_muted);
            });
        } else {
            inline(ui, t, body, c.text);
        }
    }
}

/// Texto con `**negritas**`.
fn inline(ui: &mut Ui, text: &str, size: f32, color: egui::Color32) {
    let mut job = egui::text::LayoutJob::default();
    for (i, part) in text.split("**").enumerate() {
        let font = if i % 2 == 1 { theme::bold_font(ui.ctx(), size) } else { egui::FontId::proportional(size) };
        job.append(
            part,
            0.0,
            egui::TextFormat { font_id: font, color, line_height: Some(size * 1.45), ..Default::default() },
        );
    }
    job.wrap.max_width = ui.available_width();
    ui.add(egui::Label::new(job).wrap().selectable(true));
}

// ---------------------------------------------------------------------------------------------
// Tareas

/// Qué se hizo sobre una fila de tarea.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowAct {
    Toggle,
    Delete,
    Select,
    /// UX-6: abrir el editor (doble clic, Intro, «Editar…»).
    Edit,
    /// UX-6: cambiar el nombre en la lista (F2).
    Rename,
}

pub fn tasks(app: &mut DeskApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let lists = app.session.task_lists();
    let selected = app.pim.task_list.or(lists.first().map(|l| l.id));
    split::left(ui, "task-lists", PaneKind::Sidebar, 244.0, 190.0..=360.0, |ui| {
        ui.add_space(6.0);
        section_label(ui, pick("Lists", "Listas"));
        ui.add_space(2.0);
        ui.spacing_mut().item_spacing.y = 1.0;
        for l in &lists {
            let open = app.session.tasks(l.id).iter().filter(|t| !t.is_done()).count();
            let r =
                sidebar_item(ui, Some(Icon::Tasks), &l.name, u32::try_from(open).unwrap_or(0), selected == Some(l.id));
            if r.clicked() {
                app.pim.task_list = Some(l.id);
            }
            // F7.1: de qué cuenta es (Google Tasks, To Do, CalDAV, Iurefficient).
            if let Some(b) = app.session.task_list_badge(l.id) {
                r.on_hover_text(&b);
                ui.horizontal(|ui| {
                    ui.add_space(34.0);
                    let c = theme::colors(ui);
                    ui.label(RichText::new(b).color(c.text_muted).size(theme::density(ui.ctx()).small_size()));
                });
            }
        }
    });
    split::center(ui, PaneKind::Detail, |ui| {
        let c = theme::colors(ui);
        let Some(list_id) = selected else {
            EmptyState::new(Icon::Tasks, pick("No task lists", "No hay listas de tareas"))
                .body(pick("Tasks from your accounts will appear here.", "Las tareas de tus cuentas aparecerán aquí."))
                .show(ui);
            return;
        };
        let name = lists.iter().find(|l| l.id == list_id).map(|l| l.name.clone()).unwrap_or_default();
        let all = app.session.tasks(list_id);
        let (done, open): (Vec<Task>, Vec<Task>) = all.into_iter().partition(|t| t.is_done());
        pane_title(ui, &name, &format!("{} {}", open.len(), pick("pending", "pendientes")));
        if let Some(b) = app.session.task_list_badge(list_id) {
            badge(ui, &b, BadgeKind::Neutral);
        }
        ui.add_space(8.0);
        let mut acts: Vec<(iuredesk_model::TaskId, RowAct)> = Vec::new();
        let writable = app.session.iure_can_write() || !app.session.is_iure_task(list_id);
        // F7.1: alta rápida (Intro); se sube a la cuenta de la lista en segundo plano.
        if writable {
            ui.horizontal(|ui| {
                ui.set_max_width(760.0);
                let r = ui.add(
                    egui::TextEdit::singleline(&mut app.pim.new_task)
                        .hint_text(pick("Add a task and press Enter", "Agrega una tarea y pulsa Intro"))
                        .desired_width(f32::INFINITY),
                );
                crate::widgets::text_label(&r, pick("New task", "Tarea nueva"));
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    let text = std::mem::take(&mut app.pim.new_task);
                    match app.session.add_task(list_id, &text) {
                        Ok(out) => app.outcome(&ctx, out),
                        Err(e) => app.notice(iuredesk_engine::Notice::error(e.text())),
                    }
                    r.request_focus();
                }
            });
            ui.add_space(6.0);
        }
        // UX-6: teclado sobre la tarea elegida (sin un campo de texto con el foco).
        let sel = app.pim.task_sel.filter(|id| open.iter().chain(done.iter()).any(|t| t.id == *id));
        if let Some(id) = sel
            && !ctx.egui_wants_keyboard_input()
            && app.pim.task_editor.is_none()
            && app.pim.task_rename.is_none()
        {
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                acts.push((id, RowAct::Edit));
            } else if writable && ui.input(|i| i.key_pressed(egui::Key::F2)) {
                acts.push((id, RowAct::Rename));
            }
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width(760.0);
            if open.is_empty() {
                ui.add_space(20.0);
                EmptyState::new(Icon::Check, pick("All done", "Todo listo"))
                    .body(pick("Nothing pending in this list.", "No hay nada pendiente en esta lista."))
                    .show(ui);
            }
            for t in &open {
                if let Some(a) = task_row(app, ui, t, writable) {
                    acts.push((t.id, a));
                }
            }
            if !done.is_empty() {
                ui.add_space(10.0);
                let label = format!("{} ({})", pick("Completed", "Completadas"), done.len());
                let r = ui.add(egui::Button::new(RichText::new(label).color(c.text_muted)).frame(false));
                if r.clicked() {
                    app.pim.show_done = !app.pim.show_done;
                }
                if app.pim.show_done {
                    for t in &done {
                        if let Some(a) = task_row(app, ui, t, writable) {
                            acts.push((t.id, a));
                        }
                    }
                }
            }
        });
        for (id, a) in acts {
            match a {
                RowAct::Toggle => match app.session.toggle_task(id, &lists) {
                    Ok(out) => app.outcome(&ctx, out),
                    Err(e) => app.notice(iuredesk_engine::Notice::error(e.text())),
                },
                RowAct::Delete => match app.session.delete_task(id) {
                    Ok(out) => app.outcome(&ctx, out),
                    Err(e) => app.notice(iuredesk_engine::Notice::error(e.text())),
                },
                RowAct::Select => app.pim.task_sel = Some(id),
                RowAct::Edit => {
                    app.pim.task_sel = Some(id);
                    crate::task_editor::open(app, id);
                }
                RowAct::Rename => {
                    app.pim.task_sel = Some(id);
                    let title = app.session.task(id).map(|t| t.summary).unwrap_or_default();
                    app.pim.task_rename = Some(TaskRename { id, text: title, focused: false });
                }
            }
        }
    });
}

/// UX-6: título que se está cambiando en la lista (F2).
pub(crate) struct TaskRename {
    id: iuredesk_model::TaskId,
    text: String,
    focused: bool,
}

/// Una tarea con su casilla.
fn task_row(app: &mut DeskApp, ui: &mut Ui, t: &Task, writable: bool) -> Option<RowAct> {
    let c = theme::colors(ui);
    let zone = app.session.zone();
    let today = app.session.today();
    let d = theme::density(ui.ctx());
    let h = d.row_height().max(30.0) + 6.0;
    let (rect, row) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
    let mut act: Option<RowAct> = None;
    let selected = app.pim.task_sel == Some(t.id);
    // La fila (doble clic = editar) con su nombre; la casilla lleva el título solo.
    crate::widgets::named(&row, egui::Role::ListItem, &format!("{} {}", pick("Task", "Tarea"), t.summary));
    if selected {
        ui.painter().rect_filled(rect, 6.0, c.selection);
    } else if row.hovered() {
        ui.painter().rect_filled(rect, 6.0, c.hover);
    }
    egui::Popup::context_menu(&row).show(|ui| {
        if icons::menu_item(ui, Some(Icon::Rename), pick("Edit…", "Editar…")).clicked() {
            act = Some(RowAct::Edit);
            ui.close();
        }
        if writable {
            if icons::menu_item(ui, None, pick("Rename (F2)", "Cambiar nombre (F2)")).clicked() {
                act = Some(RowAct::Rename);
                ui.close();
            }
            if icons::menu_item(ui, Some(Icon::Trash), pick("Delete task", "Eliminar tarea")).clicked() {
                act = Some(RowAct::Delete);
                ui.close();
            }
        }
    });
    let box_r = Rect::from_center_size(pos2(rect.left() + 14.0, rect.center().y), vec2(18.0, 18.0));
    let resp = ui.interact(box_r.expand(4.0), ui.scope_id().with(("task", t.id.get())), Sense::click());
    let done = t.is_done();
    let ring = if resp.hovered() { c.accent } else { c.text_muted };
    if done {
        ui.painter().circle_filled(box_r.center(), 9.0, c.accent);
        icons::paint(ui.painter(), box_r.shrink(3.0), Icon::Check, c.on_accent, c.on_accent);
    } else {
        ui.painter().circle_stroke(box_r.center(), 8.5, Stroke::new(1.5, ring));
    }
    iurefficient_ui::a11y::toggle(&resp, &t.summary, done);
    let x = box_r.right() + 12.0;
    let mut right = rect.right() - 8.0;
    if let Some(due) = &t.due {
        let local = zone.local(due.to_utc());
        let dd = local.date();
        let time = match due {
            iuredesk_model::EventTime::DateTime { .. } => format!(" {}", local.format("%H:%M")),
            iuredesk_model::EventTime::Date(_) => String::new(),
        };
        let (txt, col) = if done {
            (dd.format("%d/%m").to_string(), c.text_muted)
        } else if dd < today {
            (format!("{} · {}", pick("Overdue", "Vencida"), dd.format("%d/%m")), c.danger)
        } else if dd == today {
            (format!("{}{time}", pick("Today", "Hoy")), c.accent)
        } else if Some(dd) == today.succ_opt() {
            (format!("{}{time}", pick("Tomorrow", "Mañana")), c.text_muted)
        } else {
            (format!("{} {}{time}", iuredesk_engine::time::weekday_short(dd), dd.format("%d/%m")), c.text_muted)
        };
        let g = ui.painter().layout_no_wrap(txt, theme::small_font(ui.ctx()), col);
        let w = g.size().x;
        ui.painter().galley(pos2(right - w, rect.center().y - g.size().y / 2.0), g, col);
        right -= w + 14.0;
    }
    if !t.rrule.is_empty() {
        let fr = Rect::from_center_size(pos2(right - 7.0, rect.center().y), vec2(14.0, 14.0));
        icons::paint(ui.painter(), fr, Icon::Sync, c.text_muted, c.text_muted);
        right -= 22.0;
    }
    if !done && (1..=4).contains(&t.priority) {
        let fr = Rect::from_center_size(pos2(right - 7.0, rect.center().y), vec2(14.0, 14.0));
        icons::paint(ui.painter(), fr, Icon::Flag, c.danger, c.danger);
        right -= 22.0;
    }
    let renaming = app.pim.task_rename.as_ref().is_some_and(|r| r.id == t.id);
    if renaming {
        // F2: el título se cambia en su sitio; Intro guarda, Esc deshace.
        let r_rect = Rect::from_min_max(pos2(x, rect.top() + 4.0), pos2(right.max(x + 80.0), rect.bottom() - 4.0));
        let mut save: Option<bool> = None;
        if let Some(rn) = app.pim.task_rename.as_mut() {
            let r = ui.put(r_rect, egui::TextEdit::singleline(&mut rn.text).desired_width(r_rect.width()));
            crate::widgets::text_label(&r, pick("Task title", "Título de la tarea"));
            if !rn.focused {
                rn.focused = true;
                r.request_focus();
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                save = Some(false);
            } else if r.lost_focus() {
                save = Some(true);
            }
        }
        if let Some(keep) = save
            && let Some(rn) = app.pim.task_rename.take()
            && keep
            && rn.text.trim() != t.summary
        {
            let ctx = ui.ctx().clone();
            match app.session.rename_task(rn.id, &rn.text) {
                Ok(out) => app.outcome(&ctx, out),
                Err(e) => app.notice(iuredesk_engine::Notice::error(e.text())),
            }
        }
    } else {
        let font = theme::body_font(ui.ctx());
        let ink = if done { c.text_muted } else { c.text };
        let g = iurefficient_ui::widgets::text::elided(ui, &t.summary, font, ink, (right - x).max(0.0));
        let gy = rect.center().y - g.size().y / 2.0;
        let gw = g.size().x;
        ui.painter().galley(pos2(x, gy), g, ink);
        if done {
            ui.painter().hline(x..=(x + gw), rect.center().y, Stroke::new(1.0, c.text_muted));
        }
    }
    ui.painter().hline((rect.left() + 34.0)..=rect.right(), rect.bottom() - 0.5, Stroke::new(1.0, c.separator));
    if act.is_some() {
        return act;
    }
    if resp.clicked() {
        Some(RowAct::Toggle)
    } else if row.double_clicked() {
        Some(RowAct::Edit)
    } else if row.clicked() {
        Some(RowAct::Select)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------------------------
// Asistente
