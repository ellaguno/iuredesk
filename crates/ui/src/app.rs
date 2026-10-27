//! La aplicación: arranque del estilo, riel de módulos, atajos por esquema, paleta de comandos,
//! avisos con «Deshacer» y las ventanas (redactor, ajustes, alta de cuenta).

use std::sync::Arc;

use egui::{Context, Key, Modifiers, Ui};
use iuredesk_engine::command::{self, Chord};
use iuredesk_engine::{Event, Module, Notice, NoticeKind, Outcome, Session, SyncStatus};
use iurefficient_ui::palette::{CommandPalette, PaletteItem};
use iurefficient_ui::widgets::nav_rail::rail_width;
use iurefficient_ui::{NavItem, PaneKind, Severity, Toasts, lang, nav_rail, pick, split, system, theme};

use crate::calendar::CalendarState;
use crate::compose::ComposeWindow;
use crate::html::HtmlCache;
use crate::mail::MailState;
use crate::pim::PimState;
use crate::widgets::{kit_density, kit_theme};

/// Notificación del escritorio: (título, texto). La pone la app (en Linux, D-Bus con zbus).
pub type Notifier = Arc<dyn Fn(&str, &str) + Send + Sync>;

/// Opciones de arranque.
#[derive(Clone, Default)]
pub struct Options {
    /// Usar las fuentes del sistema (las capturas sin pantalla también las usan si existen).
    pub system_fonts: bool,
    /// No seguir el tema del escritorio (capturas y pruebas).
    pub no_theme_watch: bool,
    /// Enlace `mailto:` con el que se abrió la aplicación.
    pub mailto: Option<String>,
    /// Notificaciones del escritorio para el correo nuevo (`None` = solo avisos en la ventana).
    pub notifier: Option<Notifier>,
    /// Bandera que el puente MCP marca cuando escribe en el almacén (recargar la vista).
    pub external_change: Option<Arc<std::sync::atomic::AtomicBool>>,
}

impl std::fmt::Debug for Options {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options")
            .field("system_fonts", &self.system_fonts)
            .field("no_theme_watch", &self.no_theme_watch)
            .field("mailto", &self.mailto)
            .field("notifier", &self.notifier.is_some())
            .finish()
    }
}

/// Qué hace el botón de un aviso.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToastAction {
    Retry,
}

/// Qué lista la paleta.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PaletteMode {
    Commands,
    MoveTo,
}

/// La aplicación de escritorio.
pub struct DeskApp {
    pub session: Session,
    options: Options,
    started: bool,
    pub(crate) mail: MailState,
    pub(crate) calendar: CalendarState,
    pub(crate) pim: PimState,
    pub(crate) html: HtmlCache,
    pub palette: CommandPalette,
    palette_mode: PaletteMode,
    palette_items: Vec<PaletteItem>,
    toasts: Toasts,
    /// Notificación que ofrece «Deshacer».
    undo_toast: Option<u64>,
    /// Botones de los avisos (id del aviso → acción).
    toast_actions: Vec<(u64, ToastAction)>,
    /// Ventana del estado de la sincronización.
    pub sync_open: bool,
    /// Último error de sincronización avisado (para no repetirlo en cada cuadro).
    last_sync_error: Option<String>,
    pub(crate) composers: Vec<ComposeWindow>,
    next_composer: u64,
    pub settings_open: bool,
    pub(crate) settings_ui: crate::settings::AccountsUi,
    pub(crate) iure: crate::iure::IureUi,
    /// F4.2: asistente ✦, apps hermanas e importación.
    pub(crate) ai: crate::assistant::AiUi,
    pub(crate) suite: crate::suite_ui::SuiteUi,
    pub(crate) import: crate::import_ui::ImportUi,
    applied: Option<(iuredesk_engine::ThemePref, iuredesk_engine::DensityPref)>,
}

