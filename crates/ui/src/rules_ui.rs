//! F5.3 en la interfaz: Ajustes → «Reglas» (lista y editor en lenguaje de cualquier usuario:
//! «Cuando llegue un correo de… → Moverlo a…»), respuesta automática, filtros del servidor
//! (Sieve), menú «Posponer», seguimientos en «Hoy», pestañas Enfocados/Otros, plantillas en el
//! redactor y baja de listas con confirmación.

use chrono::NaiveDate;
use egui::{Context, RichText, Ui};
use iuredesk_engine::iuredesk_rules::category::Category;
use iuredesk_engine::iuredesk_rules::vacation::Vacation;
use iuredesk_engine::iuredesk_rules::{Action, Condition, Field, FolderRef, Rule, TextOp};
use iuredesk_engine::{Notice, RulePlace, Text};
use iuredesk_model::{AccountId, MessageId};
use iurefficient_ui::{Icon, icons, pick, primary_button, section_label, segmented, theme};

use crate::app::DeskApp;
use crate::widgets::named;

fn bi(b: iuredesk_engine::iuredesk_rules::Bi) -> String {
    pick(&b.en, &b.es).to_string()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Tab {
    #[default]
    Rules,
    Vacation,
    Server,
}

/// Formulario de la respuesta automática.
struct VacationForm {
    v: Vacation,
    start: String,
    end: String,
    error: Option<String>,
}

/// Diálogo «Posponer hasta…».
struct SnoozeDialog {
    date: String,
    time: String,
    error: Option<String>,
}

/// Estado de las ventanas de F5.3.
#[derive(Default)]
pub(crate) struct RulesUi {
    open: bool,
    tab: Tab,
    editing: Option<(Rule, Option<String>)>,
    confirm_delete: Option<u64>,
    vacation: Option<VacationForm>,
    snooze: Option<SnoozeDialog>,
    followup: bool,
    unsubscribe: Option<(MessageId, String)>,
}

impl RulesUi {
    /// Hay una ventana modal abierta (los atajos no se disparan).
    pub fn modal(&self) -> bool {
        self.open || self.snooze.is_some() || self.followup || self.unsubscribe.is_some()
    }
}

/// Atiende `Outcome::Rules`.
pub(crate) fn command(app: &mut DeskApp, _ctx: &Context, id: &str) {
    match id {
        "rules" => {
            app.rules.open = true;
            app.rules.tab = Tab::Rules;
            app.session.sieve_detect();
        }
        "vacation" => {
            app.rules.open = true;
            app.rules.tab = Tab::Vacation;
        }
        "snooze" => {
            let tomorrow = app.session.today().succ_opt().unwrap_or(app.session.today());
            app.rules.snooze =
                Some(SnoozeDialog { date: tomorrow.format("%Y-%m-%d").to_string(), time: "08:00".into(), error: None });
        }
        "followup" => app.rules.followup = true,
        "rule_from_message" => {
            if let Some(id) = app.session.focused()
                && let Some(r) = app.session.rule_from_message(id)
            {
                app.rules.open = true;
                app.rules.tab = Tab::Rules;
                app.rules.editing = Some((r, None));
            }
        }
        "labels" => app.open_settings(crate::settings::Section::Labels, None, None),
        "always_category" => app.notice(Notice::info(
            "Right-click the message → Always move to…",
            "Clic derecho en el mensaje → Mover siempre a…",
        )),
        "unsubscribe" => {
            let Some(id) = app.session.focused() else { return };
            match app.session.unsubscribe_method(id) {
                Some((_, why)) => app.rules.unsubscribe = Some((id, why.get().to_string())),
                None => app.notice(Notice::info(
                    "This sender doesn't offer a way to unsubscribe.",
                    "Este remitente no ofrece forma de darse de baja.",
                )),
            }
        }
        other => {
            if let Some(url) = other.strip_prefix("open_url:")
                && !crate::open_url(url)
            {
                app.notice(Notice::error(Text::new("The page could not be opened.", "No se pudo abrir la página.")));
            }
        }
    }
}

// ------------------------------------------------------------------------- piezas para el correo

/// Entradas del menú «Posponer» (`mail.snooze:<clave>`), con la hora a la que vuelve.
pub(crate) fn snooze_items(ui: &mut Ui, menu: &[(String, Text, String)], out: &mut Option<String>) {
    for (key, label, when) in menu {
        if icons::menu_item(ui, Some(Icon::Clock), &format!("{} — {when}", label.get())).clicked() {
            *out = Some(format!("mail.snooze:{key}"));
            ui.close();
        }
    }
    if icons::menu_item(ui, Some(Icon::Calendar), pick("Pick date & time…", "Elegir fecha y hora…")).clicked() {
        *out = Some("mail.snooze".into());
        ui.close();
    }
}

/// Más acciones del mensaje (menú contextual y «Más acciones»).
pub(crate) fn more_items(ui: &mut Ui, snoozed: bool, out: &mut Option<String>) {
    ui.menu_button(pick("Always move to…", "Mover siempre a…"), |ui| {
        for c in Category::ALL {
            if icons::menu_item(ui, Some(category_icon(c)), &bi(c.label())).clicked() {
                *out = Some(format!("mail.always_category:{}", c.key()));
                ui.close();
            }
        }
    });
    let items: [(&str, Icon, &str, &str); 5] = [
        ("mail.followup", Icon::Bell, "Remind me if nobody replies…", "Recordarme si no responden…"),
        (
            "mail.rule_from_message",
            Icon::Filter,
            "Create rule from this message…",
            "Crear regla a partir de este correo…",
        ),
        ("mail.unsubscribe", Icon::Close, "Unsubscribe…", "Cancelar suscripción…"),
        ("mail.apply_rules", Icon::Filter, "Apply rules to this folder", "Aplicar reglas a esta carpeta"),
        ("mail.unsnooze", Icon::Undo, "Unsnooze", "Quitar posposición"),
    ];
    for (id, ic, en, es) in items {
        if id == "mail.unsnooze" && !snoozed {
            continue;
        }
        if icons::menu_item(ui, Some(ic), pick(en, es)).clicked() {
            *out = Some(id.into());
            ui.close();
        }
    }
}

/// Submenú «Posponer» y más acciones para el menú contextual de la lista.
pub(crate) fn context_items(ui: &mut Ui, menu: &[(String, Text, String)], snoozed: bool, out: &mut Option<String>) {
    ui.separator();
    ui.menu_button(pick("Snooze", "Posponer"), |ui| snooze_items(ui, menu, out));
    more_items(ui, snoozed, out);
}

/// Botón «Posponer» de la barra de lectura.
pub(crate) fn snooze_button(ui: &mut Ui, menu: &[(String, Text, String)], out: &mut Option<String>) {
    let r = icons::icon_button(ui, Icon::Clock, pick("Snooze", "Posponer"), true);
    egui::Popup::menu(&r).show(|ui| snooze_items(ui, menu, out));
}

fn muted_note(ui: &mut Ui, text: &str) {
    let c = theme::colors(ui);
    ui.add(
        egui::Label::new(RichText::new(text).color(c.text_muted).size(theme::density(ui.ctx()).small_size())).wrap(),
    );
}

/// Icono de una categoría.
pub(crate) fn category_icon(c: Category) -> Icon {
    match c {
        Category::Primary => Icon::Inbox,
        Category::Promotions => Icon::Tag,
        Category::Social => Icon::Contacts,
        Category::Updates => Icon::Info,
        Category::Forums => Icon::Comment,
    }
}

/// Pestañas de categorías sobre la lista (solo en la bandeja de entrada), con no leídos.
pub(crate) fn category_tabs(app: &mut DeskApp, ui: &mut Ui) {
    if !app.session.category_tabs_shown() {
        return;
    }
    let counts = app.session.category_counts();
    let iure_tab = app.session.iure_tab();
    let iure_shown = app.session.iure_tab_shown();
    let (_, iure_unread) = app.session.iure_tab_counts();
    let current = app.session.category_tab();
    let c = theme::colors(ui);
    let d = theme::density(ui.ctx());
    let mut chosen = None;
    let mut chose_iure = false;
    egui::Frame::new().inner_margin(egui::Margin { left: 10, right: 10, top: 0, bottom: 0 }).show(ui, |ui| {
        // UX-5: con la pestaña Iurefficient las pestañas pueden no caber: se desplazan en vez de
        // ensanchar la lista (que entonces se cortaba a la derecha).
        let w = ui.available_width();
        egui::ScrollArea::horizontal()
            .id_salt("category-tabs")
            .max_width(w)
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    // UX-5: «Iurefficient», la primera: todo lo que tiene relación (filtro transversal).
                    if iure_shown && iure_tab_button(ui, iure_tab, iure_unread) {
                        chose_iure = true;
                    }
                    for (k, cat) in Category::ALL.into_iter().enumerate() {
                        let (total, unread) = counts.get(k).copied().unwrap_or((0, 0));
                        if cat != Category::Primary && total == 0 && (iure_tab || cat != current) {
                            continue;
                        }
                        let label = bi(cat.label());
                        let current = if iure_tab { None } else { Some(current) };
                        let cat_on = Some(cat) == current;
                        let font =
                            if cat_on { theme::bold_font(ui.ctx(), d.body_size()) } else { theme::body_font(ui.ctx()) };
                        let ink = if cat_on { c.text } else { c.text_muted };
                        let g = ui.painter().layout_no_wrap(label.clone(), font, ink);
                        let badge = (unread > 0).then(|| unread.min(999).to_string());
                        let bw = badge.as_ref().map_or(0.0, |b| 10.0 + 7.0 * b.len() as f32);
                        let w = 12.0 + 16.0 + 6.0 + g.size().x + bw + 12.0;
                        let (rect, resp) =
                            ui.allocate_exact_size(egui::vec2(w, d.control_height() + 6.0), egui::Sense::click());
                        let col = crate::widgets::rgb(cat.color());
                        if resp.hovered() {
                            ui.painter().rect_filled(rect, 4.0, c.hover);
                        }
                        let ir = egui::Rect::from_center_size(
                            egui::pos2(rect.left() + 20.0, rect.center().y),
                            egui::vec2(16.0, 16.0),
                        );
                        icons::paint(ui.painter(), ir, category_icon(cat), if cat_on { col } else { ink }, col);
                        ui.painter().galley(
                            egui::pos2(ir.right() + 6.0, rect.center().y - g.size().y / 2.0),
                            g.clone(),
                            ink,
                        );
                        if let Some(b) = &badge {
                            let br = egui::Rect::from_min_size(
                                egui::pos2(ir.right() + 6.0 + g.size().x + 6.0, rect.center().y - 8.0),
                                egui::vec2(bw - 4.0, 16.0),
                            );
                            ui.painter().rect_filled(br, 8.0, col);
                            ui.painter().text(
                                br.center(),
                                egui::Align2::CENTER_CENTER,
                                b,
                                theme::small_font(ui.ctx()),
                                egui::Color32::WHITE,
                            );
                        }
                        if cat_on {
                            let bar = egui::Rect::from_min_max(
                                egui::pos2(rect.left() + 4.0, rect.bottom() - 3.0),
                                egui::pos2(rect.right() - 4.0, rect.bottom()),
                            );
                            ui.painter().rect_filled(bar, 1.5, col);
                        }
                        let name = match &badge {
                            Some(b) => format!("{label}, {b} {}", pick("unread", "sin leer")),
                            None => label.clone(),
                        };
                        iurefficient_ui::a11y::toggle(&resp, &name, cat_on);
                        if resp.clicked() {
                            chosen = Some(cat);
                        }
                    }
                })
            });
    });
    if let Some(cat) = chosen {
        app.session.set_category_tab(cat);
    } else if chose_iure {
        app.session.set_iure_tab();
    }
}

