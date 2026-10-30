//! Correo: carpetas (favoritos, cuentas y panel «Hoy»), lista virtual densa, lectura y panel
//! contextual.

use std::collections::HashSet;

use egui::{Align, Color32, Layout, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use iuredesk_engine::IurePanel;
use iuredesk_engine::context::ContextCard;
use iuredesk_engine::time::{list_date, long_date};
use iuredesk_engine::{DensityPref, FolderSel, ListRow};
use iuredesk_model::{AccountId, FolderRole, MessageEnvelope, MessageId};
use iurefficient_ui::list::{Column, ListState, ListView, RowStyle};
use iurefficient_ui::{
    BadgeKind, Banner, EmptyState, Icon, PaneKind, Severity, avatar, badge, icons, pick, section_label, sidebar_item,
    split, theme,
};

use crate::app::DeskApp;
use crate::html::{HtmlCache, HtmlKey, HtmlState};
use crate::widgets::{hairline, pane_title, tool_button, wide_primary};

/// Estado de la vista de correo.
pub struct MailState {
    pub list: ListState,
    pub columns: Vec<Column>,
    /// Vista para la que vale `list` (al cambiar de carpeta se reinicia).
    list_for: Option<(FolderSel, String)>,
    pub search: String,
    pub focus_search: bool,
    expanded_accounts: HashSet<AccountId>,
    accounts_init: bool,
    /// Mensajes desplegados en la conversación.
    expanded: HashSet<MessageId>,
    conv: Option<(MessageId, u64, Vec<MessageEnvelope>)>,
    card: Option<(MessageId, u64, ContextCard)>,
    /// Mensajes con la historia citada desplegada.
    show_quoted: HashSet<MessageId>,
    /// F7.1: «Nueva carpeta…» / «Renombrar…» abierto.
    pub(crate) folder_dialog: Option<FolderDialog>,
    /// UX-3: lectura enfocada (sin carpetas ni lista; Esc vuelve).
    pub(crate) focus: bool,
    /// UX-3: franja contextual desplegada (con las tarjetas completas).
    pub(crate) strip_open: bool,
    /// UX-3: ventanas de lectura propias.
    pub(crate) readers: Vec<crate::reader::ReaderWindow>,
    pub(crate) next_reader: u64,
    /// UX-3: ancho del panel de lectura en el cuadro anterior y si el usuario lo está arrastrando.
    reading_w: Option<f32>,
    reading_dragging: bool,
}

/// Qué pide el diálogo de carpeta.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FolderDialogKind {
    New { account: AccountId, parent: Option<iuredesk_model::FolderId> },
    Rename { folder: iuredesk_model::FolderId },
}

/// Diálogo de nombre de carpeta.
#[derive(Clone, Debug)]
pub(crate) struct FolderDialog {
    pub kind: FolderDialogKind,
    pub name: String,
    pub error: Option<String>,
}

/// Lo que se eligió en el menú de una carpeta.
enum FolderAct {
    New(AccountId, Option<iuredesk_model::FolderId>),
    Rename(iuredesk_model::FolderId, String),
    Delete(iuredesk_model::FolderId),
}

impl Default for MailState {
    fn default() -> Self {
        MailState {
            list: ListState::default(),
            columns: vec![
                Column::new("", 26.0).fixed().not_sortable(),
                Column::new(pick("From", "De"), 190.0).min_width(90.0).not_sortable(),
                Column::new(pick("Subject", "Asunto"), 320.0).grow().min_width(140.0).not_sortable(),
                Column::new("", 24.0).fixed().not_sortable(),
                Column::new(pick("Received", "Recibido"), 92.0).right().min_width(64.0).not_sortable(),
            ],
            list_for: None,
            folder_dialog: None,
            search: String::new(),
            focus_search: false,
            expanded_accounts: HashSet::new(),
            accounts_init: false,
            expanded: HashSet::new(),
            conv: None,
            card: None,
            show_quoted: HashSet::new(),
            focus: false,
            strip_open: false,
            readers: Vec::new(),
            next_reader: 0,
            reading_w: None,
            reading_dragging: false,
        }
    }
}

/// Icono de una carpeta según su papel.
pub fn folder_icon(r: FolderRole) -> Icon {
    match r {
        FolderRole::Inbox => Icon::Inbox,
        FolderRole::Sent => Icon::Send,
        FolderRole::Drafts => Icon::Drafts,
        FolderRole::Trash => Icon::Trash,
        FolderRole::Junk => Icon::Junk,
        FolderRole::Archive => Icon::Archive,
        FolderRole::Custom => Icon::Folder,
    }
}

/// Ancho del riel del panel contextual plegado.
const CONTEXT_RAIL: f32 = 40.0;

/// La vista de correo completa.
pub fn show(app: &mut DeskApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    // UX-3: lectura enfocada sin mensaje no tiene sentido; Esc vuelve a la vista normal.
    if app.mail.focus && app.session.focused().is_none() {
        app.mail.focus = false;
    }
    if app.mail.focus
        && !ctx.egui_wants_keyboard_input()
        && !egui::Popup::is_any_open(&ctx)
        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    {
        app.mail.focus = false;
    }
    let focus = app.mail.focus;
    if !focus {
        split::left(ui, "folders", PaneKind::Sidebar, 244.0, 190.0..=380.0, |ui| folders(app, ui));
    }
    folder_dialog(app, &ctx);

    let focused = app.session.focused();
    let card = focused.map(|id| context_card(app, id)).unwrap_or_default();
    let iure = focused.map(|id| (id, app.session.iure_panel(id))).filter(|(_, p)| p.visible());
    // Sin sesión de Iurefficient ni datos del contacto, el panel no aparece.
    let has_context = !card.is_empty() || iure.is_some();
    // Ancho mínimo de la lista: la densa lleva columnas (De, Asunto, Recibido).
    let dense = app.session.prefs.density == DensityPref::Dense;
    let list_min = if dense { 560.0 } else { 400.0 };
    let avail = ui.available_width();
    let collapsed = app.session.prefs.context_collapsed;
    let strip = app.session.prefs.context_layout.strip_at(ctx.content_rect().width());
    // La columna solo si deja sitio a la lista (salvo en lectura enfocada) y a la lectura.
    let side_w = if collapsed { CONTEXT_RAIL } else { 264.0 };
    let fits = avail - side_w >= if focus { 460.0 } else { list_min + 460.0 };
    let side = has_context && !strip && fits;
    if side {
        if collapsed {
            context_rail(app, ui, &card, iure.as_ref());
        } else {
            split::right(ui, "context", PaneKind::Detail, 264.0, 220.0..=380.0, |ui| {
                context_panel(app, ui, &card, iure.as_ref())
            });
        }
    }
    let inline = if has_context && !side { Some((&card, iure.as_ref())) } else { None };
    if focus {
        split::center(ui, PaneKind::Detail, |ui| reading_pane(app, ui, inline));
        return;
    }
    let avail = ui.available_width();
    let reading_w = avail - list_min;
    // La lista conserva siempre su mínimo: el panel de lectura no puede comérsela aunque su
    // contenido pida más ancho.
    let max_reading = (avail - list_min).max(340.0);
    // UX-3: el ancho que dejó el usuario (si cabe).
    let start_w = app.session.prefs.reading_width.map(|w| w as f32).unwrap_or(reading_w);
    let frame = split::frame(ui, PaneKind::Detail);
    let resp = egui::Panel::right("reading")
        .frame(frame)
        .resizable(true)
        .default_size(start_w.clamp(340.0, max_reading))
        .size_range(340.0..=max_reading)
        .show(ui, |ui| {
            ui.set_max_width(ui.available_width());
            reading_pane(app, ui, inline)
        });
    remember_reading_width(app, &ctx, resp.response.rect.width());
    split::center(ui, PaneKind::Content, |ui| message_list(app, ui));
}

/// UX-3: guarda el ancho del panel de lectura cuando el usuario termina de arrastrar su borde
/// (no cuando cambia por el tamaño de la ventana).
fn remember_reading_width(app: &mut DeskApp, ctx: &egui::Context, w: f32) {
    let (down, released) = ctx.input(|i| (i.pointer.primary_down(), i.pointer.primary_released()));
    if down && app.mail.reading_w.is_some_and(|prev| (prev - w).abs() > 0.5) {
        app.mail.reading_dragging = true;
    }
    if released && std::mem::take(&mut app.mail.reading_dragging) {
        let v = w.round().max(1.0) as u32;
        if app.session.prefs.reading_width != Some(v) {
            app.session.prefs.reading_width = Some(v);
            app.session.save_prefs();
        }
    }
    app.mail.reading_w = Some(w);
}