impl DeskApp {
    pub fn new(session: Session, options: Options) -> Self {
        let (ai, suite, import) = (
            crate::assistant::AiUi::new(&session),
            crate::suite_ui::SuiteUi::new(&session),
            crate::import_ui::ImportUi::new(&session),
        );
        let mut app = DeskApp {
            session,
            options,
            started: false,
            mail: MailState::default(),
            calendar: CalendarState::default(),
            pim: PimState::default(),
            html: HtmlCache::default(),
            palette: CommandPalette::default(),
            palette_mode: PaletteMode::Commands,
            palette_items: Vec::new(),
            toasts: Toasts::default(),
            undo_toast: None,
            toast_actions: Vec::new(),
            sync_open: false,
            last_sync_error: None,
            composers: Vec::new(),
            next_composer: 0,
            settings_open: false,
            settings_ui: Default::default(),
            iure: Default::default(),
            ai,
            suite,
            import,
            applied: None,
        };
        if let Some(m) = app.options.mailto.clone()
            && let Some(d) = app.session.compose_mailto(&m)
        {
            app.open_composer(d);
        }
        app
    }

    /// Notificaciones del escritorio, si las hay (recordatorios).
    pub(crate) fn notifier(&self) -> Option<Notifier> {
        self.options.notifier.clone()
    }

    /// Descarta los recordatorios que tocan ahora (capturas deterministas).
    pub fn dismiss_reminders(&mut self) {
        for r in self.session.due_reminders() {
            self.session.dismiss_reminder(r.key);
        }
    }

    /// Al cerrar: detiene la sincronización y la bandeja de salida (lo pendiente se conserva).
    pub fn shutdown(&mut self) {
        self.session.shutdown();
    }

    /// Redactor con un mensaje de ejemplo (capturas).
    pub fn demo_compose(&mut self) {
        use iuredesk_engine::iuredesk_compose::{Attachment, RichBody, SignaturePlacement, signature};
        use iuredesk_model::Mailbox;
        let mut cd = self.session.compose_new(None);
        cd.draft.to = vec![Mailbox::new("Carmen Brown", "carmen.brown@example.net")];
        cd.draft.cc = vec![Mailbox::new("Pablo Ruiz", "pablo@despacho.example.org")];
        cd.draft.subject = "Propuesta de honorarios — caso 241/2026".into();
        let mut body = RichBody::from_markdown(
            "Hola Carmen,\n\nTe envío la **propuesta de honorarios** para el caso 241/2026. Los puntos principales:\n\n- Honorarios fijos por etapa\n- Gastos de juzgado *aparte*\n- Revisión mensual del avance\n\nEl detalle está en el [portal del despacho](https://despacho.example.org/casos/241).\n\nSaludos,",
        );
        let sig = RichBody::from_plain("Laura Méndez\nDespacho Méndez · Abogados\n+52 55 5555 0100");
        signature::apply(&mut body, Some(&sig), SignaturePlacement::AboveQuote);
        cd.draft.body = body;
        cd.draft.attachments.push(Attachment::bytes("propuesta_honorarios.pdf", "application/pdf", vec![0; 184_320]));
        self.open_composer(cd);
    }

    /// Ventanas de redacción abiertas (pruebas).
    pub fn composer_count(&self) -> usize {
        self.composers.len()
    }

    /// Asunto de la primera ventana de redacción (pruebas).
    pub fn composer_subject(&self) -> Option<String> {
        self.composers.first().map(|w| w.cd.draft.subject.clone())
    }

    /// Un cuadro completo. `ui` ocupa la ventana.
    pub fn ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.start(&ctx);
        theme::refresh(&ctx);
        self.apply_style(&ctx, false);
        self.poll(&ctx);

        if self.session.needs_onboarding() {
            if self.session.wizard.is_none() {
                self.session.wizard_begin();
            }
            crate::onboarding::page(self, ui);
            crate::import_ui::frame(self, &ctx);
            self.toasts.show(&ctx);
            return;
        }

