//! Correo: carpetas (favoritos, cuentas y panel «Hoy»), lista virtual densa, lectura y panel
//! contextual.

use std::collections::HashSet;

use egui::{Align, Color32, Layout, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use iuredesk_engine::context::{ContextCard, LinkKind};
use iuredesk_engine::time::{list_date, long_date};
use iuredesk_engine::{DensityPref, FolderSel, ListRow};
use iuredesk_model::{AccountId, FolderRole, MessageEnvelope, MessageId};
use iurefficient_ui::list::{Column, ListState, ListView, RowStyle};
use iurefficient_ui::{
    BadgeKind, Banner, EmptyState, Icon, PaneKind, SearchField, Severity, avatar, badge, icons, pick, section_label,
    segmented, sidebar_item, split, theme,
};

use crate::app::DeskApp;
use crate::html::{HtmlCache, HtmlKey, HtmlState};
use crate::widgets::{hairline, human_size, pane_title, tool_button, wide_primary};

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
            search: String::new(),
            focus_search: false,
            expanded_accounts: HashSet::new(),
            accounts_init: false,
            expanded: HashSet::new(),
            conv: None,
            card: None,
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

/// La vista de correo completa.
pub fn show(app: &mut DeskApp, ui: &mut Ui) {
    split::left(ui, "folders", PaneKind::Sidebar, 244.0, 190.0..=380.0, |ui| folders(app, ui));
    split::top_bar(ui, "mail-top", |ui| top_bar(app, ui));

    let focused = app.session.focused();
    let card = focused.map(|id| context_card(app, id)).unwrap_or_default();
    // Ancho mínimo de la lista: la densa lleva columnas (De, Asunto, Recibido).
    let dense = app.session.prefs.density == DensityPref::Dense;
    let list_min = if dense { 560.0 } else { 400.0 };
    let avail = ui.available_width();
    // El panel contextual solo aparece si deja sitio a la lista y a la lectura.
    let wide = avail - 264.0 >= list_min + 460.0;
    if wide && !card.is_empty() {
        split::right(ui, "context", PaneKind::Detail, 264.0, 220.0..=380.0, |ui| context_panel(app, ui, &card));
    }
    let avail = ui.available_width();
    let reading_w = avail - list_min;
    // La lista conserva siempre su mínimo: el panel de lectura no puede comérsela aunque su
    // contenido pida más ancho.
    let max_reading = (avail - list_min).max(340.0);
    let frame = split::frame(ui, PaneKind::Detail);
    egui::Panel::right("reading")
        .frame(frame)
        .resizable(true)
        .default_size(reading_w.clamp(340.0, max_reading))
        .size_range(340.0..=max_reading)
        .show(ui, |ui| {
            ui.set_max_width(ui.available_width());
            reading_pane(app, ui, if wide { None } else { Some(&card) })
        });
    split::center(ui, PaneKind::Content, |ui| message_list(app, ui));
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
        for l in labels {
            let open = app.mail.expanded_accounts.contains(&l.id);
            if account_header(ui, &l.name, &l.email, open).clicked() {
                if open {
                    app.mail.expanded_accounts.remove(&l.id);
                } else {
                    app.mail.expanded_accounts.insert(l.id);
                }
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
                    ui.horizontal(|ui| {
                        ui.add_space(14.0);
                        let on = !searching && current == FolderSel::Folder(fid);
                        if sidebar_item(
                            ui,
                            Some(folder_icon(role)),
                            &name,
                            u32::try_from(unread).unwrap_or(u32::MAX),
                            on,
                        )
                        .clicked()
                        {
                            app.session.select_folder(FolderSel::Folder(fid));
                            app.mail.search.clear();
                        }
                    });
                }
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
// Barra superior

fn top_bar(app: &mut DeskApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let search_id = ui.make_persistent_id(egui::Id::unique("mail-search"));
        if app.mail.focus_search {
            app.mail.focus_search = false;
            ui.memory_mut(|m| m.request_focus(search_id));
        }
        let resp = SearchField::new(&mut app.mail.search)
            .id_salt("mail-search")
            .hint(pick("Search mail  (from: has:attachment …)", "Buscar en el correo  (de: tiene:adjunto …)"))
            .desired_width((ui.available_width() * 0.55).clamp(220.0, 520.0))
            .show(ui);
        if resp.changed() {
            let q = app.mail.search.clone();
            app.session.set_search(&q);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let r = ui.add(egui::Button::new(pick("Commands", "Comandos")).frame(true));
            iurefficient_ui::keycap(ui, "Ctrl+K");
            ui.add_space(12.0);
            let mut d = app.session.prefs.density;
            let seg = segmented(
                ui,
                &mut d,
                &[
                    (DensityPref::Comfortable, pick("Comfortable", "Cómoda")),
                    (DensityPref::Compact, pick("Compact", "Compacta")),
                    (DensityPref::Dense, pick("Dense", "Densa")),
                ],
            );
            if seg.changed() {
                let id = format!("view.density.{}", d.key());
                app.run(&ctx, &id);
            }
            if r.on_hover_text(pick("Everything IureDesk can do", "Todo lo que IureDesk puede hacer")).clicked() {
                app.run(&ctx, "app.palette");
            }
        });
    });
}