/// UX-3: comandos de la vista que solo sabe hacer la UI.
pub(crate) fn view_command(app: &mut DeskApp, ctx: &egui::Context, id: &str) {
    match id {
        "mail.open_window" => {
            if let Some(m) = app.session.focused() {
                crate::reader::open(app, m);
            }
        }
        "view.reading_focus" => {
            app.session.module = iuredesk_engine::Module::Mail;
            app.mail.focus = !app.mail.focus && app.session.focused().is_some();
        }
        "view.context_panel" => {
            // En franja se despliega o pliega la franja; en columna, la columna ↔ riel.
            if app.session.prefs.context_layout.strip_at(ctx.content_rect().width()) {
                app.mail.strip_open = !app.mail.strip_open;
            } else {
                app.session.prefs.context_collapsed = !app.session.prefs.context_collapsed;
                app.session.save_prefs();
            }
        }
        _ => {}
    }
}
fn context_card(app: &mut DeskApp, id: MessageId) -> ContextCard {
    let generation = app.session.list_generation();
    if let Some((cid, g, card)) = &app.mail.card
        && *cid == id
        && *g == generation
    {
        return card.clone();
    }
    let card = app.session.context_card(id);
    app.mail.card = Some((id, generation, card.clone()));
    card
}

// ---------------------------------------------------------------------------------------------
// Carpetas

fn folders(app: &mut DeskApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    ui.add_space(6.0);
    if wide_primary(ui, Icon::Plus, pick("New message", "Nuevo mensaje")).clicked() {
        app.run(&ctx, "mail.new");
    }
    ui.add_space(6.0);
    if app.session.prefs.show_today {
        egui::Panel::bottom("today").frame(egui::Frame::new()).resizable(false).show(ui, |ui| today_panel(app, ui));
    }
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 1.0;
        ui.add_space(6.0);
        section_label(ui, pick("Favorites", "Favoritos"));
        ui.add_space(2.0);
        let current = app.session.folder_sel();
        let searching = !app.session.search_query().is_empty();
        let favs = [
            (FolderSel::Unified(FolderRole::Inbox), Icon::Inbox, pick("Inbox", "Entrada")),
            (FolderSel::Flagged, Icon::Flag, pick("Flagged", "Marcados")),
            (FolderSel::Unified(FolderRole::Sent), Icon::Send, pick("Sent", "Enviados")),
            (FolderSel::Unified(FolderRole::Drafts), Icon::Drafts, pick("Drafts", "Borradores")),
            (FolderSel::Unified(FolderRole::Archive), Icon::Archive, pick("Archive", "Archivo")),
            (FolderSel::Unified(FolderRole::Junk), Icon::Junk, pick("Junk", "No deseado")),
            (FolderSel::Unified(FolderRole::Trash), Icon::Trash, pick("Trash", "Papelera")),
        ];
        let outbox = app.session.outbox_entries().len();
        if outbox > 0 || current == FolderSel::Outbox {
            let on = !searching && current == FolderSel::Outbox;
            if sidebar_item(
                ui,
                Some(Icon::Send),
                pick("Outbox", "Bandeja de salida"),
                u32::try_from(outbox).unwrap_or(u32::MAX),
                on,
            )
            .clicked()
            {
                app.session.select_folder(FolderSel::Outbox);
                app.mail.search.clear();
            }
        }
        for (sel, ic, label) in favs {
            let count = match sel {
                FolderSel::Unified(r @ (FolderRole::Inbox | FolderRole::Junk)) => app.session.unified_unread(r),
                FolderSel::Unified(FolderRole::Drafts) => {
                    let ids: Vec<_> = app
                        .session
                        .account_labels()
                        .iter()
                        .flat_map(|l| {
                            app.session
                                .folders_of(l.id)
                                .into_iter()
                                .filter(|f| f.role == FolderRole::Drafts)
                                .map(|f| f.id)
                                .collect::<Vec<_>>()
                        })
                        .collect();
                    ids.into_iter().map(|f| app.session.count(f).total).sum()
                }
                _ => 0,
            };
            let on = !searching && current == sel;
            if sidebar_item(ui, Some(ic), label, u32::try_from(count).unwrap_or(u32::MAX), on).clicked() {
                app.session.select_folder(sel);
                app.mail.search.clear();
            }
        }
        ui.add_space(12.0);
        section_label(ui, pick("Accounts", "Cuentas"));
        ui.add_space(2.0);
        let labels = app.session.account_labels().to_vec();
        if !app.mail.accounts_init {
            app.mail.accounts_init = true;
            if let Some(first) = labels.first() {
                app.mail.expanded_accounts.insert(first.id);
            }
        }
        let rows = app.session.accounts_overview();
        for l in labels {
            let open = app.mail.expanded_accounts.contains(&l.id);
            let head = account_header(ui, &l.name, &l.email, open);
            if head.clicked() {
                if open {
                    app.mail.expanded_accounts.remove(&l.id);
                } else {
                    app.mail.expanded_accounts.insert(l.id);
                }
            }
            // Clic derecho: configurar o quitar la cuenta (Ajustes → Cuentas).
            let key = rows.iter().find(|r| r.aid == Some(l.id)).map(|r| r.id.clone()).unwrap_or_default();
            let mut action: Option<bool> = None;
            let manage = app.session.can_manage_folders(l.id);
            let mut folder_act: Option<FolderAct> = None;
            egui::Popup::context_menu(&head).show(|ui| {
                if manage && icons::menu_item(ui, Some(Icon::Folder), pick("New folder…", "Nueva carpeta…")).clicked()
                {
                    folder_act = Some(FolderAct::New(l.id, None));
                    ui.close();
                }
                if icons::menu_item(ui, Some(Icon::Settings), pick("Account settings…", "Configurar cuenta…")).clicked()
                {
                    action = Some(false);
                    ui.close();
                }
                if !key.is_empty()
                    && icons::menu_item(ui, Some(Icon::Trash), pick("Remove account…", "Eliminar cuenta…")).clicked()
                {
                    action = Some(true);
                    ui.close();
                }
            });
            match action {
                Some(false) => {
                    let edit = (!key.is_empty()).then(|| key.clone());
                    app.open_settings(crate::settings::Section::Accounts, edit, None);
                }
                Some(true) => app.open_settings(crate::settings::Section::Accounts, None, Some(key.clone())),
                None => {}
            }
            if open {
                let folders: Vec<(iuredesk_model::FolderId, FolderRole, String)> =
                    app.session.folders_of(l.id).into_iter().map(|f| (f.id, f.role, f.name.clone())).collect();
                for (fid, role, name) in folders {
                    let unread = if matches!(role, FolderRole::Sent | FolderRole::Drafts | FolderRole::Trash) {
                        0
                    } else {
                        app.session.count(fid).unread
                    };
                    let custom = manage && role == FolderRole::Custom;
                    ui.horizontal(|ui| {
                        ui.add_space(14.0);
                        let on = !searching && current == FolderSel::Folder(fid);
                        let r = sidebar_item(
                            ui,
                            Some(folder_icon(role)),
                            &name,
                            u32::try_from(unread).unwrap_or(u32::MAX),
                            on,
                        );
                        if r.clicked() {
                            app.session.select_folder(FolderSel::Folder(fid));
                            app.mail.search.clear();
                        }
                        // F7.1: clic derecho → nueva (sub)carpeta, renombrar, eliminar.
                        if manage {
                            egui::Popup::context_menu(&r).show(|ui| {
                                if icons::menu_item(ui, Some(Icon::Folder), pick("New folder…", "Nueva carpeta…"))
                                    .clicked()
                                {
                                    folder_act = Some(FolderAct::New(l.id, None));
                                    ui.close();
                                }
                                if custom {
                                    if icons::menu_item(
                                        ui,
                                        Some(Icon::Plus),
                                        pick("New subfolder…", "Nueva subcarpeta…"),
                                    )
                                    .clicked()
                                    {
                                        folder_act = Some(FolderAct::New(l.id, Some(fid)));
                                        ui.close();
                                    }
                                    if icons::menu_item(ui, Some(Icon::Rename), pick("Rename…", "Renombrar…")).clicked()
                                    {
                                        folder_act = Some(FolderAct::Rename(fid, name.clone()));
                                        ui.close();
                                    }
                                    if icons::menu_item(
                                        ui,
                                        Some(Icon::Trash),
                                        pick("Delete folder…", "Eliminar carpeta…"),
                                    )
                                    .clicked()
                                    {
                                        folder_act = Some(FolderAct::Delete(fid));
                                        ui.close();
                                    }
                                }
                            });
                        }
                    });
                }
            }
            match folder_act {
                Some(FolderAct::New(account, parent)) => {
                    app.mail.folder_dialog = Some(FolderDialog {
                        kind: FolderDialogKind::New { account, parent },
                        name: String::new(),
                        error: None,
                    });
                }
                Some(FolderAct::Rename(folder, name)) => {
                    app.mail.folder_dialog =
                        Some(FolderDialog { kind: FolderDialogKind::Rename { folder }, name, error: None });
                }
                Some(FolderAct::Delete(f)) => app.run(&ctx, &format!("mail.delete_folder:{}", f.get())),
                None => {}
            }
            ui.add_space(2.0);
        }
        ui.add_space(8.0);
        if ui
            .add(
                egui::Button::new(
                    RichText::new(pick("+ Add account", "+ Agregar cuenta")).color(theme::colors(ui).accent),
                )
                .frame(false),
            )
            .clicked()
        {
            app.run(&ctx, "app.add_account");
        }
    });
}