/// UX-5: pestaña «Iurefficient» (solo el símbolo, con «Iurefficient» al pasar y para lectores
/// de pantalla) y su contador de no leídos. Devuelve `true` si se pulsó.
fn iure_tab_button(ui: &mut Ui, on: bool, unread: usize) -> bool {
    let c = theme::colors(ui);
    let d = theme::density(ui.ctx());
    let dark = ui.visuals().dark_mode;
    let brand = crate::iure_badge::fill(dark);
    let badge = (unread > 0).then(|| unread.min(999).to_string());
    let bw = badge.as_ref().map_or(0.0, |b| 10.0 + 7.0 * b.len() as f32);
    let w = 12.0 + 15.0 + if badge.is_some() { 6.0 + bw - 4.0 } else { 0.0 } + 12.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, d.control_height() + 6.0), egui::Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 4.0, c.hover);
    }
    let mr =
        egui::Rect::from_center_size(egui::pos2(rect.left() + 12.0 + 7.0, rect.center().y), egui::vec2(14.0, 15.0));
    let ink = if on || resp.hovered() { crate::iure_badge::mark_color(dark) } else { c.text_muted };
    crate::iure_badge::paint_mark(ui.painter(), mr, ink);
    if let Some(b) = &badge {
        let br =
            egui::Rect::from_min_size(egui::pos2(mr.right() + 6.0, rect.center().y - 8.0), egui::vec2(bw - 4.0, 16.0));
        ui.painter().rect_filled(br, 8.0, brand);
        ui.painter().text(
            br.center(),
            egui::Align2::CENTER_CENTER,
            b,
            theme::small_font(ui.ctx()),
            egui::Color32::WHITE,
        );
    }
    if on {
        let bar = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 4.0, rect.bottom() - 3.0),
            egui::pos2(rect.right() - 4.0, rect.bottom()),
        );
        ui.painter().rect_filled(bar, 1.5, brand);
    }
    let name = match &badge {
        Some(b) => format!("Iurefficient, {b} {}", pick("unread", "sin leer")),
        None => "Iurefficient".to_string(),
    };
    iurefficient_ui::a11y::toggle(&resp, &name, on);
    let resp = resp.on_hover_text(pick(
        "Iurefficient — messages related to your CRM (they also stay in their category)",
        "Iurefficient — correos con relación en tu CRM (siguen también en su categoría)",
    ));
    resp.clicked()
}

/// Botón «Solo Iurefficient» de la cabecera de la lista (solo con Iurefficient conectado).
pub(crate) fn iure_filter_button(app: &mut DeskApp, ui: &mut Ui) {
    if !app.session.iure_panel_visible() {
        return;
    }
    let on = app.session.iure_only();
    let c = theme::colors(ui);
    let r = icons::icon_button(
        ui,
        Icon::Sparkle,
        if on {
            pick("Show all messages", "Ver todos los mensajes")
        } else {
            pick("Only Iurefficient", "Solo Iurefficient")
        },
        true,
    );
    if on {
        ui.painter().rect_stroke(r.rect.shrink(1.0), 5.0, egui::Stroke::new(1.5, c.brand), egui::StrokeKind::Inside);
    }
    if r.clicked() {
        app.session.set_iure_only(!on);
    }
}