// ---------------------------------------------------------------------------------------------
// Lista

fn message_list(app: &mut DeskApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let c = theme::colors(ui);
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
        row.style(RowStyle { unread, flagged: env.flags.flagged, muted: false });
        let col = *row.colors();
        let who = if sent_like.contains(&env.folder) {
            let to = env.to.first().map(|m| m.display().to_string()).unwrap_or_default();
            format!("{} {to}", pick("To:", "Para:"))
        } else {
            env.sender_display().to_string()
        };
        let date = list_date(env.date, now, zone);
        let subject = if env.subject.trim().is_empty() {
            pick("(no subject)", "(sin asunto)").to_string()
        } else {
            env.subject.clone()
        };
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
            row.text(2, &subject);
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
        let text_rect = Rect::from_min_max(line2.min, pos2(line2.right() - icons_w, line2.bottom()));
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
    if out.selection_changed {
        let ids: Vec<MessageId> = mail.list.selection.keys().map(MessageId).collect();
        let cursor = mail.list.selection.cursor().and_then(|i| rows.get(i)).map(|r| r.id);
        session.set_selection(ids, cursor);
        mail.expanded.clear();
    }
    if let Some(i) = out.activated
        && let Some(r) = rows.get(i)
    {
        session.set_selection(vec![r.id], Some(r.id));
    }
    if let Some(i) = out.context {
        let _ = i;
        let resp = out.response.clone();
        message_menu(app, &resp);
    }
}

/// Extracto en una línea.
fn painter_safe_preview(p: &str) -> String {
    p.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn message_menu(app: &mut DeskApp, resp: &egui::Response) {
    let ctx = resp.ctx.clone();
    let mut chosen: Option<&'static str> = None;
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
        ] {
            if icons::menu_item(ui, Some(ic), pick(en, es)).clicked() {
                chosen = Some(id);
                ui.close();
            }
        }
    });
    if let Some(id) = chosen {
        app.run(&ctx, id);
    }
}

// ---------------------------------------------------------------------------------------------
// Lectura

fn reading_pane(app: &mut DeskApp, ui: &mut Ui, inline_card: Option<&ContextCard>) {
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
    ui.add(
        egui::Label::new(
            RichText::new(if base.is_empty() { subject.clone() } else { base })
                .font(theme::bold_font(&ctx, 19.0))
                .color(c.text),
        )
        .wrap(),
    );
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
    ui.add_space(4.0);
    hairline(ui);

    let last = conv.last().map(|e| e.id);
    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("reading-scroll").show(ui, |ui| {
        ui.add_space(8.0);
        if let Some(card) = inline_card
            && !card.is_empty()
        {
            context_inline(ui, card);
            ui.add_space(8.0);
        }
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
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let narrow = ui.available_width() < 520.0;
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
        let more = icons::icon_button(ui, Icon::MoreHorizontal, pick("More actions", "Más acciones"), true);
        egui::Popup::menu(&more).show(|ui| {
            for (id, ic, en, es) in [
                ("mail.toggle_read", Icon::Check, "Mark as read / unread", "Marcar como leído / no leído"),
                ("mail.move", Icon::Folder, "Move to folder…", "Mover a carpeta…"),
                ("mail.toggle_conversations", Icon::List, "Group by conversation", "Agrupar por conversación"),
                ("app.palette", Icon::Search, "All commands…", "Todos los comandos…"),
            ] {
                if icons::menu_item(ui, Some(ic), pick(en, es)).clicked() {
                    fire = Some(id);
                    ui.close();
                }
            }
        });
    });
    if let Some(id) = fire {
        app.run(&ctx, id);
    }
}