/// Encabezado plegable de una cuenta.
fn account_header(ui: &mut Ui, name: &str, email: &str, open: bool) -> egui::Response {
    let c = theme::colors(ui);
    let d = theme::density(ui.ctx());
    let h = d.row_height().max(26.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 6.0, c.hover);
    }
    let ir = Rect::from_center_size(pos2(rect.left() + 12.0, rect.center().y), vec2(12.0, 12.0));
    icons::paint(
        ui.painter(),
        ir,
        if open { Icon::ChevronDown } else { Icon::ChevronRight },
        c.text_muted,
        c.text_muted,
    );
    let font = theme::bold_font(ui.ctx(), d.small_size() + 0.5);
    let label = if email.is_empty() { name } else { email };
    let g = iurefficient_ui::widgets::text::elided(ui, label, font, c.text, rect.width() - 30.0);
    ui.painter().galley(pos2(rect.left() + 24.0, rect.center().y - g.size().y / 2.0), g, c.text);
    iurefficient_ui::a11y::toggle(&resp, &format!("{name} <{email}>"), open);
    resp
}

fn today_panel(app: &mut DeskApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let c = theme::colors(ui);
    let zone = app.session.zone();
    let now = app.session.now();
    let today = app.session.today();
    ui.add_space(4.0);
    hairline(ui);
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        section_label(ui, pick("Today", "Hoy"));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let d = format!("{} {}", iuredesk_engine::time::weekday_short(today), today.format("%d"));
            ui.label(RichText::new(d).size(theme::density(ui.ctx()).small_size()).color(c.text_muted));
        });
    });
    ui.add_space(4.0);
    let events: Vec<_> = app.session.today_events().into_iter().filter(|o| o.all_day || o.end > now).take(4).collect();
    if events.is_empty() {
        ui.label(RichText::new(pick("No more events today", "No tienes más citas hoy")).color(c.text_muted));
    }
    for o in &events {
        let time = if o.all_day {
            pick("All day", "Todo el día").to_string()
        } else {
            zone.local(o.start).format("%H:%M").to_string()
        };
        let resp = ui
            .horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let (bar, _) = ui.allocate_exact_size(vec2(3.0, 16.0), Sense::hover());
                ui.painter().rect_filled(bar, 1.5, crate::widgets::rgb(o.color));
                ui.add_sized(
                    vec2(if o.all_day { 70.0 } else { 38.0 }, 16.0),
                    egui::Label::new(RichText::new(time).color(c.accent).strong()),
                );
                let w = ui.available_width();
                let g = iurefficient_ui::widgets::text::elided(ui, &o.summary, theme::body_font(ui.ctx()), c.text, w);
                ui.label(egui::WidgetText::Galley(g));
            })
            .response
            .interact(Sense::click());
        iurefficient_ui::a11y::button(&resp, &o.summary);
        if resp.clicked() {
            app.session.module = iuredesk_engine::Module::Calendar;
        }
    }
    crate::iure::today_items(app, ui);
    crate::rules_ui::today_followups(app, ui);
    let tasks = app.session.due_tasks();
    if !tasks.is_empty() {
        ui.add_space(4.0);
        for t in tasks.iter().take(3) {
            let overdue = t.due.as_ref().is_some_and(|d| d.to_utc() < zone.day_start(today));
            let resp = ui
                .horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                    ui.painter().rect_stroke(
                        r.shrink(1.0),
                        3.0,
                        Stroke::new(1.3, if overdue { c.danger } else { c.text_muted }),
                        egui::StrokeKind::Inside,
                    );
                    let w = ui.available_width();
                    let g = iurefficient_ui::widgets::text::elided(
                        ui,
                        &t.summary,
                        theme::body_font(ui.ctx()),
                        if overdue { c.danger } else { c.text },
                        w,
                    );
                    ui.label(egui::WidgetText::Galley(g));
                })
                .response
                .interact(Sense::click());
            iurefficient_ui::a11y::button(&resp, &t.summary);
            if resp.clicked() {
                app.session.module = iuredesk_engine::Module::Tasks;
            }
        }
        if tasks.len() > 3 {
            ui.label(
                RichText::new(format!("+{} {}", tasks.len() - 3, pick("more tasks", "tareas más")))
                    .color(c.text_muted)
                    .size(theme::density(&ctx).small_size()),
            );
        }
    }
    ui.add_space(6.0);
}

// ---------------------------------------------------------------------------------------------
// Lista

/// La bandeja de salida: lo que espera su hora (deshacer, programado, reintento) o falló.
fn outbox_view(app: &mut DeskApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let c = theme::colors(ui);
    let entries = app.session.outbox_entries();
    egui::Frame::new().inner_margin(egui::Margin { left: 14, right: 10, top: 10, bottom: 8 }).show(ui, |ui| {
        pane_title(
            ui,
            pick("Outbox", "Bandeja de salida"),
            &format!("{} {}", entries.len(), pick("messages", "mensajes")),
        );
    });
    hairline(ui);
    if entries.is_empty() {
        EmptyState::new(Icon::Send, pick("Nothing waiting to be sent", "No hay nada esperando para enviarse"))
            .body(pick(
                "Messages wait here during the undo window, until their scheduled time, or while offline.",
                "Aquí esperan los mensajes durante la ventana para deshacer, hasta su hora programada o mientras no hay conexión.",
            ))
            .show(ui);
        // Mientras haya algo, se vuelve a mirar cada segundo.
        return;
    }
    ctx.request_repaint_after(std::time::Duration::from_secs(1));
    let now = chrono::Utc::now();
    let zone = app.session.zone();
    let mut cancel = None;
    let mut retry = None;
    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("outbox-scroll").show(ui, |ui| {
        for e in &entries {
            egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 8)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        let subject =
                            if e.subject.trim().is_empty() { pick("(no subject)", "(sin asunto)") } else { &e.subject };
                        ui.label(RichText::new(subject).strong().color(c.text));
                        ui.label(RichText::new(format!("{} {}", pick("To:", "Para:"), e.to)).color(c.text_muted));
                        let state = match e.state {
                            iuredesk_engine::iuredesk_compose::OutboxState::Failed => e
                                .last_error
                                .as_ref()
                                .map(|er| {
                                    er.message(if iuredesk_engine::lang::spanish() {
                                        iuredesk_engine::iuredesk_compose::Lang::Es
                                    } else {
                                        iuredesk_engine::iuredesk_compose::Lang::En
                                    })
                                })
                                .unwrap_or_else(|| pick("Failed", "Falló").to_string()),
                            iuredesk_engine::iuredesk_compose::OutboxState::Sending => {
                                pick("Sending…", "Enviando…").to_string()
                            }
                            iuredesk_engine::iuredesk_compose::OutboxState::Waiting if e.scheduled => format!(
                                "{} {}",
                                pick("Scheduled for", "Programado para"),
                                iuredesk_engine::compose::schedule_label(e.not_before, now, zone)
                            ),
                            iuredesk_engine::iuredesk_compose::OutboxState::Waiting if e.attempts > 0 => format!(
                                "{} {}",
                                pick("Will retry", "Se reintentará"),
                                iuredesk_engine::compose::schedule_label(e.not_before, now, zone)
                            ),
                            iuredesk_engine::iuredesk_compose::OutboxState::Waiting => {
                                let secs = (e.not_before - now).num_seconds().max(0);
                                format!("{} {secs} s", pick("Sending in", "Se envía en"))
                            }
                        };
                        let failed = e.state == iuredesk_engine::iuredesk_compose::OutboxState::Failed;
                        ui.add(
                            egui::Label::new(RichText::new(state).color(if failed { c.danger } else { c.text_muted }))
                                .wrap(),
                        );
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .button(pick("Edit", "Editar"))
                            .on_hover_text(pick("Stop sending and reopen", "Detener el envío y reabrir"))
                            .clicked()
                        {
                            cancel = Some(e.id);
                        }
                        if e.state != iuredesk_engine::iuredesk_compose::OutboxState::Sending
                            && ui.button(pick("Send now", "Enviar ya")).clicked()
                        {
                            retry = Some(e.id);
                        }
                    });
                });
            });
            hairline(ui);
        }
    });
    if let Some(id) = cancel {
        match app.session.outbox_cancel(id) {
            Ok(out) => app.outcome(&ctx, out),
            Err(e) => app.notice(iuredesk_engine::Notice::error(e.text())),
        }
    }
    if let Some(id) = retry {
        app.session.outbox_retry(id);
    }
}