/// Chips de etiquetas: con texto (o puntos de color si `dots`). Devuelve el ancho usado.
pub(crate) fn paint_label_chips(
    p: &egui::Painter,
    ctx: &Context,
    origin: egui::Pos2,
    max_w: f32,
    labels: &[(String, u32)],
    dots: bool,
) -> f32 {
    let mut x = origin.x;
    let font = egui::FontId::proportional(theme::density(ctx).small_size() - 1.0);
    for (name, color) in labels.iter().take(4) {
        let col = crate::widgets::rgb(*color);
        if dots {
            if x + 9.0 > origin.x + max_w {
                break;
            }
            p.circle_filled(egui::pos2(x + 4.0, origin.y), 3.5, col);
            x += 9.0;
            continue;
        }
        let short: String = name.chars().take(16).collect();
        let g = p.layout_no_wrap(short, font.clone(), col);
        let w = g.size().x + 10.0;
        if x + w > origin.x + max_w {
            break;
        }
        let r = egui::Rect::from_min_size(egui::pos2(x, origin.y - 8.0), egui::vec2(w, 16.0));
        p.rect_filled(r, 8.0, col.gamma_multiply(0.16));
        p.rect_stroke(r, 8.0, egui::Stroke::new(1.0, col.gamma_multiply(0.55)), egui::StrokeKind::Inside);
        p.galley(egui::pos2(r.left() + 5.0, r.center().y - g.size().y / 2.0), g, col);
        x += w + 4.0;
    }
    x - origin.x
}

/// Chips de etiquetas e insignia en la cabecera del mensaje (panel de lectura).
pub(crate) fn reading_chips(app: &mut DeskApp, ui: &mut Ui, env: &iuredesk_model::MessageEnvelope) {
    let labels = app.session.message_labels(env);
    let badge = app.session.iure_badge(env.id);
    if labels.is_empty() && badge.is_none() {
        return;
    }
    let c = theme::colors(ui);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if let Some(b) = &badge {
            let dark = ui.visuals().dark_mode;
            let (r, resp) = ui.allocate_exact_size(crate::iure_badge::capsule_size(16.0), egui::Sense::hover());
            crate::iure_badge::paint_capsule(ui.painter(), r, b.kind, dark);
            ui.label(RichText::new(b.tip.get()).color(c.brand).size(theme::density(ui.ctx()).small_size()));
            iurefficient_ui::a11y::label(&resp, b.tip.get());
        }
        for (name, color) in &labels {
            let col = crate::widgets::rgb(*color);
            let txt = RichText::new(name).color(col).size(theme::density(ui.ctx()).small_size());
            egui::Frame::new()
                .fill(col.gamma_multiply(0.16))
                .stroke(egui::Stroke::new(1.0, col.gamma_multiply(0.55)))
                .corner_radius(8)
                .inner_margin(egui::Margin::symmetric(6, 1))
                .show(ui, |ui| ui.label(txt));
        }
    });
}

/// Ajustes → Etiquetas: color de cada etiqueta.
pub(crate) fn labels_section(app: &mut DeskApp, ui: &mut Ui) {
    section_label(ui, pick("Labels", "Etiquetas"));
    muted_note(
        ui,
        pick(
            "Gmail labels, Outlook categories and your own labels, with the color shown in the list. In the dense view they appear as small dots.",
            "Etiquetas de Gmail, categorías de Outlook y las tuyas, con el color que se ve en la lista. En la vista densa aparecen como puntos.",
        ),
    );
    ui.add_space(6.0);
    let labels = app.session.known_labels();
    if labels.is_empty() {
        ui.label(RichText::new(pick("No labels yet.", "Aún no hay etiquetas.")).color(theme::colors(ui).text_muted));
    }
    for (name, color) in labels {
        ui.horizontal(|ui| {
            let mut rgb = [((color >> 16) & 0xFF) as u8, ((color >> 8) & 0xFF) as u8, (color & 0xFF) as u8];
            let r = ui.color_edit_button_srgb(&mut rgb);
            named(&r, egui::Role::Button, &format!("{} {name}", pick("Color of", "Color de")));
            if r.changed() {
                let v = (u32::from(rgb[0]) << 16) | (u32::from(rgb[1]) << 8) | u32::from(rgb[2]);
                app.session.set_label_color(&name, v);
            }
            ui.label(&name);
        });
    }
    ui.add_space(10.0);
    section_label(ui, pick("Categories", "Categorías"));
    for cat in Category::ALL {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
            ui.painter().circle_filled(r.center(), 5.0, crate::widgets::rgb(cat.color()));
            ui.label(bi(cat.label()));
        });
    }
}

/// Seguimientos vencidos en el panel «Hoy».
pub(crate) fn today_followups(app: &mut DeskApp, ui: &mut Ui) {
    let due: Vec<_> = app.session.due_followups().iter().take(3).cloned().collect();
    if due.is_empty() {
        return;
    }
    let c = theme::colors(ui);
    ui.add_space(4.0);
    for f in due {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let (r, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
            icons::paint(ui.painter(), r, Icon::Bell, c.warning, c.warning);
            let label = format!("{} {}", pick("No reply:", "Sin respuesta:"), f.subject);
            let w = ui.available_width() - 26.0;
            let g = iurefficient_ui::widgets::text::elided(ui, &label, theme::body_font(ui.ctx()), c.text, w.max(40.0));
            let resp = ui.add(egui::Label::new(egui::WidgetText::Galley(g)).sense(egui::Sense::click()));
            iurefficient_ui::a11y::button(&resp, &label);
            if resp.on_hover_text(&f.to).clicked() {
                app.session.open_followup(f.msg);
            }
            if icons::icon_button(ui, Icon::Close, pick("Dismiss reminder", "Descartar recordatorio"), true).clicked() {
                app.session.dismiss_followup(f.msg);
            }
        });
    }
}

/// Sección de Ajustes.
pub(crate) fn settings_section(app: &mut DeskApp, ui: &mut Ui) {
    section_label(ui, pick("Rules and filters", "Reglas y filtros"));
    let n = app.session.rule_book().rules.len();
    let mut cats = app.session.rule_book().categories.enabled;
    ui.horizontal(|ui| {
        if ui.button(pick("Manage rules…", "Administrar reglas…")).clicked() {
            app.rules.open = true;
            app.rules.tab = Tab::Rules;
            app.session.sieve_detect();
        }
        if ui.button(pick("Automatic reply…", "Respuesta automática…")).clicked() {
            app.rules.open = true;
            app.rules.tab = Tab::Vacation;
        }
        ui.label(RichText::new(format!("{n} {}", pick("rules", "reglas"))).color(theme::colors(ui).text_muted));
    });
    if ui
        .checkbox(
            &mut cats,
            pick(
                "Inbox categories: Primary, Promotions, Social, Updates, Forums",
                "Categorías en la bandeja: Principal, Promociones, Social, Notificaciones, Foros",
            ),
        )
        .changed()
    {
        app.session.set_categories_enabled(cats);
    }
    muted_note(
        ui,
        pick(
            "Gmail and Microsoft 365 accounts use what the provider knows; the rest are sorted on this computer.",
            "Las cuentas de Gmail y Microsoft 365 usan lo que sabe el proveedor; las demás se ordenan en este equipo.",
        ),
    );
    ui.add_space(14.0);
}

// ------------------------------------------------------------------------- ventanas

/// Ventanas de F5.3 (cada cuadro).
pub(crate) fn show(app: &mut DeskApp, ctx: &Context) {
    rules_window(app, ctx);
    snooze_dialog(app, ctx);
    followup_dialog(app, ctx);
    unsubscribe_dialog(app, ctx);
}