        let modal = self.palette.is_open()
            || self.session.wizard.is_some()
            || self.settings_open
            || self.iure.dialog.is_some()
            || self.ai.modal()
            || self.suite.modal()
            || self.import.modal();
        // Escribiendo en el redactor, los atajos de la aplicación no se disparan.
        let composing = !self.composers.is_empty() && ctx.egui_wants_keyboard_input();
        if !modal && !composing {
            self.shortcuts(&ctx);
        }
        self.dropped_files(&ctx);
        if self.palette.handle_shortcut(&ctx) {
            self.palette_mode = PaletteMode::Commands;
        }

        self.rail(ui);
        match self.session.module {
            Module::Mail => crate::mail::show(self, ui),
            Module::Calendar => crate::calendar::show(self, ui),
            Module::Contacts => crate::pim::contacts(self, ui),
            Module::Notes => crate::pim::notes(self, ui),
            Module::Tasks => crate::pim::tasks(self, ui),
            Module::Assistant => crate::assistant::page(self, ui),
        }

        crate::event_editor::show(self, &ctx);
        crate::reminders::show(self, &ctx);
        crate::compose::show_all(self, &ctx);
        crate::settings::show(self, &ctx);
        crate::iure::dialogs(self, &ctx);
        crate::iure::bell(self, &ctx);
        crate::assistant::frame(self, &ctx);
        crate::suite_ui::frame(self, &ctx);
        crate::import_ui::frame(self, &ctx);
        self.sync_window(&ctx);
        if self.session.wizard.is_some() {
            crate::onboarding::modal(self, &ctx);
        }
        self.show_palette(&ctx);
        self.show_toasts(&ctx);
        if let Some(d) = self.session.next_deadline() {
            ctx.request_repaint_after(d.max(std::time::Duration::from_millis(16)));
        }
    }

    fn show_toasts(&mut self, ctx: &Context) {
        let Some(id) = self.toasts.show(ctx) else { return };
        if Some(id) == self.undo_toast {
            self.undo_toast = None;
            self.run(ctx, "undo");
            return;
        }
        if let Some(pos) = self.toast_actions.iter().position(|(t, _)| *t == id) {
            let (_, act) = self.toast_actions.remove(pos);
            match act {
                ToastAction::Retry => self.session.sync_now(),
            }
        }
    }

    /// Archivos soltados sobre la ventana: se adjuntan al redactor (o abren uno nuevo).
    fn dropped_files(&mut self, ctx: &Context) {
        let (hovering, dropped) = ctx.input(|i| {
            (
                !i.raw.hovered_files.is_empty(),
                i.raw
                    .dropped_files
                    .iter()
                    .map(|f| f.path().to_path_buf())
                    .filter(|p| !p.as_os_str().is_empty())
                    .collect::<Vec<_>>(),
            )
        });
        if hovering {
            let rect = ctx.content_rect();
            let painter =
                ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("drop-overlay")));
            painter.rect_filled(rect, 0.0, egui::Color32::from_black_alpha(110));
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                pick("Drop to attach", "Suelta para adjuntar"),
                egui::FontId::proportional(22.0),
                egui::Color32::WHITE,
            );
        }
        if !dropped.is_empty() && !self.session.needs_onboarding() {
            crate::compose::on_dropped(self, dropped);
        }
    }

    /// No queda trabajo de fondo ni renders pendientes (capturas y pruebas deterministas).
    pub fn is_settled(&self) -> bool {
        self.session.is_idle()
            && !self.html.pending()
            && self.ai.hub.is_idle()
            && self.import.hub.is_idle()
            && !self.suite.hub.busy()
    }

    /// Asistente en el paso del correo con datos escritos (capturas).
    pub fn demo_email_step(&mut self, email: &str, name: &str) {
        self.session.wizard_begin();
        if let Some(w) = self.session.wizard.as_mut() {
            w.email = email.to_string();
            w.name = name.to_string();
        }
    }

    /// Asistente en la tarjeta del proveedor, sin red (capturas).
    pub fn demo_provider_step(&mut self, email: &str) {
        self.session.wizard_begin();
        if let Some(w) = self.session.wizard.as_mut() {
            w.email = email.to_string();
        }
        if let Some(d) = iuredesk_engine::onboarding::known_provider_discovery(email) {
            self.session.wizard_apply_discovery(d);
        }
    }

    /// Fila del cursor de la lista de correo (pruebas).
    pub fn mail_cursor(&self) -> Option<usize> {
        self.mail.list.selection.cursor()
    }

    /// Calendario en vista de mes.
    pub fn set_calendar_month(&mut self) {
        self.session.set_cal_view(iuredesk_engine::calendar::nav::CalView::Month);
    }

    fn start(&mut self, ctx: &Context) {
        if self.started {
            return;
        }
        self.started = true;
        if !self.options.no_theme_watch {
            system::watch_system_theme(ctx);
        }
        system::install_fonts(ctx, self.options.system_fonts);
        // El idioma: el de las preferencias o el del sistema; el motor y el ui-kit, sincronizados.
        let l = match self.session.prefs.lang.as_deref() {
            Some(tag) => lang::from_tag(tag),
            None => lang::lang(),
        };
        set_language(l);
        let waker_ctx = ctx.clone();
        self.session.set_waker(Arc::new(move || waker_ctx.request_repaint()));
        // Escanear fuentes del render HTML tarda ~1 s: se hace ya, en segundo plano.
        self.session.spawn_blocking(|| {
            iuredesk_html_view::warm_up();
            iuredesk_engine::JobOutput::Custom(Box::new(()))
        });
        self.apply_style(ctx, true);
    }

    /// Aplica tema y densidad si cambiaron en las preferencias.
    pub(crate) fn apply_style(&mut self, ctx: &Context, force: bool) {
        let want = (self.session.prefs.theme, self.session.prefs.density);
        if force || self.applied != Some(want) {
            theme::apply(ctx, kit_theme(want.0), kit_density(want.1));
            self.applied = Some(want);
        }
    }

    fn poll(&mut self, ctx: &Context) {
        // El puente MCP escribió en el almacén (borrador, evento, nota): recargar.
        if let Some(f) = &self.options.external_change
            && f.swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            self.session.reload_external();
        }
        for ev in self.session.poll() {
            match ev {
                Event::Notice(n) => self.notice(n),
                Event::NewMail { summary, detail, account } => {
                    let first = detail.lines().next().unwrap_or("").to_string();
                    let text = if first.is_empty() {
                        summary.get().to_string()
                    } else {
                        format!("{}: {first}", summary.get())
                    };
                    self.toasts.push(Severity::Info, text, None, 6.0);
                    if self.session.prefs.notify_new_mail
                        && let Some(n) = &self.options.notifier
                    {
                        let title = if account.is_empty() {
                            summary.get().to_string()
                        } else {
                            format!("{} — {account}", summary.get())
                        };
                        n(&title, &detail);
                    }
                }
                Event::Compose(d) => self.open_composer(*d),
                Event::Custom { id, value } => {
                    if let Some(f) = value.downcast_ref::<iuredesk_engine::Fetched>() {
                        self.html.on_fetched(f.clone());
                    } else if value.is::<crate::compose::PickedFiles>() {
                        if let Ok(p) = value.downcast::<crate::compose::PickedFiles>() {
                            crate::compose::on_picked(self, *p);
                        }
                    } else if let Ok(done) = value.downcast::<crate::html::HtmlDone>() {
                        self.html.on_done(ctx, id, *done);
                    }
                }
            }
        }
    }

    /// Muestra un aviso (con «Deshacer» si corresponde).
    pub(crate) fn notice(&mut self, n: Notice) {
        let sev = match n.kind {
            NoticeKind::Info => Severity::Info,
            NoticeKind::Success => Severity::Success,
            NoticeKind::Error => Severity::Error,
        };
        if n.undo {
            if let Some(old) = self.undo_toast.take() {
                self.toasts.dismiss(old);
            }
            let secs = n.secs.map_or(10.0, |s| s as f32);
            let id = self.toasts.push(sev, n.text.get(), Some(pick("Undo", "Deshacer")), secs);
            self.undo_toast = Some(id);
        } else {
            self.toasts.push(sev, n.text.get(), None, if n.kind == NoticeKind::Error { 8.0 } else { 4.0 });
        }
    }

    /// Ejecuta un comando y atiende lo que pida.
    pub fn run(&mut self, ctx: &Context, id: &str) {
        match self.session.run(id) {
            Ok(out) => self.outcome(ctx, out),
            Err(e) => self.notice(Notice::error(e.text())),
        }
    }

    pub(crate) fn outcome(&mut self, ctx: &Context, out: Outcome) {
        match out {
            Outcome::Done => {}
            Outcome::Notice(n) => self.notice(n),
            Outcome::Compose(d) => self.open_composer(*d),
            Outcome::PickFolder => self.open_move_palette(),
            Outcome::OpenPalette => {
                self.palette_mode = PaletteMode::Commands;
                self.palette.placeholder = None;
                self.palette.open();
            }
            Outcome::AddAccount => {}
            Outcome::FocusSearch => self.mail.focus_search = true,
            Outcome::Restyle => self.apply_style(ctx, false),
            Outcome::EditEvent(d) => crate::event_editor::open(self, *d),
            Outcome::Assist(id) => crate::assistant::command(self, ctx, &id),
        }
    }

    pub(crate) fn open_composer(&mut self, d: iuredesk_engine::ComposeDraft) {
        self.next_composer += 1;
        self.composers.push(ComposeWindow::new(self.next_composer, d));
    }

    /// Atajos del esquema elegido. Los de una tecla (Gmail) no se disparan mientras se escribe.
    fn shortcuts(&mut self, ctx: &Context) {
        let scheme = self.session.scheme();
        let typing = ctx.egui_wants_keyboard_input();
        let in_calendar = self.session.module == Module::Calendar;
        let mut fire: Option<&'static str> = None;
        ctx.input_mut(|i| {
            let mut consumed = None;
            for (idx, e) in i.events.iter().enumerate() {
                let egui::Event::Key { key, pressed: true, modifiers, .. } = e else { continue };
                for (cmd, chord) in command::bindings(scheme) {
                    if cmd == "app.palette"
                        || (typing && !chord.has_modifier())
                        || (cmd.starts_with("cal.") && !in_calendar)
                    {
                        continue;
                    }
                    if chord_matches(&chord, *key, *modifiers) {
                        fire = Some(cmd);
                        consumed = Some(idx);
                        break;
                    }
                }
                if fire.is_some() {
                    break;
                }
            }
            if let Some(idx) = consumed {
                i.events.remove(idx);
            }
        });
        if let Some(cmd) = fire {
            self.run(ctx, cmd);
        }
    }

    fn rail(&mut self, ui: &mut Ui) {
        let unread = self.session.unified_unread(iuredesk_model::FolderRole::Inbox);
        let due = self.session.due_tasks().len();
        let scheme = self.session.scheme();
        let sc = |id: &str| command::shortcut(id, scheme).map(|c| c.label(cfg!(target_os = "macos")));
        let labels = [
            ("mail", sc("nav.mail")),
            ("calendar", sc("nav.calendar")),
            ("contacts", sc("nav.contacts")),
            ("notes", sc("nav.notes")),
            ("tasks", sc("nav.tasks")),
            ("assistant", sc("nav.assistant")),
        ];
        let short = |k: &str| labels.iter().find(|(id, _)| *id == k).and_then(|(_, s)| s.clone()).unwrap_or_default();
        let (s_mail, s_cal, s_con, s_not, s_tas, s_ai) =
            (short("mail"), short("calendar"), short("contacts"), short("notes"), short("tasks"), short("assistant"));
        let status = self.session.sync_status();
        let (sync_icon, sync_label) = match &status {
            SyncStatus::Off if self.session.is_sample() => (
                iurefficient_ui::Icon::Sync,
                pick("Sample mailbox (offline)", "Buzón de ejemplo (sin conexión)").to_string(),
            ),
            SyncStatus::Off => {
                (iurefficient_ui::Icon::Sync, pick("No accounts to sync", "No hay cuentas que sincronizar").to_string())
            }
            SyncStatus::Idle => {
                (iurefficient_ui::Icon::Sync, pick("Mail is up to date", "El correo está al día").to_string())
            }
            SyncStatus::Busy(t) => (iurefficient_ui::Icon::Sync, t.clone()),
            SyncStatus::Offline(t) => {
                (iurefficient_ui::Icon::Offline, format!("{} — {t}", pick("Offline", "Sin conexión")))
            }
            SyncStatus::Error { text, .. } => (iurefficient_ui::Icon::Alert, text.clone()),
        };
        // Un error nuevo se avisa una vez, con «Reintentar».
        if let SyncStatus::Error { text, .. } = &status {
            if self.last_sync_error.as_deref() != Some(text.as_str()) {
                self.last_sync_error = Some(text.clone());
                let id = self.toasts.push(Severity::Error, text.clone(), Some(pick("Retry", "Reintentar")), 12.0);
                self.toast_actions.push((id, ToastAction::Retry));
            }
        } else {
            self.last_sync_error = None;
        }
        let items = [
            NavItem::new("mail", iurefficient_ui::Icon::Mail, Module::Mail.label())
                .badge(u32::try_from(unread).unwrap_or(u32::MAX))
                .shortcut(&s_mail),
            NavItem::new("calendar", iurefficient_ui::Icon::Calendar, Module::Calendar.label()).shortcut(&s_cal),
            NavItem::new("contacts", iurefficient_ui::Icon::Contacts, Module::Contacts.label()).shortcut(&s_con),
            NavItem::new("notes", iurefficient_ui::Icon::Notes, Module::Notes.label()).shortcut(&s_not),
            NavItem::new("tasks", iurefficient_ui::Icon::Tasks, Module::Tasks.label())
                .badge(u32::try_from(due).unwrap_or(u32::MAX))
                .shortcut(&s_tas),
            NavItem::new("assistant", iurefficient_ui::Icon::Sparkle, Module::Assistant.label()).shortcut(&s_ai),
            NavItem::new("settings", iurefficient_ui::Icon::Settings, pick("Settings", "Ajustes")).bottom(),
            NavItem::new("sync", sync_icon, &sync_label).bottom(),
        ];
        // Campana de Iurefficient solo si hay sesión (o se recuerda una).
        let bell_label = pick("Iurefficient notifications", "Avisos de Iurefficient");
        let bell = NavItem::new("iure-bell", iurefficient_ui::Icon::Bell, bell_label)
            .badge(u32::try_from(self.session.iure_feed().unread).unwrap_or(u32::MAX))
            .bottom();
        let show_bell = matches!(
            self.session.iure_status(),
            iuredesk_engine::iure::IureStatus::Ready { .. } | iuredesk_engine::iure::IureStatus::Offline { .. }
        );
        let mut items = items.to_vec();
        if show_bell {
            items.insert(items.len().saturating_sub(2), bell);
        }
        let w = rail_width(ui.ctx());
        let current = self.session.module.key();
        let mut clicked: Option<String> = None;
        split::left(ui, "rail", PaneKind::Rail, w, w..=w, |ui| {
            let full = ui.max_rect();
            if let Some(id) = nav_rail(ui, &items, current) {
                clicked = Some(id.to_string());
            }
            // Estado sobre el botón de sincronización (el último del pie del riel).
            let side = w - 12.0;
            let center = egui::pos2(full.center().x, full.bottom() - 6.0 - side / 2.0);
            let c = theme::colors(ui);
            match &status {
                SyncStatus::Busy(_) => {
                    let t = ui.input(|i| i.time) as f32;
                    let r = side * 0.36;
                    let a0 = t * 4.0;
                    let pts: Vec<egui::Pos2> = (0..=20)
                        .map(|k| {
                            let a = a0 + (k as f32 / 20.0) * std::f32::consts::PI * 1.4;
                            center + egui::vec2(a.cos(), a.sin()) * r
                        })
                        .collect();
                    ui.painter().add(egui::Shape::line(pts, egui::Stroke::new(2.0, c.accent)));
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
                }
                SyncStatus::Error { .. } => {
                    ui.painter().circle_filled(center + egui::vec2(side * 0.28, -side * 0.28), 4.5, c.danger);
                }
                SyncStatus::Offline(_) => {
                    ui.painter().circle_filled(center + egui::vec2(side * 0.28, -side * 0.28), 4.0, c.warning);
                }
                _ => {}
            }
        });
        match clicked.as_deref() {
            Some("settings") => self.settings_open = true,
            Some("iure-bell") => self.iure.bell_open = !self.iure.bell_open,
            Some("sync") => {
                if self.session.is_live() {
                    self.sync_open = !self.sync_open;
                } else if self.session.is_sample() {
                    self.notice(Notice::info(
                        "This is the sample mailbox: nothing is synced. Add your account to use real mail.",
                        "Este es el buzón de ejemplo: no se sincroniza nada. Agrega tu cuenta para usar tu correo.",
                    ));
                } else {
                    self.run(&ui.ctx().clone(), "app.add_account");
                }
            }
            Some(k) => {
                if let Some(m) = Module::from_key(k) {
                    self.session.module = m;
                }
            }
            None => {}
        }
    }

    /// Ventana con el estado de cada cuenta, «Reintentar» y «Volver a iniciar sesión».
    fn sync_window(&mut self, ctx: &Context) {
        if !self.sync_open {
            return;
        }
        let mut open = true;
        let mut retry = false;
        let mut reauth: Option<String> = None;
        let rows = self.session.accounts_overview();
        egui::Window::new(pick("Sync", "Sincronización"))
            .id(egui::Id::unique("sync-window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .show(ctx, |ui| {
                let c = theme::colors(ui);
                for r in &rows {
                    ui.horizontal(|ui| {
                        let (ink, label) = match &r.status {
                            Some(iuredesk_engine::AccountSync::Error { .. }) => {
                                (c.danger, r.status.as_ref().map(|s| s.label()).unwrap_or_default())
                            }
                            Some(iuredesk_engine::AccountSync::Offline(_)) => {
                                (c.warning, r.status.as_ref().map(|s| s.label()).unwrap_or_default())
                            }
                            Some(s) => (c.success, s.label()),
                            None => (c.text_muted, String::new()),
                        };
                        let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                        ui.painter().circle_filled(dot.center(), 4.0, ink);
                        ui.vertical(|ui| {
                            ui.label(egui::RichText::new(&r.email).strong().color(c.text));
                            if !label.is_empty() {
                                ui.add(egui::Label::new(egui::RichText::new(label).color(c.text_muted)).wrap());
                            }
                        });
                    });
                    if matches!(r.status, Some(iuredesk_engine::AccountSync::Error { reauth: true, .. }))
                        && ui.button(pick("Sign in again…", "Volver a iniciar sesión…")).clicked()
                    {
                        reauth = Some(r.id.clone());
                    }
                    ui.add_space(6.0);
                }
                if iurefficient_ui::primary_button(ui, pick("Retry now", "Reintentar ahora")).clicked() {
                    retry = true;
                }
            });
        if retry {
            self.session.sync_now();
        }
        if let Some(id) = reauth {
            self.sync_open = false;
            self.session.reauth_account(&id);
        }
        if !open {
            self.sync_open = false;
        }
    }

    fn command_items(&self) -> Vec<PaletteItem> {
        let scheme = self.session.scheme();
        let mac = cfg!(target_os = "macos");
        command::COMMANDS
            .iter()
            .map(|c| {
                let other = if iuredesk_engine::lang::spanish() { c.en } else { c.es };
                let mut it = PaletteItem::new(c.id, c.label())
                    .hint(c.group.label())
                    .icon(crate::widgets::icon(c.icon))
                    .keywords(format!("{} {other}", c.keywords))
                    .enabled(self.session.is_enabled(c.id));
                if let Some(k) = command::shortcut(c.id, scheme) {
                    it = it.shortcut(k.label(mac));
                }
                it
            })
            .collect()
    }

    fn open_move_palette(&mut self) {
        let accounts: std::collections::HashSet<iuredesk_model::AccountId> = self
            .session
            .selection()
            .to_vec()
            .into_iter()
            .chain(self.session.focused())
            .filter_map(|id| self.session.envelope(id).map(|e| e.account))
            .collect();
        let mut items = Vec::new();
        for l in self.session.account_labels().to_vec() {
            if !accounts.contains(&l.id) {
                continue;
            }
            for f in self.session.folders_of(l.id) {
                items.push(
                    PaletteItem::new(format!("mail.move:{}", f.id.get()), f.name.clone())
                        .hint(l.email.clone())
                        .icon(crate::mail::folder_icon(f.role)),
                );
            }
        }
        self.palette_items = items;
        self.palette_mode = PaletteMode::MoveTo;
        self.palette.placeholder = Some(pick("Move to folder…", "Mover a la carpeta…").to_string());
        self.palette.open();
    }

    fn show_palette(&mut self, ctx: &Context) {
        if !self.palette.is_open() {
            if self.palette_mode == PaletteMode::MoveTo {
                self.palette_mode = PaletteMode::Commands;
                self.palette.placeholder = None;
            }
            return;
        }
        let items = match self.palette_mode {
            PaletteMode::Commands => self.command_items(),
            PaletteMode::MoveTo => self.palette_items.clone(),
        };
        if let Some(id) = self.palette.show(ctx, &items) {
            self.palette_mode = PaletteMode::Commands;
            self.palette.placeholder = None;
            self.run(ctx, &id);
        }
    }
}