fn message_list(app: &mut DeskApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let c = theme::colors(ui);
    if app.session.folder_sel() == FolderSel::Outbox && app.session.search_query().is_empty() {
        outbox_view(app, ui);
        return;
    }
    let view = (app.session.folder_sel(), app.session.search_query().to_string());
    if app.mail.list_for.as_ref() != Some(&view) {
        app.mail.list = ListState::default();
        app.mail.list_for = Some(view);
    }
    if let Some((idx, id)) = app.session.take_focus_request() {
        app.mail.list.selection.select_only(idx, id.get());
        app.mail.list.scroll_to(idx);
        app.session.set_selection(vec![id], Some(id));
    }
    let rows = app.session.rows_shared();
    let conversations = app.session.prefs.conversations && app.session.search_query().is_empty();

    // Encabezado: carpeta, total, conversaciones y densidad.
    egui::Frame::new().inner_margin(egui::Margin { left: 14, right: 10, top: 10, bottom: 8 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            let n = rows.len();
            let detail = if conversations {
                format!("{n} {}", pick("conversations", "conversaciones"))
            } else {
                format!("{n} {}", pick("messages", "mensajes"))
            };
            pane_title(ui, &app.session.folder_title(), &detail);
            if app.session.is_loading() {
                ui.add(egui::Spinner::new().size(12.0));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                crate::rules_ui::iure_filter_button(app, ui);
                let conv_on = app.session.prefs.conversations;
                let r = icons::icon_button(
                    ui,
                    Icon::List,
                    if conv_on {
                        pick("Show individual messages", "Ver mensajes sueltos")
                    } else {
                        pick("Group by conversation", "Agrupar por conversación")
                    },
                    true,
                );
                if conv_on {
                    ui.painter().rect_stroke(
                        r.rect.shrink(1.0),
                        5.0,
                        Stroke::new(1.0, c.accent),
                        egui::StrokeKind::Inside,
                    );
                }
                if r.clicked() {
                    app.run(&ctx, "mail.toggle_conversations");
                }
            });
        });
    });
    crate::rules_ui::category_tabs(app, ui);
    hairline(ui);

    if rows.is_empty() {
        if app.session.is_loading() {
            return;
        }
        let searching = !app.session.search_query().is_empty();
        let (title, body) = if searching {
            (
                pick("No results", "Sin resultados"),
                pick(
                    "Try other words, or operators like from:, has:attachment, before:2026-01-01.",
                    "Prueba otras palabras u operadores como de:, tiene:adjunto, antes:2026-01-01.",
                ),
            )
        } else {
            (
                pick("Nothing here", "Nada por aquí"),
                pick(
                    "Messages that arrive in this folder will show up here.",
                    "Los mensajes que lleguen a esta carpeta aparecerán aquí.",
                ),
            )
        };
        EmptyState::new(if searching { Icon::Search } else { Icon::Inbox }, title).body(body).show(ui);
        return;
    }

    let density = app.session.prefs.density;
    let now = app.session.now();
    let zone = app.session.zone();
    let body_font = theme::body_font(&ctx);
    let small_font = theme::small_font(&ctx);
    let bold_font = theme::bold_font(&ctx, body_font.size);
    let sent_like: HashSet<iuredesk_model::FolderId> = app
        .session
        .account_labels()
        .to_vec()
        .iter()
        .flat_map(|l| {
            app.session
                .folders_of(l.id)
                .into_iter()
                .filter(|f| matches!(f.role, FolderRole::Sent | FolderRole::Drafts))
                .map(|f| f.id)
                .collect::<Vec<_>>()
        })
        .collect();
    let list_label = format!("{} {}", pick("Messages in", "Mensajes de"), app.session.folder_title());
    let hover = ctx.pointer_hover_pos();
    let mut badge_tip: Option<String> = None;
    let DeskApp { session, mail, .. } = app;
    let rows_for_key = std::sync::Arc::clone(&rows);
    let key_of = move |i: usize| rows_for_key.get(i).map_or(u64::MAX - i as u64, |r| r.id.get());
    let mut lv = ListView::new("mail-list", &mut mail.columns, rows.len()).label(list_label);
    if density != DensityPref::Dense {
        let h = if density == DensityPref::Comfortable { 62.0 } else { 48.0 };
        lv = lv.row_height(h).header(false);
    }
    let out = lv.show(ui, &mut mail.list, key_of, |row| {
        let Some(&ListRow { id, count }) = rows.get(row.index) else { return };
        let Some(env) = session.envelope(id).cloned() else { return };
        let unread = !env.flags.seen;
        let labels = session.message_labels(&env);
        let badge = session.iure_badge(id);
        if let Some(b) = &badge {
            row.describe(b.tip.get());
        }
        row.style(RowStyle { unread, flagged: env.flags.flagged, muted: false });
        let col = *row.colors();
        let dark = col.dark;
        let who = if sent_like.contains(&env.folder) {
            let to = env.to.first().map(|m| m.display().to_string()).unwrap_or_default();
            format!("{} {to}", pick("To:", "Para:"))
        } else {
            env.sender_display().to_string()
        };
        let date = list_date(env.date, now, zone);
        // UX-5: con la cápsula, la etiqueta «[Iurefficient]» del asunto sobra.
        let shown = if badge.is_some() {
            iuredesk_engine::iuredesk_rules::iure_mail::display_subject(&env.subject)
        } else {
            env.subject.as_str()
        };
        let subject =
            if shown.trim().is_empty() { pick("(no subject)", "(sin asunto)").to_string() } else { shown.to_string() };
        if unread {
            row.describe(pick("unread", "no leído"));
        }
        if env.flags.flagged {
            row.describe(pick("flagged", "marcado"));
        }
        if env.has_attachments {
            row.describe(pick("attachment", "adjunto"));
        }
        if density == DensityPref::Dense {
            if env.flags.flagged {
                row.icon_colored(0, Icon::Flag, col.warning, col.warning);
            } else if env.flags.answered {
                row.icon_colored(0, Icon::Reply, col.text_muted, col.text_muted);
            }
            let who_text = if count > 1 { format!("{who}  ({count})") } else { who };
            row.text(1, &who_text);
            // F5.3: insignia Iurefficient y etiquetas como puntos de color antes del asunto.
            let cell = row.cell(2);
            let mut x = cell.left();
            if let Some(b) = &badge {
                // UX-5: en densa, solo el símbolo de Iurefficient en el color de la marca.
                let br = Rect::from_min_size(pos2(x, cell.center().y - 6.0), vec2(11.0, 12.0));
                crate::iure_badge::paint_dot(row.painter(), br, dark);
                if hover.is_some_and(|h| br.expand(3.0).contains(h)) {
                    badge_tip = Some(b.tip.get().to_string());
                }
                x += 17.0;
            }
            x += crate::rules_ui::paint_label_chips(row.painter(), &ctx, pos2(x, cell.center().y), 40.0, &labels, true);
            if x > cell.left() {
                x += 2.0;
            }
            let subj_font = if unread { bold_font.clone() } else { body_font.clone() };
            row.text_in(Rect::from_min_max(pos2(x, cell.top()), cell.max), Align::Min, &subject, subj_font, col.text);
            if env.has_attachments {
                row.icon_colored(3, Icon::Attachment, col.text_muted, col.text_muted);
            }
            row.text_muted(4, &date);
            return;
        }
        // Dos líneas (cómoda con avatar).
        let r = row.rect;
        let mut left = r.left() + 14.0;
        if density == DensityPref::Comfortable {
            let center = pos2(left + 17.0, r.center().y);
            iurefficient_ui::widgets::avatar::paint_avatar(
                row.painter(),
                center,
                34.0,
                env.from.first().and_then(|m| m.name.as_deref()).unwrap_or(""),
                env.from.first().map(|m| m.addr.as_str()).unwrap_or(""),
            );
            left += 46.0;
        }
        let right = r.right() - 12.0;
        let mid = r.center().y;
        let line1 = Rect::from_min_max(pos2(left, r.top() + 4.0), pos2(right, mid + 1.0));
        let line2 = Rect::from_min_max(pos2(left, mid - 1.0), pos2(right, r.bottom() - 4.0));
        // Línea 1: remitente y fecha.
        let date_font = if unread { bold_font.clone() } else { small_font.clone() };
        let date_color = if unread { col.accent } else { col.text_muted };
        let date_w = 76.0;
        row.text_in(
            Rect::from_min_max(pos2(line1.right() - date_w, line1.top()), line1.max),
            Align::Max,
            &date,
            date_font,
            date_color,
        );
        let who_font = if unread { bold_font.clone() } else { body_font.clone() };
        let count_w = if count > 1 { 30.0 } else { 0.0 };
        let who_rect = Rect::from_min_max(line1.min, pos2(line1.right() - date_w - 8.0 - count_w, line1.bottom()));
        // F5.3: insignia Iurefficient a la izquierda de la fecha.
        let who_rect = if let Some(b) = &badge {
            // UX-5: cápsula (símbolo + tipo) a la izquierda de la fecha.
            let size = crate::iure_badge::capsule_size(17.0);
            let br = Rect::from_min_size(pos2(who_rect.right() - size.x + 2.0, line1.center().y - size.y / 2.0), size);
            crate::iure_badge::paint_capsule(row.painter(), br, b.kind, dark);
            if hover.is_some_and(|h| br.contains(h)) {
                badge_tip = Some(b.tip.get().to_string());
            }
            Rect::from_min_max(who_rect.min, pos2(br.left() - 6.0, who_rect.bottom()))
        } else {
            who_rect
        };
        row.text_in(who_rect, Align::Min, &who, who_font.clone(), col.text);
        if count > 1 {
            let g = row.painter().layout_no_wrap(who.clone(), who_font, col.text);
            let x = (who_rect.left() + g.size().x + 6.0).min(who_rect.right() + 4.0);
            let cr = Rect::from_min_max(pos2(x, line1.top()), pos2(x + count_w, line1.bottom()));
            row.text_in(cr, Align::Min, &count.to_string(), small_font.clone(), col.text_muted);
        }
        // Línea 2: asunto, extracto e iconos.
        let mut icons_w = 0.0;
        let icon_side = 14.0;
        let mut ix = line2.right();
        for (on, ic, ink) in
            [(env.flags.flagged, Icon::Flag, col.warning), (env.has_attachments, Icon::Attachment, col.text_muted)]
        {
            if on {
                let ir =
                    Rect::from_center_size(pos2(ix - icon_side / 2.0, line2.center().y), vec2(icon_side, icon_side));
                icons::paint(row.painter(), ir, ic, ink, ink);
                ix -= icon_side + 6.0;
                icons_w += icon_side + 6.0;
            }
        }
        let mut text_rect = Rect::from_min_max(line2.min, pos2(line2.right() - icons_w, line2.bottom()));
        // F5.3: chips de etiquetas y categoría antes del asunto.
        if !labels.is_empty() {
            let used = crate::rules_ui::paint_label_chips(
                row.painter(),
                &ctx,
                pos2(text_rect.left(), text_rect.center().y),
                text_rect.width() * 0.45,
                &labels,
                false,
            );
            if used > 0.0 {
                text_rect.min.x += used + 2.0;
            }
        }
        let subj_font = if unread { bold_font.clone() } else { body_font.clone() };
        let g = row.painter().layout_no_wrap(subject.clone(), subj_font.clone(), col.text);
        let subj_w = g.size().x.min(text_rect.width() * 0.7);
        row.text_in(
            Rect::from_min_max(text_rect.min, pos2(text_rect.left() + subj_w, text_rect.bottom())),
            Align::Min,
            &subject,
            subj_font,
            col.text,
        );
        let prev_rect = Rect::from_min_max(pos2(text_rect.left() + subj_w + 8.0, text_rect.top()), text_rect.max);
        if prev_rect.width() > 24.0 && !env.preview.is_empty() {
            let p = painter_safe_preview(&env.preview);
            row.text_in(prev_rect, Align::Min, &p, body_font.clone(), col.text_muted);
        }
    });
    if let Some(tip) = badge_tip {
        out.response.clone().on_hover_text_at_pointer(tip);
    }
    if out.selection_changed {
        let ids: Vec<MessageId> = mail.list.selection.keys().map(MessageId).collect();
        let cursor = mail.list.selection.cursor().and_then(|i| rows.get(i)).map(|r| r.id);
        session.set_selection(ids, cursor);
        mail.expanded.clear();
    }
    let mut edit_draft = false;
    let mut open_window = None;
    if let Some(i) = out.activated
        && let Some(r) = rows.get(i)
    {
        session.set_selection(vec![r.id], Some(r.id));
        // F7.1: doble clic / Intro en un borrador lo reabre en el redactor.
        edit_draft = session.is_draft(r.id);
        // UX-3: en lo demás, como en Outlook, abre el mensaje en su propia ventana.
        if !edit_draft {
            open_window = Some(r.id);
        }
    }
    if let Some(i) = out.context {
        let _ = i;
        let resp = out.response.clone();
        message_menu(app, &resp);
    }
    if edit_draft {
        app.run(&ctx, "mail.edit_draft");
    }
    if let Some(id) = open_window {
        crate::reader::open(app, id);
    }
}