fn rules_window(app: &mut DeskApp, ctx: &Context) {
    if !app.rules.open {
        return;
    }
    let mut open = true;
    egui::Window::new(pick("Rules", "Reglas"))
        .id(egui::Id::unique("rules-window"))
        .open(&mut open)
        .collapsible(false)
        .default_width(680.0)
        .default_height(560.0)
        .show(ctx, |ui| {
            let mut tab = app.rules.tab;
            segmented(
                ui,
                &mut tab,
                &[
                    (Tab::Rules, pick("Rules", "Reglas")),
                    (Tab::Vacation, pick("Automatic reply", "Respuesta automática")),
                    (Tab::Server, pick("Server", "Servidor")),
                ],
            );
            app.rules.tab = tab;
            ui.add_space(8.0);
            egui::ScrollArea::vertical().max_height(ctx.content_rect().height() - 160.0).show(ui, |ui| match tab {
                Tab::Rules => {
                    if app.rules.editing.is_some() {
                        editor(app, ui);
                    } else {
                        rule_list(app, ui);
                    }
                }
                Tab::Vacation => vacation_tab(app, ui),
                Tab::Server => server_tab(app, ui),
            });
        });
    if !open {
        app.rules.open = false;
        app.rules.editing = None;
        app.rules.vacation = None;
    }
}

fn rule_list(app: &mut DeskApp, ui: &mut Ui) {
    let c = theme::colors(ui);
    ui.label(
        RichText::new(pick(
            "Rules sort your mail as it arrives, in this order.",
            "Las reglas ordenan tu correo al llegar, en este orden.",
        ))
        .color(c.text_muted),
    );
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if primary_button(ui, pick("New rule", "Nueva regla")).clicked() {
            app.rules.editing = Some((Rule::default(), None));
        }
        if ui.button(pick("Apply to the inbox now", "Aplicar a la bandeja ahora")).clicked() {
            let ctx = ui.ctx().clone();
            app.session.select_folder(iuredesk_engine::FolderSel::default());
            app.run(&ctx, "mail.apply_rules");
        }
    });
    ui.add_space(8.0);
    let rules = app.session.rule_book().rules.clone();
    if rules.is_empty() {
        iurefficient_ui::EmptyState::new(Icon::Filter, pick("No rules yet", "Aún no hay reglas"))
            .body(pick(
                "Create one here, or right-click a message → “Create rule from this message”.",
                "Crea una aquí, o con clic derecho en un correo → «Crear regla a partir de este correo».",
            ))
            .show(ui);
        return;
    }
    let n = rules.len();
    for (i, r) in rules.into_iter().enumerate() {
        let place = app.session.rule_place(&r);
        egui::Frame::new()
            .fill(c.surface_alt)
            .stroke(egui::Stroke::new(1.0, c.separator))
            .corner_radius(8)
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let mut on = r.enabled;
                    if ui.checkbox(&mut on, RichText::new(&r.name).strong()).changed() {
                        app.session.set_rule_enabled(r.id, on);
                    }
                    let (txt, col) = match &place {
                        RulePlace::Server => (pick("On the server", "En el servidor"), c.success),
                        RulePlace::Local(_) => (pick("On this computer", "En este equipo"), c.text_muted),
                    };
                    ui.label(RichText::new(txt).color(col).size(theme::density(ui.ctx()).small_size()));
                });
                ui.add(egui::Label::new(RichText::new(bi(r.summary())).color(c.text)).wrap());
                if let RulePlace::Local(Some(why)) = &place {
                    ui.label(RichText::new(why.get()).color(c.text_muted).size(theme::density(ui.ctx()).small_size()));
                }
                ui.horizontal(|ui| {
                    if ui.button(pick("Edit", "Editar")).clicked() {
                        app.rules.editing = Some((r.clone(), None));
                    }
                    if icons::icon_button(ui, Icon::ChevronUp, pick("Move up", "Subir"), i > 0).clicked() {
                        app.session.move_rule(r.id, -1);
                    }
                    if icons::icon_button(ui, Icon::ChevronDown, pick("Move down", "Bajar"), i + 1 < n).clicked() {
                        app.session.move_rule(r.id, 1);
                    }
                    if app.rules.confirm_delete == Some(r.id) {
                        ui.label(RichText::new(pick("Delete this rule?", "¿Eliminar esta regla?")).color(c.danger));
                        if ui.button(pick("Yes, delete", "Sí, eliminar")).clicked() {
                            app.rules.confirm_delete = None;
                            let out = app.session.delete_rule(r.id);
                            let ctx = ui.ctx().clone();
                            app.outcome(&ctx, out);
                        }
                        if ui.button(pick("Cancel", "Cancelar")).clicked() {
                            app.rules.confirm_delete = None;
                        }
                    } else if ui.button(pick("Delete…", "Eliminar…")).clicked() {
                        app.rules.confirm_delete = Some(r.id);
                    }
                });
            });
        ui.add_space(6.0);
    }
}

// ---------------------------------------------------------------- editor

#[derive(Clone, Copy, PartialEq, Eq)]
enum CondKind {
    From,
    To,
    Recipients,
    Subject,
    Body,
    ListId,
    Domain,
    Attachment,
    Bigger,
    Smaller,
    Reply,
    List,
    Contact,
}

const COND_KINDS: [CondKind; 13] = [
    CondKind::From,
    CondKind::To,
    CondKind::Recipients,
    CondKind::Subject,
    CondKind::Body,
    CondKind::ListId,
    CondKind::Domain,
    CondKind::Attachment,
    CondKind::Bigger,
    CondKind::Smaller,
    CondKind::Reply,
    CondKind::List,
    CondKind::Contact,
];