fn message_card(app: &mut DeskApp, ui: &mut Ui, env: &MessageEnvelope, expanded: bool) {
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
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().size(12.0));
            ui.label(RichText::new(&env.preview).color(c.text));
        });
        return;
    };
    if let Some(html) = &body.html {
        // Los boletines suelen medir 600 px: por debajo se pinta a 600 y se reduce al mostrar.
        let width = HtmlCache::width_for(ui.available_width().max(600.0));
        let key = HtmlKey {
            id: env.id,
            width,
            ppp_x100: (ctx.pixels_per_point() * 100.0).round() as u32,
            dark: ui.visuals().dark_mode,
            remote: app.html.remote_ready(env.id),
        };
        let state = app.html.get(&mut app.session, key, html, &body.inline);
        match state {
            Some(HtmlState::Ready { tex, size_px, links, blocked, scale }) => {
                let (tex, size_px, links, blocked, scale) =
                    (tex.clone(), *size_px, links.clone(), blocked.clone(), *scale);
                if !blocked.is_empty() && !app.html.remote_ready(env.id) {
                    let loading = app.html.remote_loading(env.id);
                    let r = Banner::new(
                        Severity::Info,
                        if loading {
                            pick("Downloading remote images…", "Descargando imágenes remotas…")
                        } else {
                            pick(
                                "Remote images blocked to protect your privacy.",
                                "Imágenes remotas bloqueadas para proteger tu privacidad.",
                            )
                        },
                    )
                    .action(pick("Show", "Mostrar"))
                    .show(ui);
                    if r.action && !loading {
                        app.html.allow_remote(&mut app.session, env.id, blocked);
                    }
                    ui.add_space(8.0);
                }
                let natural = vec2(size_px[0] as f32 / scale, size_px[1] as f32 / scale);
                let fit = (ui.available_width() / natural.x).clamp(0.3, 1.0);
                let size = natural * fit;
                let scale = scale / fit;
                let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
                ui.painter().image(tex.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
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
                                if let Some(d) = iuredesk_engine::ComposeDraft::from_mailto(&href) {
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
            Some(HtmlState::Pending) => {
                plain_text(ui, &body.text);
            }
            Some(HtmlState::Failed) | None => {
                plain_text(ui, &body.text);
            }
        }
    } else {
        plain_text(ui, &body.text);
    }
    if !body.attachments.is_empty() {
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            for a in &body.attachments {
                attachment_chip(ui, &a.name, a.size);
            }
        });
    } else if env.has_attachments {
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            for a in &env.attachments {
                attachment_chip(ui, &a.filename, usize::try_from(a.size).unwrap_or(usize::MAX));
            }
        });
    }
}

fn plain_text(ui: &mut Ui, text: &str) {
    let c = theme::colors(ui);
    let size = theme::density(ui.ctx()).body_size() + 0.5;
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

fn attachment_chip(ui: &mut Ui, name: &str, size: usize) {
    let c = theme::colors(ui);
    egui::Frame::new()
        .fill(c.surface_alt)
        .stroke(Stroke::new(1.0, c.separator))
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(8, 5))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                icons::paint(ui.painter(), r, Icon::Attachment, c.text_muted, c.accent);
                ui.label(RichText::new(name).color(c.text));
                ui.label(
                    RichText::new(human_size(size)).color(c.text_muted).size(theme::density(ui.ctx()).small_size()),
                );
            });
        });
}

// ---------------------------------------------------------------------------------------------
// Panel contextual

fn context_panel(_app: &mut DeskApp, ui: &mut Ui, card: &ContextCard) {
    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("context-scroll").show(ui, |ui| {
        context_inline(ui, card);
    });
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
    if !card.links.is_empty() {
        egui::Frame::new()
            .fill(c.surface)
            .stroke(Stroke::new(1.0, c.separator))
            .corner_radius(8)
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                    ui.painter().circle_filled(r.center(), 6.0, c.accent);
                    ui.label(RichText::new("Iurefficient").font(theme::bold_font(ui.ctx(), 13.0)).color(c.text));
                });
                ui.add_space(4.0);
                for l in &card.links {
                    ui.horizontal(|ui| {
                        let (kind, k) = match l.kind {
                            LinkKind::Case => (pick("Case", "Caso"), BadgeKind::Accent),
                            LinkKind::Lead => ("Lead", BadgeKind::Success),
                        };
                        badge(ui, kind, k);
                        ui.label(RichText::new(&l.title).strong().color(c.text));
                    });
                    ui.label(
                        RichText::new(l.detail.get()).color(c.text_muted).size(theme::density(ui.ctx()).small_size()),
                    );
                    ui.add_space(4.0);
                }
                ui.label(
                    RichText::new(pick(
                        "Live links with your instance arrive in F3.",
                        "Los vínculos en vivo con tu instancia llegan en F3.",
                    ))
                    .italics()
                    .color(c.text_muted)
                    .size(theme::density(ui.ctx()).small_size()),
                );
            });
    }
}