/// F7.1: diálogo «Nueva carpeta» / «Renombrar carpeta».
pub(crate) fn folder_dialog(app: &mut DeskApp, ctx: &egui::Context) {
    let Some(mut d) = app.mail.folder_dialog.clone() else { return };
    let (title, ok_label) = match d.kind {
        FolderDialogKind::New { parent: Some(_), .. } => {
            (pick("New subfolder", "Nueva subcarpeta"), pick("Create", "Crear"))
        }
        FolderDialogKind::New { .. } => (pick("New folder", "Nueva carpeta"), pick("Create", "Crear")),
        FolderDialogKind::Rename { .. } => (pick("Rename folder", "Renombrar carpeta"), pick("Rename", "Renombrar")),
    };
    let mut answer: Option<bool> = None;
    let modal = egui::Modal::new(egui::Id::unique("folder-dialog")).show(ctx, |ui| {
        ui.set_width(340.0);
        let c = theme::colors(ui);
        ui.label(RichText::new(title).font(theme::bold_font(ui.ctx(), 16.0)).color(c.text));
        ui.add_space(8.0);
        let r = ui.add(
            egui::TextEdit::singleline(&mut d.name)
                .hint_text(pick("Folder name", "Nombre de la carpeta"))
                .desired_width(f32::INFINITY),
        );
        crate::widgets::text_label(&r, pick("Folder name", "Nombre de la carpeta"));
        if !r.has_focus() && d.error.is_none() && d.name.is_empty() {
            r.request_focus();
        }
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            answer = Some(true);
        }
        if let Some(e) = &d.error {
            ui.add_space(4.0);
            ui.add(egui::Label::new(RichText::new(e).color(c.danger)).wrap());
        }
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            let ok = ui.add_enabled(
                !d.name.trim().is_empty(),
                egui::Button::new(RichText::new(ok_label).color(c.on_accent)).fill(c.accent),
            );
            if ok.clicked() {
                answer = Some(true);
            }
            if ui.button(pick("Cancel", "Cancelar")).clicked() {
                answer = Some(false);
            }
        });
    });
    if modal.should_close() && answer.is_none() {
        answer = Some(false);
    }
    match answer {
        Some(true) => {
            let r = match d.kind {
                FolderDialogKind::New { account, parent } => {
                    app.session.create_folder(account, parent, &d.name).map(|f| {
                        app.session.select_folder(FolderSel::Folder(f));
                        iuredesk_engine::Outcome::Notice(iuredesk_engine::Notice::success(
                            format!("Folder \"{}\" created", d.name.trim()),
                            format!("Carpeta «{}» creada", d.name.trim()),
                        ))
                    })
                }
                FolderDialogKind::Rename { folder } => app.session.rename_folder(folder, &d.name),
            };
            match r {
                Ok(out) => {
                    app.mail.folder_dialog = None;
                    app.outcome(ctx, out);
                }
                Err(e) => {
                    d.error = Some(e.text().get().to_string());
                    app.mail.folder_dialog = Some(d);
                }
            }
        }
        Some(false) => app.mail.folder_dialog = None,
        None => app.mail.folder_dialog = Some(d),
    }
}