impl CondKind {
    fn label(self) -> &'static str {
        match self {
            CondKind::From => pick("The sender", "El remitente"),
            CondKind::To => pick("To", "Para"),
            CondKind::Recipients => pick("To or Cc", "Para o CC"),
            CondKind::Subject => pick("The subject", "El asunto"),
            CondKind::Body => pick("The message text", "El texto del mensaje"),
            CondKind::ListId => pick("The mailing list", "La lista de correo"),
            CondKind::Domain => pick("The sender's domain is", "El dominio del remitente es"),
            CondKind::Attachment => pick("It has attachments", "Tiene adjuntos"),
            CondKind::Bigger => pick("It's larger than (KB)", "Pesa más de (KB)"),
            CondKind::Smaller => pick("It's smaller than (KB)", "Pesa menos de (KB)"),
            CondKind::Reply => pick("It's a reply", "Es una respuesta"),
            CondKind::List => pick("It's a newsletter or list", "Es un boletín o lista"),
            CondKind::Contact => pick("The sender is a contact", "El remitente es un contacto"),
        }
    }

    fn of(c: &Condition) -> CondKind {
        match c {
            Condition::Text { field, .. } => match field {
                Field::From => CondKind::From,
                Field::To | Field::Cc => CondKind::To,
                Field::Recipients => CondKind::Recipients,
                Field::Subject => CondKind::Subject,
                Field::Body => CondKind::Body,
                Field::ListId | Field::Header(_) => CondKind::ListId,
            },
            Condition::Domain { .. } => CondKind::Domain,
            Condition::HasAttachment => CondKind::Attachment,
            Condition::SizeOver { .. } => CondKind::Bigger,
            Condition::SizeUnder { .. } => CondKind::Smaller,
            Condition::IsReply => CondKind::Reply,
            Condition::IsList => CondKind::List,
            Condition::KnownContact | Condition::Account { .. } | Condition::InFolder { .. } => CondKind::Contact,
        }
    }

    fn make(self, keep: &str) -> Condition {
        let text = |field| Condition::Text { field, op: TextOp::Contains, value: keep.to_string() };
        match self {
            CondKind::From => text(Field::From),
            CondKind::To => text(Field::To),
            CondKind::Recipients => text(Field::Recipients),
            CondKind::Subject => text(Field::Subject),
            CondKind::Body => text(Field::Body),
            CondKind::ListId => text(Field::ListId),
            CondKind::Domain => Condition::Domain { field: Field::From, domain: keep.to_string() },
            CondKind::Attachment => Condition::HasAttachment,
            CondKind::Bigger => Condition::SizeOver { bytes: 1024 * 1024 },
            CondKind::Smaller => Condition::SizeUnder { bytes: 100 * 1024 },
            CondKind::Reply => Condition::IsReply,
            CondKind::List => Condition::IsList,
            CondKind::Contact => Condition::KnownContact,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActKind {
    Move,
    Label,
    Flag,
    Read,
    Delete,
    Forward,
    Redirect,
    Reply,
    Notify,
    Silence,
    Stop,
}

const ACT_KINDS: [ActKind; 11] = [
    ActKind::Move,
    ActKind::Label,
    ActKind::Flag,
    ActKind::Read,
    ActKind::Delete,
    ActKind::Forward,
    ActKind::Redirect,
    ActKind::Reply,
    ActKind::Notify,
    ActKind::Silence,
    ActKind::Stop,
];

impl ActKind {
    fn label(self) -> &'static str {
        match self {
            ActKind::Move => pick("Move it to", "Moverlo a"),
            ActKind::Label => pick("Label it", "Etiquetarlo"),
            ActKind::Flag => pick("Flag it for follow-up", "Marcarlo para seguimiento"),
            ActKind::Read => pick("Mark it as read", "Marcarlo como leído"),
            ActKind::Delete => pick("Move it to Trash", "Enviarlo a la papelera"),
            ActKind::Forward => pick("Forward it to", "Reenviarlo a"),
            ActKind::Redirect => pick("Redirect it to", "Redirigirlo a"),
            ActKind::Reply => pick("Reply automatically", "Responder automáticamente"),
            ActKind::Notify => pick("Always notify me", "Avisarme siempre"),
            ActKind::Silence => pick("Don't notify me", "No avisarme"),
            ActKind::Stop => pick("Don't apply more rules", "No aplicar más reglas"),
        }
    }

    fn of(a: &Action) -> ActKind {
        match a {
            Action::MoveTo { .. } => ActKind::Move,
            Action::Label { .. } => ActKind::Label,
            Action::Flag => ActKind::Flag,
            Action::MarkRead => ActKind::Read,
            Action::Delete => ActKind::Delete,
            Action::Forward { .. } => ActKind::Forward,
            Action::Redirect { .. } => ActKind::Redirect,
            Action::AutoReply { .. } => ActKind::Reply,
            Action::Notify => ActKind::Notify,
            Action::Silence => ActKind::Silence,
            Action::Stop => ActKind::Stop,
        }
    }

    fn make(self, folders: &[(String, FolderRef)]) -> Action {
        match self {
            ActKind::Move => Action::MoveTo { folder: folders.first().map(|f| f.1.clone()).unwrap_or_default() },
            ActKind::Label => Action::Label { name: String::new() },
            ActKind::Flag => Action::Flag,
            ActKind::Read => Action::MarkRead,
            ActKind::Delete => Action::Delete,
            ActKind::Forward => Action::Forward { to: String::new() },
            ActKind::Redirect => Action::Redirect { to: String::new() },
            ActKind::Reply => Action::AutoReply { subject: String::new(), body: String::new(), days: 7 },
            ActKind::Notify => Action::Notify,
            ActKind::Silence => Action::Silence,
            ActKind::Stop => Action::Stop,
        }
    }
}

fn text_field(ui: &mut Ui, value: &mut String, name: &str, hint: &str, width: f32) {
    let r = ui.add(egui::TextEdit::singleline(value).desired_width(width).hint_text(hint));
    named(&r, egui::Role::TextInput, name);
}

fn editor(app: &mut DeskApp, ui: &mut Ui) {
    let Some((mut rule, mut error)) = app.rules.editing.take() else { return };
    let folders = app.session.rule_folders();
    let accounts: Vec<(AccountId, String)> =
        app.session.account_labels().iter().map(|l| (l.id, l.email.clone())).collect();
    let c = theme::colors(ui);
    let mut close = false;
    let mut save = false;

    ui.horizontal(|ui| {
        let l = ui.label(pick("Name", "Nombre"));
        let r = ui.add(
            egui::TextEdit::singleline(&mut rule.name)
                .desired_width(320.0)
                .hint_text(pick("e.g. Clients", "p. ej. Clientes")),
        );
        r.labelled_by(l.id);
    });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(pick("When a message arrives that matches", "Cuando llegue un correo que cumpla")).strong(),
        );
        let combo = egui::ComboBox::from_id_salt("rule-all")
            .selected_text(if rule.all { pick("all", "todas") } else { pick("any", "cualquiera") })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut rule.all, true, pick("all", "todas"));
                ui.selectable_value(&mut rule.all, false, pick("any", "cualquiera"));
            });
        named(&combo.response, egui::Role::ComboBox, pick("All or any conditions", "Todas o cualquiera"));
        ui.label(RichText::new(pick("of these conditions:", "de estas condiciones:")).strong());
    });
    let mut remove_c = None;
    for (i, cond) in rule.conditions.iter_mut().enumerate() {
        ui.push_id(("cond", i), |ui| {
            ui.horizontal(|ui| {
                let kind = CondKind::of(cond);
                let mut k = kind;
                let combo =
                    egui::ComboBox::from_id_salt("kind").width(190.0).selected_text(kind.label()).show_ui(ui, |ui| {
                        for x in COND_KINDS {
                            ui.selectable_value(&mut k, x, x.label());
                        }
                    });
                named(&combo.response, egui::Role::ComboBox, pick("Condition", "Condición"));
                if k != kind {
                    let keep = match cond {
                        Condition::Text { value, .. } => value.clone(),
                        Condition::Domain { domain, .. } => domain.clone(),
                        _ => String::new(),
                    };
                    *cond = k.make(&keep);
                }
                match cond {
                    Condition::Text { op, value, .. } => {
                        let cur = *op;
                        let combo = egui::ComboBox::from_id_salt("op")
                            .width(150.0)
                            .selected_text(bi(cur.label()))
                            .show_ui(ui, |ui| {
                                for o in TextOp::ALL {
                                    ui.selectable_value(op, o, bi(o.label()));
                                }
                            });
                        named(&combo.response, egui::Role::ComboBox, pick("Comparison", "Comparación"));
                        text_field(
                            ui,
                            value,
                            pick("Text to look for", "Texto que se busca"),
                            pick("text", "texto"),
                            200.0,
                        );
                    }
                    Condition::Domain { domain, .. } => {
                        text_field(ui, domain, pick("Domain", "Dominio"), "example.com", 220.0);
                    }
                    Condition::SizeOver { bytes } | Condition::SizeUnder { bytes } => {
                        let mut kb = *bytes / 1024;
                        let r = ui.add(egui::DragValue::new(&mut kb).range(1..=1_000_000).suffix(" KB"));
                        named(&r, egui::Role::SpinButton, pick("Size in KB", "Tamaño en KB"));
                        *bytes = kb.saturating_mul(1024);
                    }
                    _ => {}
                }
                if icons::icon_button(ui, Icon::Close, pick("Remove condition", "Quitar condición"), true).clicked() {
                    remove_c = Some(i);
                }
            });
        });
    }
    if let Some(i) = remove_c {
        rule.conditions.remove(i);
    }
    if rule.conditions.is_empty() {
        ui.label(RichText::new(pick("(any message)", "(cualquier correo)")).color(c.text_muted));
    }
    if ui.button(pick("+ Add condition", "+ Añadir condición")).clicked() {
        rule.conditions.push(CondKind::From.make(""));
    }
    ui.add_space(10.0);
    ui.label(RichText::new(pick("Then:", "Entonces:")).strong());
    let mut remove_a = None;
    for (i, act) in rule.actions.iter_mut().enumerate() {
        ui.push_id(("act", i), |ui| {
            ui.horizontal(|ui| {
                let kind = ActKind::of(act);
                let mut k = kind;
                let combo =
                    egui::ComboBox::from_id_salt("kind").width(220.0).selected_text(kind.label()).show_ui(ui, |ui| {
                        for x in ACT_KINDS {
                            ui.selectable_value(&mut k, x, x.label());
                        }
                    });
                named(&combo.response, egui::Role::ComboBox, pick("Action", "Acción"));
                if k != kind {
                    *act = k.make(&folders);
                }
                match act {
                    Action::MoveTo { folder } => {
                        let current = folders
                            .iter()
                            .find(|(_, f)| f.id == folder.id)
                            .map(|(e, f)| format!("{} · {e}", f.name))
                            .unwrap_or_else(|| folder.name.clone());
                        let combo = egui::ComboBox::from_id_salt("folder").width(260.0).selected_text(current).show_ui(
                            ui,
                            |ui| {
                                for (email, f) in &folders {
                                    if ui.selectable_label(f.id == folder.id, format!("{} · {email}", f.name)).clicked()
                                    {
                                        *folder = f.clone();
                                    }
                                }
                            },
                        );
                        named(&combo.response, egui::Role::ComboBox, pick("Folder", "Carpeta"));
                    }
                    Action::Label { name } => {
                        text_field(ui, name, pick("Label", "Etiqueta"), pick("label", "etiqueta"), 200.0)
                    }
                    Action::Forward { to } | Action::Redirect { to } => {
                        text_field(ui, to, pick("Address", "Dirección"), "nombre@example.com", 240.0)
                    }
                    Action::AutoReply { days, .. } => {
                        ui.label(pick("once every", "una vez cada"));
                        let r = ui.add(egui::DragValue::new(days).range(1..=90).suffix(pick(" days", " días")));
                        named(&r, egui::Role::SpinButton, pick("Days between replies", "Días entre respuestas"));
                    }
                    _ => {}
                }
                if icons::icon_button(ui, Icon::Close, pick("Remove action", "Quitar acción"), true).clicked() {
                    remove_a = Some(i);
                }
            });
            if let Action::AutoReply { subject, body, .. } = act {
                ui.indent("reply", |ui| {
                    text_field(
                        ui,
                        subject,
                        pick("Reply subject", "Asunto de la respuesta"),
                        pick("Subject (optional)", "Asunto (opcional)"),
                        360.0,
                    );
                    let r = ui.add(
                        egui::TextEdit::multiline(body)
                            .desired_rows(3)
                            .desired_width(f32::INFINITY)
                            .hint_text(pick("Thanks for writing…", "Gracias por escribir…")),
                    );
                    named(&r, egui::Role::MultilineTextInput, pick("Reply text", "Texto de la respuesta"));
                });
            }
        });
    }
    if let Some(i) = remove_a {
        rule.actions.remove(i);
    }
    if ui.button(pick("+ Add action", "+ Añadir acción")).clicked() {
        rule.actions.push(ActKind::Move.make(&folders));
    }
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.label(pick("For", "Para"));
        let current = rule
            .account
            .and_then(|a| accounts.iter().find(|(id, _)| *id == a).map(|(_, e)| e.clone()))
            .unwrap_or_else(|| pick("all accounts", "todas las cuentas").to_string());
        let combo = egui::ComboBox::from_id_salt("rule-account").selected_text(current).show_ui(ui, |ui| {
            ui.selectable_value(&mut rule.account, None, pick("all accounts", "todas las cuentas"));
            for (id, e) in &accounts {
                ui.selectable_value(&mut rule.account, Some(*id), e);
            }
        });
        named(&combo.response, egui::Role::ComboBox, pick("Account", "Cuenta"));
    });
    ui.checkbox(
        &mut rule.prefer_server,
        pick(
            "Run it on the server when possible (works with IureDesk closed)",
            "Ejecutarla en el servidor cuando se pueda (funciona con IureDesk cerrado)",
        ),
    );
    if rule.sends_mail() {
        ui.label(
            RichText::new(pick(
                "Never sent to lists, automatic notices or yourself; at most once per sender as configured.",
                "Nunca se envía a listas, avisos automáticos ni a ti; como mucho una vez por remitente según lo elegido.",
            ))
            .color(c.text_muted),
        );
    }
    ui.add_space(6.0);
    ui.add(egui::Label::new(RichText::new(bi(rule.summary())).italics().color(c.text_muted)).wrap());
    if let Some(e) = &error {
        ui.label(RichText::new(e).color(c.danger));
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if primary_button(ui, pick("Save rule", "Guardar regla")).clicked() {
            save = true;
        }
        if ui.button(pick("Cancel", "Cancelar")).clicked() {
            close = true;
        }
    });
    if save {
        match rule.validate() {
            Err(b) => error = Some(bi(b)),
            Ok(()) => match app.session.save_rule(rule.clone()) {
                Ok(_) => {
                    close = true;
                    app.notice(Notice::success("Rule saved", "Regla guardada"));
                }
                Err(e) => error = Some(e.text().get().to_string()),
            },
        }
    }
    if !close {
        app.rules.editing = Some((rule, error));
    }
}