/// Cambia el idioma del ui-kit y del motor a la vez.
pub fn set_language(l: lang::Lang) {
    lang::set_lang(l);
    iuredesk_engine::lang::set_lang(if l == lang::Lang::En {
        iuredesk_engine::lang::Lang::En
    } else {
        iuredesk_engine::lang::Lang::Es
    });
}

/// ¿La tecla y los modificadores son exactamente la combinación?
fn chord_matches(c: &Chord, key: Key, m: Modifiers) -> bool {
    // Los dígitos se nombran «Num1» en el motor; egui los reconoce como «Digit1».
    let name = match c.key.strip_prefix("Num") {
        Some(d) if d.len() == 1 => format!("Digit{d}"),
        _ => c.key.to_string(),
    };
    let Some(want) = Key::from_name(&name) else { return false };
    if want != key {
        return false;
    }
    let ctrl_ok = if cfg!(target_os = "macos") { m.ctrl == c.ctrl } else { true };
    m.command == (c.command || c.ctrl) && m.shift == c.shift && m.alt == c.alt && ctrl_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_match_exactly() {
        assert!(chord_matches(&Chord::cmd("R"), Key::R, Modifiers::COMMAND));
        assert!(!chord_matches(&Chord::cmd("R"), Key::R, Modifiers::COMMAND | Modifiers::SHIFT));
        assert!(chord_matches(&Chord::cmd_shift("R"), Key::R, Modifiers::COMMAND | Modifiers::SHIFT));
        assert!(chord_matches(&Chord::key("E"), Key::E, Modifiers::NONE));
        assert!(!chord_matches(&Chord::key("E"), Key::E, Modifiers::COMMAND));
        assert!(chord_matches(&Chord::cmd("Num1"), Key::Num1, Modifiers::COMMAND));
    }
}