/// Extracto en una línea.
fn painter_safe_preview(p: &str) -> String {
    p.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn message_menu(app: &mut DeskApp, resp: &egui::Response) {
    let ctx = resp.ctx.clone();
    let mut chosen: Option<&'static str> = None;
    let snooze = app.session.snooze_menu();
    let snoozed = app.session.focused().is_some_and(|m| app.session.is_snoozed(m));
    let mut extra: Option<String> = None;
    egui::Popup::context_menu(resp).show(|ui| {
        for (id, ic, en, es) in [
            ("mail.reply", Icon::Reply, "Reply", "Responder"),
            ("mail.reply_all", Icon::ReplyAll, "Reply all", "Responder a todos"),
            ("mail.forward", Icon::Forward, "Forward", "Reenviar"),
            ("mail.archive", Icon::Archive, "Archive", "Archivar"),
            ("mail.delete", Icon::Trash, "Delete", "Eliminar"),
            ("mail.toggle_read", Icon::Check, "Mark read / unread", "Marcar leído / no leído"),
            ("mail.toggle_flag", Icon::Flag, "Flag / unflag", "Marcar / quitar seguimiento"),
            ("mail.move", Icon::Folder, "Move to…", "Mover a…"),
            ("mail.open_window", Icon::Open, "Open in window", "Abrir en ventana"),
        ] {
            if icons::menu_item(ui, Some(ic), pick(en, es)).clicked() {
                chosen = Some(id);
                ui.close();
            }
        }
        crate::rules_ui::context_items(ui, &snooze, snoozed, &mut extra);
    });
    if let Some(id) = chosen {
        app.run(&ctx, id);
    }
    if let Some(id) = extra {
        app.run(&ctx, &id);
    }
}

// ---------------------------------------------------------------------------------------------
// Lectura

fn reading_pane(app: &mut DeskApp, ui: &mut Ui, inline_card: Option<(&ContextCard, Option<&(MessageId, IurePanel)>)>) {
    let ctx = ui.ctx().clone();
    let c = theme::colors(ui);
    let Some(id) = app.session.focused() else {
        let n = app.session.selection().len();
        if n > 1 {
            EmptyState::new(Icon::Mail, &format!("{n} {}", pick("messages selected", "mensajes seleccionados")))
                .body(pick(
                    "Archive, delete or move them all at once from the toolbar or with Ctrl+K.",
                    "Archívalos, elimínalos o muévelos juntos desde la barra o con Ctrl+K.",
                ))
                .show(ui);
        } else {
            EmptyState::new(Icon::Mail, pick("Select a message", "Selecciona un mensaje"))
                .body(pick(
                    "Pick a message from the list to read it here.",
                    "Elige un mensaje de la lista para leerlo aquí.",
                ))
                .show(ui);
        }
        return;
    };
    // Conversación (con caché por generación de la lista).
    let generation = app.session.list_generation();
    let conv = match &app.mail.conv {
        Some((cid, g, v)) if *cid == id && *g == generation => v.clone(),
        _ => {
            let v = app.session.conversation(id);
            app.mail.conv = Some((id, generation, v.clone()));
            v
        }
    };
    let Some(subject) =
        conv.last().map(|e| e.subject.clone()).or_else(|| app.session.envelope(id).map(|e| e.subject.clone()))
    else {
        return;
    };
    let (base, _) = iuredesk_model::normalize_subject(&subject);
    let mut view_cmd: Option<&'static str> = None;
    ui.horizontal_top(|ui| {
        let w = (ui.available_width() - 64.0).max(120.0);
        ui.allocate_ui_with_layout(vec2(w, 0.0), Layout::top_down(Align::Min), |ui| {
            ui.set_width(w);
            ui.add(
                egui::Label::new(
                    RichText::new(if base.is_empty() { subject.clone() } else { base })
                        .font(theme::bold_font(&ctx, 19.0))
                        .color(c.text),
                )
                .wrap(),
            );
        });
        // UX-3: leer en grande (Esc vuelve) o en una ventana propia.
        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let mac = cfg!(target_os = "macos");
            let scheme = app.session.scheme();
            let tip = |label: &str, id: &str| match iuredesk_engine::command::shortcut(id, scheme) {
                Some(k) => format!("{label}  ({})", k.label(mac)),
                None => label.to_string(),
            };
            let open_tip = tip(pick("Open in window", "Abrir en ventana"), "mail.open_window");
            if icons::icon_button(ui, Icon::Open, &open_tip, true).clicked() {
                view_cmd = Some("mail.open_window");
            }
            let (ic, label) = if app.mail.focus {
                (Icon::Restore, pick("Back to the list (Esc)", "Volver a la lista (Esc)"))
            } else {
                (Icon::Maximize, pick("Focused reading", "Lectura enfocada"))
            };
            if icons::icon_button(ui, ic, &tip(label, "view.reading_focus"), true).clicked() {
                view_cmd = Some("view.reading_focus");
            }
        });
    });
    if let Some(cmd) = view_cmd {
        app.run(&ctx, cmd);
    }
    if conv.len() > 1 {
        ui.label(
            RichText::new(format!(
                "{} {}",
                conv.len(),
                pick("messages in this conversation", "mensajes en esta conversación")
            ))
            .color(c.text_muted),
        );
    }
    ui.add_space(6.0);
    action_bar(app, ui);
    if let Some(env) = conv.iter().find(|e| e.id == id).or(conv.last()).cloned() {
        crate::rules_ui::reading_chips(app, ui, &env);
    }
    crate::assistant::reading_bar(app, ui, id);
    // UX-3: con poco ancho, el contexto va en una franja compacta bajo la cabecera.
    if let Some((card, iure)) = inline_card {
        ui.add_space(4.0);
        context_strip(app, ui, card, iure);
    }
    ui.add_space(4.0);
    hairline(ui);

    let last = conv.last().map(|e| e.id);
    egui::ScrollArea::both().auto_shrink([false, false]).id_salt("reading-scroll").show(ui, |ui| {
        ui.add_space(8.0);
        crate::assistant::reading_cards(app, ui, id);
        let n = conv.len();
        for (k, env) in conv.iter().enumerate() {
            let expanded = env.id == id || Some(env.id) == last || app.mail.expanded.contains(&env.id) || n == 1;
            message_card(app, ui, env, expanded);
            if k + 1 < n {
                ui.add_space(6.0);
                hairline(ui);
                ui.add_space(6.0);
            }
        }
        crate::assistant::reply_chips(app, ui, id);
        ui.add_space(24.0);
    });
}

fn action_bar(app: &mut DeskApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let scheme = app.session.scheme();
    let mac = cfg!(target_os = "macos");
    let tip = |label: &str, id: &str| match iuredesk_engine::command::shortcut(id, scheme) {
        Some(k) => format!("{label}  ({})", k.label(mac)),
        None => label.to_string(),
    };
    let mut fire: Option<&'static str> = None;
    let snooze = app.session.snooze_menu();
    let snoozed = app.session.focused().is_some_and(|m| app.session.is_snoozed(m));
    let draft = app.session.focused().is_some_and(|m| app.session.is_draft(m));
    let mut extra: Option<String> = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let narrow = ui.available_width() < 520.0;
        // F7.1: un borrador se reabre en el redactor (también con doble clic o Intro).
        if draft {
            let label = pick("Edit draft", "Editar borrador");
            if tool_button(ui, Icon::Rename, label, &tip(label, "mail.edit_draft"), true).clicked() {
                fire = Some("mail.edit_draft");
            }
            ui.add_space(6.0);
        }
        for (id, ic, en, es) in [
            ("mail.reply", Icon::Reply, "Reply", "Responder"),
            ("mail.reply_all", Icon::ReplyAll, "Reply all", "Responder a todos"),
            ("mail.forward", Icon::Forward, "Forward", "Reenviar"),
        ] {
            let label = pick(en, es);
            let r = if narrow {
                icons::icon_button(ui, ic, &tip(label, id), true)
            } else {
                tool_button(ui, ic, label, &tip(label, id), true)
            };
            if r.clicked() {
                fire = Some(id);
            }
        }
        ui.add_space(6.0);
        for (id, ic, en, es) in [
            ("mail.archive", Icon::Archive, "Archive", "Archivar"),
            ("mail.delete", Icon::Trash, "Delete", "Eliminar"),
            ("mail.toggle_flag", Icon::Flag, "Flag", "Marcar para seguimiento"),
        ] {
            if icons::icon_button(ui, ic, &tip(pick(en, es), id), true).clicked() {
                fire = Some(id);
            }
        }
        crate::rules_ui::snooze_button(ui, &snooze, &mut extra);
        let more = icons::icon_button(ui, Icon::MoreHorizontal, pick("More actions", "Más acciones"), true);
        egui::Popup::menu(&more).show(|ui| {
            for (id, ic, en, es) in [
                ("mail.toggle_read", Icon::Check, "Mark as read / unread", "Marcar como leído / no leído"),
                ("mail.move", Icon::Folder, "Move to folder…", "Mover a carpeta…"),
                ("mail.toggle_conversations", Icon::List, "Group by conversation", "Agrupar por conversación"),
                ("mail.open_window", Icon::Open, "Open in window", "Abrir en ventana"),
                ("view.reading_focus", Icon::Maximize, "Focused reading", "Lectura enfocada"),
                ("view.zoom_in", Icon::ZoomIn, "Zoom in message", "Acercar el mensaje"),
                ("view.zoom_out", Icon::ZoomOut, "Zoom out message", "Alejar el mensaje"),
                ("view.zoom_reset", Icon::Fit, "Reset message zoom", "Restablecer el zoom del mensaje"),
                ("view.context_panel", Icon::Sparkle, "Show / hide context", "Mostrar / ocultar el contexto"),
                ("app.palette", Icon::Search, "All commands…", "Todos los comandos…"),
            ] {
                if icons::menu_item(ui, Some(ic), pick(en, es)).clicked() {
                    fire = Some(id);
                    ui.close();
                }
            }
            crate::rules_ui::more_items(ui, snoozed, &mut extra);
        });
    });
    if let Some(id) = fire {
        app.run(&ctx, id);
    }
    if let Some(id) = extra {
        app.run(&ctx, &id);
    }
}

pub(crate) fn message_card(app: &mut DeskApp, ui: &mut Ui, env: &MessageEnvelope, expanded: bool) {
    let ctx = ui.ctx().clone();
    let c = theme::colors(ui);
    let zone = app.session.zone();
    let from_name = env.from.first().and_then(|m| m.name.clone()).unwrap_or_default();
    let from_addr = env.from.first().map(|m| m.addr.clone()).unwrap_or_default();
    let header = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            avatar(ui, &from_name, &from_addr, if expanded { 36.0 } else { 28.0 });
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.horizontal(|ui| {
                    let name = if from_name.is_empty() { from_addr.clone() } else { from_name.clone() };
                    ui.label(
                        RichText::new(name)
                            .font(theme::bold_font(&ctx, theme::density(&ctx).body_size()))
                            .color(c.text),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(long_date(env.date, zone))
                                .color(c.text_muted)
                                .size(theme::density(&ctx).small_size()),
                        );
                        if env.flags.flagged {
                            badge(ui, pick("Flagged", "Seguimiento"), BadgeKind::Warning);
                        }
                    });
                });
                if expanded {
                    let to: Vec<String> = env.to.iter().map(|m| m.display().to_string()).collect();
                    let mut line = format!("{from_addr}  ·  {} {}", pick("To:", "Para:"), to.join(", "));
                    if !env.cc.is_empty() {
                        let cc: Vec<String> = env.cc.iter().map(|m| m.display().to_string()).collect();
                        line.push_str(&format!("   Cc: {}", cc.join(", ")));
                    }
                    ui.add(egui::Label::new(RichText::new(line).color(c.text_muted)).truncate());
                } else {
                    let p = painter_safe_preview(&env.preview);
                    ui.add(egui::Label::new(RichText::new(p).color(c.text_muted)).truncate());
                }
            });
        })
        .response
        .interact(Sense::click());
    iurefficient_ui::a11y::button(
        &header,
        &format!(
            "{} {}",
            pick("Message from", "Mensaje de"),
            if from_name.is_empty() { &from_addr } else { &from_name }
        ),
    );
    if header.clicked() {
        if app.mail.expanded.contains(&env.id) {
            app.mail.expanded.remove(&env.id);
        } else {
            app.mail.expanded.insert(env.id);
        }
    }
    if !expanded {
        return;
    }
    ui.add_space(10.0);
    message_body(app, ui, env);
}