// ---------------------------------------------------------------- respuesta automática y servidor

fn parse_date(s: &str) -> Result<Option<NaiveDate>, ()> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d").map(Some).map_err(|_| ())
}

fn vacation_tab(app: &mut DeskApp, ui: &mut Ui) {
    let c = theme::colors(ui);
    if app.rules.vacation.is_none() {
        let v = app.session.vacation();
        let fmt = |d: Option<NaiveDate>| d.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default();
        app.rules.vacation = Some(VacationForm { start: fmt(v.start), end: fmt(v.end), v, error: None });
    }
    let on_server = app.session.vacation_on_server();
    let Some(f) = app.rules.vacation.as_mut() else { return };
    ui.checkbox(
        &mut f.v.enabled,
        pick("Reply automatically while I'm away", "Responder automáticamente mientras no estoy"),
    );
    ui.horizontal(|ui| {
        ui.label(pick("From", "Desde"));
        text_field(ui, &mut f.start, pick("Start date", "Fecha de inicio"), pick("YYYY-MM-DD", "AAAA-MM-DD"), 110.0);
        ui.label(pick("until", "hasta"));
        text_field(ui, &mut f.end, pick("End date", "Fecha de fin"), pick("YYYY-MM-DD", "AAAA-MM-DD"), 110.0);
    });
    text_field(ui, &mut f.v.subject, pick("Subject", "Asunto"), pick("Out of office", "Fuera de la oficina"), 420.0);
    let r = ui.add(egui::TextEdit::multiline(&mut f.v.body).desired_rows(5).desired_width(f32::INFINITY).hint_text(
        pick("I'm away until… For urgent matters write to…", "Estoy fuera hasta el… Para urgencias escribe a…"),
    ));
    named(&r, egui::Role::MultilineTextInput, pick("Message", "Mensaje"));
    ui.horizontal(|ui| {
        ui.label(pick("Reply to the same person once every", "Responder a la misma persona una vez cada"));
        let r = ui.add(egui::DragValue::new(&mut f.v.days).range(1..=90).suffix(pick(" days", " días")));
        named(&r, egui::Role::SpinButton, pick("Days between replies", "Días entre respuestas"));
    });
    ui.checkbox(&mut f.v.contacts_only, pick("Only to my contacts", "Solo a mis contactos"));
    ui.add_space(6.0);
    ui.label(
        RichText::new(if on_server {
            pick(
                "Your server sends these replies, even with IureDesk closed.",
                "Tu servidor envía estas respuestas, aunque IureDesk esté cerrado.",
            )
        } else {
            pick(
                "Your server can't send them: IureDesk replies only while it's open on this computer.",
                "Tu servidor no puede enviarlas: IureDesk responde solo mientras esté abierto en este equipo.",
            )
        })
        .color(if on_server { c.success } else { c.warning }),
    );
    if let Some(e) = &f.error {
        ui.label(RichText::new(e).color(c.danger));
    }
    let mut save = false;
    if primary_button(ui, pick("Save", "Guardar")).clicked() {
        save = true;
    }
    if save {
        match (parse_date(&f.start), parse_date(&f.end)) {
            (Ok(s), Ok(e)) => {
                f.v.start = s;
                f.v.end = e;
                if f.v.enabled && f.v.body.trim().is_empty() {
                    f.error = Some(pick("Write the message.", "Escribe el mensaje.").into());
                    return;
                }
                f.error = None;
                let v = f.v.clone();
                let out = app.session.set_vacation(v);
                let ctx = ui.ctx().clone();
                app.outcome(&ctx, out);
            }
            _ => f.error = Some(pick("Write dates like 2026-12-20.", "Escribe las fechas como 2026-12-20.").into()),
        }
    }
}

fn server_tab(app: &mut DeskApp, ui: &mut Ui) {
    let c = theme::colors(ui);
    ui.label(
        RichText::new(pick(
            "If your mail server supports filters (Sieve), your rules run there too — even with IureDesk closed. Rules the server can't run stay on this computer.",
            "Si tu servidor de correo admite filtros (Sieve), tus reglas también corren allí, aunque IureDesk esté cerrado. Las que el servidor no sabe ejecutar se quedan en este equipo.",
        ))
        .color(c.text_muted),
    );
    ui.add_space(8.0);
    let rows = app.session.sieve_rows();
    if rows.is_empty() {
        ui.label(pick("No accounts yet.", "Aún no hay cuentas."));
    }
    for r in rows {
        egui::Frame::new()
            .fill(c.surface_alt)
            .corner_radius(8)
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(&r.email).strong());
                if r.busy {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(12.0));
                        ui.label(pick("Checking the server…", "Consultando el servidor…"));
                    });
                } else if r.state.available {
                    let mut on = r.state.enabled;
                    if ui
                        .checkbox(&mut on, pick("Use my server's filters", "Usar los filtros de mi servidor"))
                        .changed()
                    {
                        app.session.sieve_set_enabled(r.account, on);
                    }
                    let detail = match r.state.synced_at {
                        Some(t) => format!(
                            "{} {} · {} {}",
                            r.state.server_rules.len(),
                            pick("rules on the server", "reglas en el servidor"),
                            pick("updated", "actualizado"),
                            app.session.snooze_when(t)
                        ),
                        None => pick("Not uploaded yet.", "Aún no se han subido.").to_string(),
                    };
                    ui.label(RichText::new(detail).color(c.text_muted));
                } else {
                    ui.label(
                        RichText::new(pick(
                            "This server has no filters: rules run on this computer while IureDesk is open.",
                            "Este servidor no tiene filtros: las reglas corren en este equipo mientras IureDesk está abierto.",
                        ))
                        .color(c.text_muted),
                    );
                }
                if let Some(e) = &r.state.last_error {
                    ui.label(RichText::new(e).color(c.warning));
                }
                if !r.busy && ui.button(pick("Check again", "Comprobar de nuevo")).clicked() {
                    app.session.sieve_set_enabled(r.account, r.state.enabled);
                }
            });
        ui.add_space(6.0);
    }
}

// ---------------------------------------------------------------- diálogos pequeños

fn snooze_dialog(app: &mut DeskApp, ctx: &Context) {
    let Some(mut d) = app.rules.snooze.take() else { return };
    let menu = app.session.snooze_menu();
    let mut open = true;
    let mut fire: Option<String> = None;
    let mut done = false;
    egui::Window::new(pick("Snooze until…", "Posponer hasta…"))
        .id(egui::Id::unique("snooze-dialog"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(340.0)
        .show(ctx, |ui| {
            let c = theme::colors(ui);
            for (key, label, when) in &menu {
                let r = ui.add_sized(
                    [ui.available_width(), 30.0],
                    egui::Button::new(format!("{}   ·   {when}", label.get())),
                );
                if r.clicked() {
                    fire = Some(format!("mail.snooze:{key}"));
                }
            }
            ui.add_space(6.0);
            ui.label(RichText::new(pick("Or pick a date and time", "O elige fecha y hora")).color(c.text_muted));
            ui.horizontal(|ui| {
                text_field(ui, &mut d.date, pick("Date", "Fecha"), pick("YYYY-MM-DD", "AAAA-MM-DD"), 110.0);
                text_field(ui, &mut d.time, pick("Time", "Hora"), "HH:MM", 60.0);
                if ui.button(pick("Snooze", "Posponer")).clicked() {
                    match iuredesk_engine::compose::parse_schedule(
                        &d.date,
                        &d.time,
                        app.session.now(),
                        app.session.zone(),
                    ) {
                        Some(t) => fire = Some(format!("mail.snooze:{}", t.to_rfc3339())),
                        None => {
                            d.error = Some(
                                pick("Write a future date and time.", "Escribe una fecha y hora futuras.").to_string(),
                            )
                        }
                    }
                }
            });
            ui.label(
                RichText::new(pick(
                    "It leaves your inbox and comes back unread at that time.",
                    "Sale de tu bandeja y vuelve sin leer a esa hora.",
                ))
                .color(c.text_muted)
                .size(theme::density(ui.ctx()).small_size()),
            );
            if let Some(e) = &d.error {
                ui.label(RichText::new(e).color(c.danger));
            }
        });
    if let Some(id) = fire {
        app.run(ctx, &id);
        done = true;
    }
    if open && !done {
        app.rules.snooze = Some(d);
    }
}

fn followup_dialog(app: &mut DeskApp, ctx: &Context) {
    if !app.rules.followup {
        return;
    }
    let mut open = true;
    let mut fire = None;
    egui::Window::new(pick("Remind me if nobody replies", "Recordarme si no responden"))
        .id(egui::Id::unique("followup-dialog"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.label(pick(
                "If nobody replies within this time, it will show up in Today.",
                "Si nadie responde en este plazo, aparecerá en Hoy.",
            ));
            ui.horizontal(|ui| {
                for (d, en, es) in [
                    (1u32, "1 day", "1 día"),
                    (2, "2 days", "2 días"),
                    (3, "3 days", "3 días"),
                    (7, "1 week", "1 semana"),
                    (14, "2 weeks", "2 semanas"),
                ] {
                    if ui.button(pick(en, es)).clicked() {
                        fire = Some(d);
                    }
                }
            });
        });
    if let Some(d) = fire {
        app.rules.followup = false;
        app.run(ctx, &format!("mail.followup:{d}"));
    } else if !open {
        app.rules.followup = false;
    }
}

fn unsubscribe_dialog(app: &mut DeskApp, ctx: &Context) {
    let Some((id, why)) = app.rules.unsubscribe.clone() else { return };
    let mut open = true;
    let mut go = false;
    let mut cancel = false;
    egui::Window::new(pick("Unsubscribe?", "¿Cancelar suscripción?"))
        .id(egui::Id::unique("unsubscribe-dialog"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(380.0)
        .show(ctx, |ui| {
            ui.add(egui::Label::new(why.as_str()).wrap());
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if primary_button(ui, pick("Unsubscribe", "Cancelar suscripción")).clicked() {
                    go = true;
                }
                if ui.button(pick("Not now", "Ahora no")).clicked() {
                    cancel = true;
                }
            });
        });
    if go {
        app.rules.unsubscribe = None;
        match app.session.unsubscribe(id) {
            Ok(out) => app.outcome(ctx, out),
            Err(e) => app.notice(Notice::error(e.text())),
        }
    } else if cancel || !open {
        app.rules.unsubscribe = None;
    }
}

// ---------------------------------------------------------------- plantillas en el redactor

/// Plantillas de una ventana de redacción.
#[derive(Default)]
pub(crate) struct ComposeTemplates {
    /// Nombres (los refresca [`template_pump`]).
    names: Vec<String>,
    pending: Option<TemplateAct>,
    naming: Option<String>,
}

enum TemplateAct {
    Insert(String),
    Save(String),
    Delete(String),
}

/// Botón «Plantillas» de la barra del redactor.
pub(crate) fn template_button(ui: &mut Ui, t: &mut ComposeTemplates) {
    let r = icons::icon_button(ui, Icon::Notes, pick("Templates", "Plantillas"), true);
    egui::Popup::menu(&r).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        if t.names.is_empty() {
            ui.label(
                RichText::new(pick("No templates yet", "Aún no hay plantillas")).color(theme::colors(ui).text_muted),
            );
        }
        for n in t.names.clone() {
            ui.horizontal(|ui| {
                if icons::menu_item(ui, Some(Icon::Notes), &n).clicked() {
                    t.pending = Some(TemplateAct::Insert(n.clone()));
                    ui.close();
                }
                if icons::icon_button(ui, Icon::Trash, pick("Delete template", "Eliminar plantilla"), true).clicked() {
                    t.pending = Some(TemplateAct::Delete(n.clone()));
                }
            });
        }
        ui.separator();
        match t.naming.as_mut() {
            None => {
                if icons::menu_item(ui, Some(Icon::Save), pick("Save as template…", "Guardar como plantilla…"))
                    .clicked()
                {
                    t.naming = Some(String::new());
                }
            }
            Some(name) => {
                ui.horizontal(|ui| {
                    text_field(
                        ui,
                        name,
                        pick("Template name", "Nombre de la plantilla"),
                        pick("Name", "Nombre"),
                        160.0,
                    );
                    if ui.button(pick("Save", "Guardar")).clicked() && !name.trim().is_empty() {
                        t.pending = Some(TemplateAct::Save(name.trim().to_string()));
                    }
                });
            }
        }
    });
}