fn message_body(app: &mut DeskApp, ui: &mut Ui, env: &MessageEnvelope) {
    let ctx = ui.ctx().clone();
    let c = theme::colors(ui);
    let Some(body) = app.session.body(env.id).cloned() else {
        // Mientras llega (o se descarga del servidor): el extracto y un esqueleto.
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().size(12.0));
            ui.label(
                RichText::new(if env.blob.is_some() {
                    pick("Opening…", "Abriendo…")
                } else {
                    pick("Downloading the message…", "Descargando el mensaje…")
                })
                .color(c.text_muted),
            );
        });
        ui.add_space(6.0);
        if !env.preview.trim().is_empty() {
            ui.add(egui::Label::new(RichText::new(&env.preview).color(c.text)).wrap());
            ui.add_space(6.0);
        }
        let w = ui.available_width().min(560.0);
        for k in 0..4 {
            let (r, _) = ui.allocate_exact_size(vec2(w * [0.92, 0.78, 0.85, 0.5][k], 10.0), Sense::hover());
            ui.painter().rect_filled(r, 4.0, c.surface_alt);
            ui.add_space(6.0);
        }
        return;
    };
    if let Some(note) = &body.note {
        let r = Banner::new(Severity::Warning, note.get()).action(pick("Retry", "Reintentar")).show(ui);
        if r.action {
            app.session.retry_body(env.id);
        }
        ui.add_space(8.0);
    }
    crate::security_ui::badge(ui, env.id, body.security.as_deref());
    crate::invitation::card(app, ui, env.id);
    let full = app.mail.show_quoted.contains(&env.id);
    let html_cut = body.html.as_deref().and_then(iuredesk_engine::body::strip_quoted_html);
    let html_view = if full { body.html.clone() } else { html_cut.clone().or_else(|| body.html.clone()) };
    // UX-3: zoom del cuerpo (Ctrl +/−/0): el HTML se pinta con menos ancho CSS y más escala.
    let zoom = app.session.prefs.reading_zoom();
    if let Some(html) = &html_view {
        // Los boletines suelen medir 600 px: por debajo se pinta a 600 y se reduce al mostrar.
        let width = HtmlCache::width_for((ui.available_width() / zoom).max(600.0));
        let key = HtmlKey {
            id: env.id,
            width,
            ppp_x100: (ctx.pixels_per_point() * zoom * 100.0).round() as u32,
            dark: ui.visuals().dark_mode,
            remote: app.html.remote_ready(env.id),
            full: full || html_cut.is_none(),
        };
        app.html.set_max_texture_side(ctx.input(|i| i.max_texture_side));
        let state = app.html.get(&mut app.session, key, html, &body.inline);
        match state {
            Some(HtmlState::Ready { tiles, size_px, links, blocked, scale }) => {
                let (tiles, size_px, links, blocked, scale) =
                    (tiles.clone(), *size_px, links.clone(), blocked.clone(), *scale);
                // UX-5: aviso con «Mostrar siempre de este remitente / dominio», o carga sola
                // según Ajustes → Privacidad.
                crate::remote_images_ui::banner(app, ui, env.id, html, blocked);
                // `scale` = píxeles por px CSS; en pantalla, cada px CSS mide `zoom` puntos.
                let natural = vec2(size_px[0] as f32 / scale, size_px[1] as f32 / scale) * zoom;
                // Acercado, lo ancho se desplaza en horizontal en vez de reducirse otra vez.
                let fit = if zoom > 1.0 { 1.0 } else { (ui.available_width() / natural.x).clamp(0.3, 1.0) };
                let size = natural * fit;
                let scale = size_px[0].max(1) as f32 / size.x.max(1.0);
                let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
                // Franjas de arriba abajo (un boletín largo no cabe en una sola textura).
                let full_h = size_px[1].max(1) as f32;
                let mut y = 0usize;
                for (tex, h) in &tiles {
                    let top = rect.top() + rect.height() * (y as f32 / full_h);
                    let bottom = rect.top() + rect.height() * ((y + h) as f32 / full_h);
                    let r = Rect::from_min_max(pos2(rect.left(), top), pos2(rect.right(), bottom));
                    ui.painter().image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                    y += h;
                }
                iurefficient_ui::a11y::label(&resp, &body.text.chars().take(400).collect::<String>());
                if let Some(p) = resp.hover_pos() {
                    let px = (p - rect.min) * scale;
                    if let Some(l) = links.iter().rev().find(|l| l.rect.contains(px.x, px.y)) {
                        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                        // Se muestra el destino real del enlace (protección contra phishing).
                        egui::Tooltip::always_open(
                            ctx.clone(),
                            ui.layer_id(),
                            resp.id.with("link"),
                            egui::PopupAnchor::Pointer,
                        )
                        .show(|ui| ui.label(&l.href));
                        if resp.clicked() {
                            let href = l.href.clone();
                            if href.to_ascii_lowercase().starts_with("mailto:") {
                                if let Some(d) = app.session.compose_mailto(&href) {
                                    app.open_composer(d);
                                }
                            } else if !crate::open_url(&href) {
                                app.notice(iuredesk_engine::Notice::error(iuredesk_engine::Text::new(
                                    "Could not open the link.",
                                    "No se pudo abrir el enlace.",
                                )));
                            }
                        }
                    }
                }
            }
            Some(HtmlState::Pending) | Some(HtmlState::Failed) | None => {
                quoted_text(app, ui, env.id, &body.text);
            }
        }
        if html_cut.is_some() {
            quoted_toggle(app, ui, env.id);
        }
    } else {
        quoted_text(app, ui, env.id, &body.text);
    }
    // UX-3: clic abre con la app del sistema; clic derecho, el resto. Si el cuerpo aún no está en
    // el equipo (solo los datos del índice), se descarga al pedirlo.
    use crate::attach_ui::Item;
    if !body.attachments.is_empty() {
        ui.add_space(10.0);
        let items: Vec<Item> = body
            .attachments
            .iter()
            .map(|a| Item { name: a.name.clone(), size: a.size, mime: a.mime.clone() })
            .collect();
        crate::attach_ui::show(app, ui, env.id, &items, body.note.is_none());
    } else if env.has_attachments {
        ui.add_space(10.0);
        let items: Vec<Item> = env
            .attachments
            .iter()
            .filter(|a| a.content_id.is_none())
            .map(|a| Item {
                name: a.filename.clone(),
                size: usize::try_from(a.size).unwrap_or(usize::MAX),
                mime: a.mime_type.clone(),
            })
            .collect();
        crate::attach_ui::show(app, ui, env.id, &items, false);
    }
}

/// Texto plano con la historia citada plegada tras «… mostrar texto citado» (como Gmail).
fn quoted_text(app: &mut DeskApp, ui: &mut Ui, id: MessageId, text: &str) {
    let zoom = app.session.prefs.reading_zoom();
    let (visible, quoted) = iuredesk_engine::body::split_quoted(text);
    match quoted {
        None => plain_text(ui, text, zoom),
        Some(q) => {
            plain_text(ui, &visible, zoom);
            quoted_toggle(app, ui, id);
            if app.mail.show_quoted.contains(&id) {
                plain_text(ui, &q, zoom);
            }
        }
    }
}

fn quoted_toggle(app: &mut DeskApp, ui: &mut Ui, id: MessageId) {
    let c = theme::colors(ui);
    let open = app.mail.show_quoted.contains(&id);
    let label = if open {
        pick("Hide quoted text", "Ocultar texto citado")
    } else {
        pick("… show quoted text", "… mostrar texto citado")
    };
    let r = ui.add(
        egui::Button::new(RichText::new(label).color(c.text_muted).size(theme::density(ui.ctx()).small_size()))
            .fill(c.surface_alt)
            .corner_radius(8),
    );
    if r.clicked() {
        if open {
            app.mail.show_quoted.remove(&id);
        } else {
            app.mail.show_quoted.insert(id);
        }
    }
    ui.add_space(4.0);
}

fn plain_text(ui: &mut Ui, text: &str, zoom: f32) {
    let c = theme::colors(ui);
    let size = (theme::density(ui.ctx()).body_size() + 0.5) * zoom;
    // Las líneas citadas («> …») se atenúan, como en los clientes de escritorio.
    let mut job = egui::text::LayoutJob::default();
    for line in text.lines().take(5_000) {
        let quoted = line.trim_start().starts_with('>');
        let fmt = egui::TextFormat {
            font_id: egui::FontId::proportional(size),
            color: if quoted { c.text_muted } else { c.text },
            line_height: Some(size * 1.45),
            ..Default::default()
        };
        job.append(line, 0.0, fmt.clone());
        job.append("\n", 0.0, fmt);
    }
    job.wrap.max_width = ui.available_width();
    ui.add(egui::Label::new(job).selectable(true).wrap());
}

// ---------------------------------------------------------------------------------------------
// Panel contextual

/// Atajo de un comando como texto («Ctrl+Shift+I»), para las ayudas.
fn shortcut_hint(app: &DeskApp, id: &str) -> String {
    iuredesk_engine::command::shortcut(id, app.session.scheme())
        .map(|k| format!("  ({})", k.label(cfg!(target_os = "macos"))))
        .unwrap_or_default()
}

/// Resumen del contexto en pocas palabras (riel y franja).
fn iure_summary(p: &IurePanel) -> String {
    if !matches!(p.status, iuredesk_engine::iure::IureStatus::Ready { .. }) {
        return p.status.label().get().to_string();
    }
    let mut parts: Vec<String> = Vec::new();
    let n = |k: usize, en: &str, ens: &str, es: &str, ess: &str| {
        format!("{k} {}", if k == 1 { pick(en, es) } else { pick(ens, ess) })
    };
    if !p.links.is_empty() {
        parts.push(n(p.links.len(), "link", "links", "vínculo", "vínculos"));
    }
    if let Some(c) = &p.context {
        if !c.deals.is_empty() {
            parts.push(n(c.deals.len(), "deal", "deals", "oportunidad", "oportunidades"));
        }
        if !c.cases.is_empty() {
            parts.push(n(c.cases.len(), "case", "cases", "caso", "casos"));
        }
        if !c.tickets.is_empty() {
            parts.push(n(c.tickets.len(), "ticket", "tickets", "ticket", "tickets"));
        }
        if parts.is_empty() && !c.people.is_empty() {
            parts.push(n(c.people.len(), "person", "people", "persona", "personas"));
        }
    }
    if parts.is_empty() { pick("Nothing linked yet", "Nada vinculado aún").to_string() } else { parts.join(" · ") }
}

/// Elementos del contexto (insignia del riel).
fn iure_count(p: &IurePanel) -> usize {
    let c = p.context.as_ref().map_or(0, |c| c.deals.len() + c.cases.len() + c.tickets.len());
    p.links.len().saturating_add(c)
}

/// Columna completa, con su botón para plegarla al riel.
fn context_panel(app: &mut DeskApp, ui: &mut Ui, card: &ContextCard, iure: Option<&(MessageId, IurePanel)>) {
    let ctx = ui.ctx().clone();
    let c = theme::colors(ui);
    let mut collapse = false;
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(pick("Context", "Contexto")).color(c.text_muted).size(theme::density(&ctx).small_size()),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let tip =
                format!("{}{}", pick("Collapse panel", "Plegar el panel"), shortcut_hint(app, "view.context_panel"));
            if icons::icon_button(ui, Icon::ChevronRight, &tip, true).clicked() {
                collapse = true;
            }
        });
    });
    ui.add_space(4.0);
    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("context-scroll").show(ui, |ui| {
        context_inline(ui, card);
        if let Some((id, p)) = iure {
            crate::iure::panel(app, ui, *id, p);
        }
    });
    if collapse {
        app.run(&ctx, "view.context_panel");
    }
}

/// Riel delgado del panel plegado: iconos con insignias; cualquier clic lo despliega.
fn context_rail(app: &mut DeskApp, ui: &mut Ui, card: &ContextCard, iure: Option<&(MessageId, IurePanel)>) {
    let ctx = ui.ctx().clone();
    let frame = split::frame(ui, PaneKind::Sidebar).inner_margin(egui::Margin::symmetric(4, 8));
    let mut expand = false;
    egui::Panel::right("context-rail").frame(frame).exact_size(CONTEXT_RAIL).resizable(false).show(ui, |ui| {
        let c = theme::colors(ui);
        ui.vertical_centered(|ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            let tip = format!(
                "{}{}",
                pick("Show context panel", "Mostrar el panel contextual"),
                shortcut_hint(app, "view.context_panel")
            );
            if icons::icon_button(ui, Icon::ChevronLeft, &tip, true).clicked() {
                expand = true;
            }
            if let Some(p) = &card.contact {
                let role = [p.title.as_str(), p.org.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>();
                let tip = if role.is_empty() {
                    p.full_name.clone()
                } else {
                    format!("{} — {}", p.full_name, role.join(" · "))
                };
                let r = icons::icon_button(ui, Icon::Person, &tip, true);
                if r.clicked() {
                    expand = true;
                }
            }
            if let Some((_, p)) = iure {
                let tip = format!("Iurefficient — {}", iure_summary(p));
                let r = icons::icon_button(ui, Icon::Sparkle, &tip, true);
                let n = iure_count(p);
                if n > 0 {
                    let center = r.rect.right_top() + vec2(-3.0, 4.0);
                    ui.painter().circle_filled(center, 7.0, c.brand);
                    ui.painter().text(
                        center,
                        egui::Align2::CENTER_CENTER,
                        if n > 9 { "9+".to_string() } else { n.to_string() },
                        egui::FontId::proportional(9.0),
                        c.on_accent,
                    );
                }
                if r.clicked() {
                    expand = true;
                }
            }
        });
    });
    if expand {
        app.run(&ctx, "view.context_panel");
    }
}

/// Franja compacta bajo la cabecera del mensaje: el contacto y el resumen de Iurefficient en una
/// línea; desplegada, las tarjetas completas.
fn context_strip(app: &mut DeskApp, ui: &mut Ui, card: &ContextCard, iure: Option<&(MessageId, IurePanel)>) {
    let ctx = ui.ctx().clone();
    let c = theme::colors(ui);
    let open = app.mail.strip_open;
    let mut toggle = false;
    egui::Frame::new()
        .fill(c.surface_alt)
        .stroke(Stroke::new(1.0, c.separator))
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(10, 5))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let row = ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let w = (ui.available_width() - 30.0).max(80.0);
                ui.allocate_ui_with_layout(vec2(w, 22.0), Layout::left_to_right(Align::Center), |ui| {
                    ui.set_width(w);
                    if let Some(p) = &card.contact {
                        avatar(ui, &p.full_name, p.primary_email().unwrap_or(""), 20.0);
                        ui.add(egui::Label::new(RichText::new(&p.full_name).strong().color(c.text)).selectable(false));
                        if !p.org.is_empty() {
                            ui.add(
                                egui::Label::new(RichText::new(&p.org).color(c.text_muted))
                                    .truncate()
                                    .selectable(false),
                            );
                        }
                    }
                    if let Some((_, p)) = iure {
                        if card.contact.is_some() {
                            ui.label(RichText::new("·").color(c.text_muted));
                        }
                        let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                        ui.painter().circle_filled(r.center(), 4.5, c.brand);
                        ui.add(
                            egui::Label::new(RichText::new("Iurefficient").strong().color(c.text)).selectable(false),
                        );
                        ui.add(
                            egui::Label::new(RichText::new(iure_summary(p)).color(c.text_muted))
                                .truncate()
                                .selectable(false),
                        );
                    }
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let tip = format!(
                        "{}{}",
                        if open {
                            pick("Hide details", "Ocultar detalles")
                        } else {
                            pick("Show details", "Ver detalles")
                        },
                        shortcut_hint(app, "view.context_panel")
                    );
                    if icons::icon_button(ui, if open { Icon::ChevronUp } else { Icon::ChevronDown }, &tip, true)
                        .clicked()
                    {
                        toggle = true;
                    }
                });
            });
            let r = row.response.interact(Sense::click());
            iurefficient_ui::a11y::button(&r, pick("Context of the message", "Contexto del mensaje"));
            if r.clicked() {
                toggle = true;
            }
            if open {
                ui.add_space(6.0);
                egui::ScrollArea::vertical().max_height(300.0).id_salt("context-strip-scroll").show(ui, |ui| {
                    context_inline(ui, card);
                    if let Some((id, p)) = iure {
                        crate::iure::panel(app, ui, *id, p);
                    }
                });
            }
        });
    if toggle {
        let _ = &ctx;
        app.mail.strip_open = !app.mail.strip_open;
    }
}

fn context_inline(ui: &mut Ui, card: &ContextCard) {
    let c = theme::colors(ui);
    if let Some(p) = &card.contact {
        egui::Frame::new()
            .fill(c.surface_alt)
            .stroke(Stroke::new(1.0, c.separator))
            .corner_radius(8)
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    avatar(ui, &p.full_name, p.primary_email().unwrap_or(""), 40.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&p.full_name).font(theme::bold_font(ui.ctx(), 15.0)).color(c.text));
                        let role = [p.title.as_str(), p.org.as_str()]
                            .into_iter()
                            .filter(|s| !s.is_empty())
                            .collect::<Vec<_>>()
                            .join(" · ");
                        if !role.is_empty() {
                            ui.label(RichText::new(role).color(c.text_muted));
                        }
                    });
                });
                ui.add_space(6.0);
                if let Some(e) = p.primary_email() {
                    crate::widgets::info_row(ui, Icon::Mail, "", e);
                }
                if let Some(ph) = p.phones.first() {
                    crate::widgets::info_row(ui, Icon::Phone, "", &ph.value);
                }
            });
        ui.add_space(8.0);
    }
}