/// Atiende lo pedido en los menús de plantillas (necesita la sesión).
pub(crate) fn template_pump(app: &mut DeskApp) {
    let names = app.session.templates();
    let mut notices = Vec::new();
    for w in &mut app.composers {
        w.templates.names.clone_from(&names);
        match w.templates.pending.take() {
            Some(TemplateAct::Insert(n)) => {
                w.commit();
                match app.session.apply_template(&mut w.cd, &n) {
                    Ok(()) => {
                        w.editor.set_doc(crate::rich_edit::RichDoc::from_rich(&w.cd.draft.body));
                        w.touch();
                    }
                    Err(e) => notices.push(Notice::error(e.text())),
                }
            }
            Some(TemplateAct::Save(n)) => {
                w.commit();
                w.templates.naming = None;
                match app.session.save_template(&n, &w.cd) {
                    Ok(iuredesk_engine::Outcome::Notice(x)) => notices.push(x),
                    Ok(_) => {}
                    Err(e) => notices.push(Notice::error(e.text())),
                }
            }
            Some(TemplateAct::Delete(n)) => {
                if let Err(e) = app.session.delete_template(&n) {
                    notices.push(Notice::error(e.text()));
                }
            }
            None => {}
        }
    }
    for n in notices {
        app.notice(n);
    }
}

// ---------------------------------------------------------------- capturas

impl DeskApp {
    /// Reglas de ejemplo y la ventana abierta (capturas y pruebas): `list`, `editor`,
    /// `vacation`, `server`.
    pub fn demo_rules(&mut self, what: &str) {
        let folders = self.session.rule_folders();
        let find = |n: &str| folders.iter().find(|(_, f)| f.name.eq_ignore_ascii_case(n)).map(|(_, f)| f.clone());
        let any = folders.first().map(|(_, f)| f.clone()).unwrap_or_default();
        let t = |field, op, v: &str| Condition::Text { field, op, value: v.into() };
        let samples = vec![
            Rule {
                name: "Clientes".into(),
                conditions: vec![Condition::Domain { field: Field::From, domain: "cliente.example".into() }],
                actions: vec![Action::Flag, Action::MoveTo { folder: find("Clientes").unwrap_or(any.clone()) }],
                ..Rule::default()
            },
            Rule {
                name: "Boletines".into(),
                conditions: vec![Condition::IsList],
                actions: vec![
                    Action::MarkRead,
                    Action::MoveTo { folder: find("Archive").or(find("Archivo")).unwrap_or(any.clone()) },
                ],
                ..Rule::default()
            },
            Rule {
                name: "Facturas con adjunto".into(),
                conditions: vec![t(Field::Subject, TextOp::Contains, "factura"), Condition::HasAttachment],
                actions: vec![
                    Action::Label { name: "Facturas".into() },
                    Action::Forward { to: "contabilidad@despacho.example.org".into() },
                ],
                ..Rule::default()
            },
        ];
        if self.session.rule_book().rules.is_empty() {
            for r in samples {
                let _ = self.session.save_rule(r);
            }
        }
        self.rules.open = true;
        self.rules.tab = match what {
            "vacation" => Tab::Vacation,
            "server" => Tab::Server,
            _ => Tab::Rules,
        };
        if what == "editor" {
            self.rules.editing = self.session.rule_book().rules.first().cloned().map(|r| (r, None));
        }
        if what == "vacation" {
            let v = Vacation {
                enabled: true,
                start: NaiveDate::from_ymd_opt(2026, 12, 21),
                end: NaiveDate::from_ymd_opt(2027, 1, 4),
                subject: "Fuera de la oficina".into(),
                body: "Hola:\n\nEstoy de vacaciones hasta el 4 de enero. Para asuntos urgentes escribe a pablo@despacho.example.org.\n\nLaura".into(),
                days: 7,
                contacts_only: false,
            };
            let _ = self.session.set_vacation(v);
        }
    }

    /// Etiquetas de ejemplo en los primeros mensajes de la bandeja (capturas).
    pub fn demo_labels(&mut self) {
        let names = ["Clientes", "Facturas", "Urgente", "Personal", "Proyecto Norte"];
        let rows: Vec<MessageId> = self.session.rows().iter().take(40).map(|r| r.id).collect();
        for (i, id) in rows.into_iter().enumerate() {
            if i % 3 == 2 {
                continue;
            }
            let _ = self.session.add_label(id, names[i % names.len()]);
            if i % 4 == 0 {
                let _ = self.session.add_label(id, names[(i + 2) % names.len()]);
            }
        }
        self.session.rebuild_rows();
    }

    /// Diálogo «Posponer hasta…» abierto (capturas).
    pub fn demo_snooze(&mut self) {
        let ctx = Context::default();
        command(self, &ctx, "snooze");
    }

    /// Hay una regla con este nombre (pruebas).
    pub fn has_rule(&mut self, name: &str) -> bool {
        self.session.rule_book().rules.iter().any(|r| r.name == name)
    }

    /// El editor de reglas está abierto (pruebas).
    pub fn rule_editor_open(&self) -> bool {
        self.rules.editing.is_some()
    }
}
