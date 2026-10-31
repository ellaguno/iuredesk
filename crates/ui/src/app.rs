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

/// Notificación del escritorio: (título, texto, enlace `iuredesk://` que abre el clic). La pone la
/// app (Linux: D-Bus con zbus; Windows: avisos de WinRT; macOS: UserNotifications).
pub type Notifier = Arc<dyn Fn(&str, &str, Option<&str>) + Send + Sync>;

/// R1.2: argumentos que llegan con la app abierta (otra instancia, clic en una notificación).
/// Cada lote es una petición; un lote vacío solo trae la ventana al frente.
pub type Incoming = Arc<std::sync::Mutex<Vec<Vec<String>>>>;

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
    /// Barra de título integrada (sin decoraciones del sistema). `None` = la del sistema: la
    /// barra de la app se dibuja igual, sin botones de ventana ni arrastre.
    pub chrome: Option<crate::header::WindowChrome>,
    /// R1.2: archivos y enlaces con los que se abrió (rutas ya absolutas).
    pub open: Vec<String>,
    /// R1.2: lo que llega después (ver [`Incoming`]).
    pub incoming: Option<Incoming>,
}

impl std::fmt::Debug for Options {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options")
            .field("system_fonts", &self.system_fonts)
            .field("no_theme_watch", &self.no_theme_watch)
            .field("mailto", &self.mailto)
            .field("notifier", &self.notifier.is_some())
            .field("chrome", &self.chrome)
            .field("open", &self.open)
            .finish_non_exhaustive()
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
    /// F6.1: «Mi día».
    pub(crate) today: crate::today::TodayState,
    /// Último lugar recordado (módulo y carpeta) para «Donde lo dejé».
    place: Option<(Module, iuredesk_engine::FolderSel)>,
    /// F5.3: reglas, posponer, seguimiento, baja de listas.
    pub(crate) rules: crate::rules_ui::RulesUi,
    applied: Option<(iuredesk_engine::ThemePref, iuredesk_engine::DensityPref)>,
    /// Tipografía aplicada (clave, tamaño %, nítido).
    applied_fonts: Option<(String, u32, bool)>,
    /// Confirmación pendiente (`Outcome::Confirm`): texto, botón y comando a ejecutar.
    pub(crate) confirm: Option<(iuredesk_engine::lang::Text, iuredesk_engine::lang::Text, String)>,
    /// R2.1: tiempo por cuadro, memoria y estado para Ajustes → Avanzado.
    pub(crate) diag: crate::diagnostics::DiagState,
    /// R1.2: archivos abiertos desde el sistema.
    pub(crate) opened: crate::opened::OpenedUi,
    /// UX-3: adjuntos (abrir con el sistema, guardar, miniaturas).
    pub(crate) attach: crate::attach_ui::AttachUi,
}

impl DeskApp {
    pub fn new(session: Session, options: Options) -> Self {
        let attach = crate::attach_ui::AttachUi::new(&session);
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
            today: Default::default(),
            place: None,
            rules: Default::default(),
            applied: None,
            applied_fonts: None,
            confirm: None,
            diag: Default::default(),
            opened: Default::default(),
            attach,
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

    /// F7.1: guarda la redacción de ejemplo en «Borradores», la cierra y la reabre desde allí
    /// como lo haría el usuario («Editar borrador» / doble clic) (capturas y pruebas).
    pub fn demo_reopen_draft(&mut self) {
        self.demo_compose();
        let Some(mut w) = self.composers.pop() else { return };
        let Ok(id) = self.session.save_draft(&mut w.cd, true) else { return };
        self.session.module = Module::Mail;
        self.session.select_folder(iuredesk_engine::FolderSel::Unified(iuredesk_model::FolderRole::Drafts));
        self.session.set_selection(vec![id], Some(id));
        let ctx = egui::Context::default();
        self.run(&ctx, "mail.edit_draft");
    }

    /// Notas en «Vista previa» (capturas).
    pub fn set_note_preview(&mut self, on: bool) {
        self.pim.note_preview = on;
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
        let frame_start = std::time::Instant::now();
        self.frame(ui);
        self.diag.frames.record(frame_start, frame_start.elapsed());
    }

    /// Tiempo de los últimos cuadros (p50, p99, máx., fps) para diagnóstico y bancos de prueba.
    pub fn frame_summary(&self) -> crate::diagnostics::FrameSummary {
        self.diag.frames.summary()
    }

    fn frame(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.start(&ctx);
        theme::refresh(&ctx);
        self.apply_style(&ctx, false);
        self.poll(&ctx);

        if self.session.needs_onboarding() {
            if self.session.wizard.is_none() {
                self.session.wizard_begin();
            }
            crate::header::show(self, ui, false);
            crate::onboarding::page(self, ui);
            crate::opened::frame(self, &ctx);
            crate::import_ui::frame(self, &ctx);
            self.toasts.show(&ctx);
            crate::header::window_frame(self, ui);
            return;
        }

        let modal = self.palette.is_open()
            || self.session.wizard.is_some()
            || self.confirm.is_some()
            || self.settings_open
            || self.iure.dialog.is_some()
            || self.ai.modal()
            || self.suite.modal()
            || self.import.modal()
            || self.rules.modal()
            || self.attach.modal();
        // Escribiendo en el redactor, los atajos de la aplicación no se disparan.
        let composing = !self.composers.is_empty() && ctx.egui_wants_keyboard_input();
        if !modal && !composing {
            self.shortcuts(&ctx);
        }
        self.dropped_files(&ctx);
        if self.palette.handle_shortcut(&ctx) {
            self.palette_mode = PaletteMode::Commands;
        }

        crate::header::show(self, ui, true);
        self.rail(ui);
        // F7.1: lo escrito en una nota se guarda al salir de Notas.
        if self.session.module != Module::Notes {
            crate::pim::flush_note(self);
        }
        match self.session.module {
            Module::Today => crate::today::show(self, ui),
            Module::Mail => crate::mail::show(self, ui),
            Module::Calendar => crate::calendar::show(self, ui),
            Module::Contacts => crate::pim::contacts(self, ui),
            Module::Notes => crate::pim::notes(self, ui),
            Module::Tasks => crate::pim::tasks(self, ui),
            Module::Assistant => crate::assistant::page(self, ui),
        }

        crate::event_editor::show(self, &ctx);
        crate::task_editor::show(self, &ctx);
        crate::opened::frame(self, &ctx);
        crate::reminders::show(self, &ctx);
        crate::compose::show_all(self, &ctx);
        crate::reader::show_all(self, &ctx);
        crate::attach_ui::frame(self, &ctx);
        crate::settings::show(self, &ctx);
        self.confirm_dialog(&ctx);
        crate::rules_ui::show(self, &ctx);
        crate::iure::dialogs(self, &ctx);
        crate::iure::bell(self, &ctx);
        crate::assistant::frame(self, &ctx);
        crate::suite_ui::frame(self, &ctx);
        crate::import_ui::frame(self, &ctx);
        crate::security_ui::frame(self, &ctx);
        self.sync_window(&ctx);
        if self.session.wizard.is_some() {
            crate::onboarding::modal(self, &ctx);
        }
        self.show_palette(&ctx);
        self.show_toasts(&ctx);
        if let Some(d) = self.session.next_deadline() {
            ctx.request_repaint_after(d.max(std::time::Duration::from_millis(16)));
        }
        crate::header::window_frame(self, ui);
        self.remember_place();
    }

    /// Recuerda dónde está el usuario (para abrir ahí la próxima vez, si lo eligió).
    fn remember_place(&mut self) {
        let now = (self.session.module, self.session.folder_sel());
        if self.place != Some(now) {
            self.place = Some(now);
            self.session.remember_place();
        }
    }

    /// La barra de título integrada, si la ventana no lleva la del sistema.
    pub(crate) fn chrome(&self) -> Option<&crate::header::WindowChrome> {
        self.options.chrome.as_ref()
    }

    /// Título de la barra: la carpeta y su total en el correo; el módulo en los demás.
    pub(crate) fn header_title(&self) -> String {
        match self.session.module {
            Module::Mail => format!("{} · {}", self.session.folder_title(), self.session.rows().len()),
            Module::Today => {
                format!("{} · {}", Module::Today.label(), iuredesk_engine::today::long_day(self.session.today()))
            }
            m => m.label().to_string(),
        }
    }

    /// Icono y texto del estado de la sincronización.
    pub(crate) fn sync_label(&self, status: &SyncStatus) -> (iurefficient_ui::Icon, String) {
        match status {
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
        }
    }

    /// Clic en el estado de la sincronización (riel o barra).
    pub(crate) fn sync_clicked(&mut self, ctx: &Context) {
        if self.session.is_live() {
            self.sync_open = !self.sync_open;
        } else if self.session.is_sample() {
            self.notice(Notice::info(
                "This is the sample mailbox: nothing is synced. Add your account to use real mail.",
                "Este es el buzón de ejemplo: no se sincroniza nada. Agrega tu cuenta para usar tu correo.",
            ));
        } else {
            self.run(ctx, "app.add_account");
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

    /// R1.2: abre archivos y enlaces como si los pasara el sistema (se atienden en el próximo
    /// cuadro; pruebas y capturas).
    pub fn open_args(&mut self, args: Vec<String>) {
        self.options.open.extend(args);
    }

    /// R1.2: ventanas de archivos abiertos (pruebas).
    pub fn opened_windows(&self) -> usize {
        self.opened.count()
    }

    /// No queda trabajo de fondo ni renders pendientes (capturas y pruebas deterministas).
    pub fn is_settled(&self) -> bool {
        self.options.open.is_empty()
            && self.opened.pending() == 0
            && self.session.is_idle()
            && !self.html.pending()
            && self.ai.hub.is_idle()
            && self.import.hub.is_idle()
            && !self.suite.hub.busy()
            && !self.attach.is_busy()
    }

    /// UX-6: ¿está abierto el editor de tareas? (pruebas)
    pub fn task_editor_open(&self) -> bool {
        crate::task_editor::is_open(self)
    }

    /// UX-6: abre el editor de una tarea (capturas).
    pub fn demo_task_editor(&mut self, id: iuredesk_model::TaskId) {
        self.session.module = iuredesk_engine::Module::Tasks;
        if let Some(t) = self.session.task(id) {
            self.pim.task_list = Some(t.list);
        }
        self.pim.task_sel = Some(id);
        crate::task_editor::open(self, id);
    }

    /// UX-3: mensajes abiertos en ventanas propias (pruebas).
    pub fn reader_windows(&self) -> Vec<iuredesk_model::MessageId> {
        crate::reader::open_messages(self)
    }

    /// UX-3: abre el mensaje enfocado en su ventana (capturas).
    pub fn demo_reader_window(&mut self) {
        if let Some(m) = self.session.focused() {
            crate::reader::open(self, m);
        }
    }

    /// UX-3: lectura enfocada (capturas y pruebas).
    pub fn reading_focus(&self) -> bool {
        self.mail.focus
    }

    /// UX-3: carpeta de la caché de adjuntos (pruebas).
    pub fn set_attachment_cache(&mut self, root: std::path::PathBuf) {
        self.attach.set_cache_root(root);
    }

    /// UX-3: hay una confirmación de adjunto peligroso abierta (pruebas).
    pub fn attachment_confirm_open(&self) -> bool {
        self.attach.modal()
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

    /// Asistente en «Configurar manualmente» como queda cuando no se encontró nada (capturas).
    pub fn demo_manual_step(&mut self, email: &str) {
        self.session.wizard_begin();
        if let Some(w) = self.session.wizard.as_mut() {
            w.email = email.to_string();
        }
        self.session.wizard_manual();
        if let Some(w) = self.session.wizard.as_mut() {
            w.message = Some(iuredesk_engine::Notice::info(
                "We couldn't find the mail servers for that address. Enter them below (your provider has them) and test the connection.",
                "No encontramos los servidores de correo de esa dirección. Escríbelos abajo (tu proveedor te los da) y prueba la conexión.",
            ));
        }
    }

    /// Asistente con una dirección que ya es de una cuenta (capturas).
    pub fn demo_duplicate_step(&mut self, email: &str) {
        self.session.wizard_begin();
        if let Some(w) = self.session.wizard.as_mut() {
            w.email = email.to_string();
        }
        self.session.wizard_check_existing();
    }

    /// Ajustes abiertos en una sección (`accounts`, `appearance`…) y, con `edit`, el editor de
    /// esa cuenta (capturas).
    pub fn demo_settings(&mut self, section: &str, edit: Option<&str>) {
        let s = match section {
            "general" => crate::settings::Section::General,
            "appearance" => crate::settings::Section::Appearance,
            "mail" => crate::settings::Section::Mail,
            "privacy" => crate::settings::Section::Privacy,
            "integrations" => crate::settings::Section::Integrations,
            "rules" => crate::settings::Section::Rules,
            "labels" => crate::settings::Section::Labels,
            "advanced" => crate::settings::Section::Advanced,
            _ => crate::settings::Section::Accounts,
        };
        self.open_settings(s, edit.map(str::to_string), None);
    }

    /// Desplaza la lista de correo hasta la fila `row` en el próximo cuadro (banco de pruebas).
    pub fn demo_scroll_mail(&mut self, row: usize) {
        self.mail.list.scroll_to(row);
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

    /// Aplica la tipografía (familia, tamaño del texto y nitidez) si cambió en las preferencias.
    /// En vivo: no hace falta reiniciar.
    pub(crate) fn apply_fonts(&mut self, ctx: &Context, force: bool) {
        let p = &self.session.prefs;
        let want = (p.font.clone(), p.text_scale_pct, p.crisp_text);
        if !force && self.applied_fonts.as_ref() == Some(&want) {
            return;
        }
        let font_changed = self.applied_fonts.as_ref().is_none_or(|a| a.0 != want.0 || a.2 != want.2);
        iurefficient_ui::fonts::set_crisp_text(ctx, want.2);
        if force || font_changed {
            match iurefficient_ui::fonts::FontChoice::from_key(&want.0) {
                iurefficient_ui::fonts::FontChoice::Bundled => {
                    system::install_fonts(ctx, self.options.system_fonts);
                }
                choice => {
                    iurefficient_ui::fonts::install_choice(ctx, &choice);
                }
            }
        }
        system::set_text_scale(ctx, self.session.prefs.text_scale());
        self.applied_fonts = Some(want);
    }

    /// Diálogo de confirmación de `Outcome::Confirm` («¿Eliminar 3 contactos?»).
    fn confirm_dialog(&mut self, ctx: &Context) {
        let Some((text, yes, command)) = self.confirm.clone() else { return };
        let mut answer: Option<bool> = None;
        let modal = egui::Modal::new(egui::Id::unique("confirm")).show(ctx, |ui| {
            ui.set_max_width(380.0);
            let c = theme::colors(ui);
            ui.add(egui::Label::new(egui::RichText::new(text.get()).color(c.text)).wrap());
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let y = ui.add(egui::Button::new(egui::RichText::new(yes.get()).color(c.on_accent)).fill(c.danger));
                if y.clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
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
                self.confirm = None;
                self.run(ctx, &command);
            }
            Some(false) => self.confirm = None,
            None => {}
        }
    }

    /// Abre los ajustes en una sección (y, si se da, el editor de esa cuenta o la confirmación de
    /// quitarla).
    pub(crate) fn open_settings(
        &mut self,
        section: crate::settings::Section,
        edit: Option<String>,
        remove: Option<String>,
    ) {
        self.settings_open = true;
        self.settings_ui.section = section;
        if let Some(id) = edit {
            crate::settings::open_editor(self, &id);
        }
        if remove.is_some() {
            self.settings_ui.confirm_remove = remove;
        }
    }

    /// Aplica tema y densidad si cambiaron en las preferencias.
    pub(crate) fn apply_style(&mut self, ctx: &Context, force: bool) {
        let want = (self.session.prefs.theme, self.session.prefs.density);
        if force || self.applied != Some(want) {
            theme::apply(ctx, kit_theme(want.0), kit_density(want.1));
            self.applied = Some(want);
        }
        self.apply_fonts(ctx, force);
    }

    fn poll(&mut self, ctx: &Context) {
        // El puente MCP escribió en el almacén (borrador, evento, nota): recargar.
        if let Some(f) = &self.options.external_change
            && f.swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            self.session.reload_external();
        }
        // R1.2: archivos y enlaces del arranque y de otras instancias.
        if !self.options.open.is_empty() {
            let args = std::mem::take(&mut self.options.open);
            crate::opened::handle(self, ctx, &args);
        }
        let batches: Vec<Vec<String>> = match &self.options.incoming {
            Some(q) => q.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default(),
            None => Vec::new(),
        };
        for args in batches {
            crate::opened::handle(self, ctx, &args);
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            // Wayland no deja robar el foco: al menos que la ventana pida atención.
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Informational));
        }
        for ev in self.session.poll() {
            match ev {
                Event::Notice(n) => self.notice(n),
                Event::NewMail { summary, detail, account, link } => {
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
                        n(&title, &detail, link.as_deref());
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
                    } else if value.is::<iuredesk_engine::session::FileOpened>() {
                        if let Ok(f) = value.downcast::<iuredesk_engine::session::FileOpened>() {
                            crate::opened::on_loaded(self, id, *f);
                        }
                    } else if value.is::<crate::diagnostics::StoreReport>() {
                        if let Ok(r) = value.downcast::<crate::diagnostics::StoreReport>() {
                            self.diag.on_store(id, *r);
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
            Outcome::Assist(id) if id.starts_with("print.") || id.starts_with("security.") => {
                crate::security_ui::command(self, ctx, &id)
            }
            Outcome::Assist(id) => crate::assistant::command(self, ctx, &id),
            Outcome::Confirm { text, yes, command } => self.confirm = Some((text, yes, command)),
            Outcome::Rules(id) => crate::rules_ui::command(self, ctx, &id),
            Outcome::View(id) => crate::mail::view_command(self, ctx, &id),
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
        let in_mail = self.session.module == Module::Mail;
        let mut fire: Option<&'static str> = None;
        ctx.input_mut(|i| {
            let mut consumed = None;
            for (idx, e) in i.events.iter().enumerate() {
                let egui::Event::Key { key, pressed: true, modifiers, .. } = e else { continue };
                // UX-3: Ctrl+= también acerca (teclados donde «+» lleva Mayús).
                let key = if *key == Key::Equals { Key::Plus } else { *key };
                for (cmd, chord) in command::bindings(scheme) {
                    if cmd == "app.palette"
                        || (typing && !chord.has_modifier())
                        || (cmd.starts_with("cal.") && !in_calendar)
                        // El zoom del mensaje solo en el correo (en lo demás, el de egui).
                        || (cmd.starts_with("view.zoom") && !in_mail)
                        || ((cmd == "mail.open_window" || cmd == "view.reading_focus") && !in_mail)
                    {
                        continue;
                    }
                    if chord_matches(&chord, key, *modifiers) {
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
            ("today", sc("nav.today")),
            ("mail", sc("nav.mail")),
            ("calendar", sc("nav.calendar")),
            ("contacts", sc("nav.contacts")),
            ("notes", sc("nav.notes")),
            ("tasks", sc("nav.tasks")),
            ("assistant", sc("nav.assistant")),
        ];
        let short = |k: &str| labels.iter().find(|(id, _)| *id == k).and_then(|(_, s)| s.clone()).unwrap_or_default();
        let s_today = short("today");
        let (s_mail, s_cal, s_con, s_not, s_tas, s_ai) =
            (short("mail"), short("calendar"), short("contacts"), short("notes"), short("tasks"), short("assistant"));
        let status = self.session.sync_status();
        let (sync_icon, sync_label) = self.sync_label(&status);
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
            NavItem::new("today", iurefficient_ui::Icon::Sun, Module::Today.label()).shortcut(&s_today),
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
        // La campana de Iurefficient está en la barra de título (UI-HEADER).
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
            Some("settings") => self.open_settings(crate::settings::Section::Accounts, None, None),
            Some("sync") => self.sync_clicked(&ui.ctx().clone()),
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
        let mut configure: Option<String> = None;
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
                    ui.horizontal(|ui| {
                        ui.add_space(14.0);
                        if r.needs_signin() && ui.button(pick("Sign in again…", "Volver a iniciar sesión…")).clicked()
                        {
                            reauth = Some(r.id.clone());
                        }
                        if !r.id.is_empty() && ui.link(pick("Account settings…", "Configurar cuenta…")).clicked() {
                            configure = Some(r.id.clone());
                        }
                    });
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
        if let Some(id) = configure {
            self.sync_open = false;
            self.open_settings(crate::settings::Section::Accounts, Some(id), None);
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

/// La combinación como atajo de egui (solo si lleva modificadores: los de una tecla no se
/// muestran ni se consumen en un campo).
pub(crate) fn keyboard_shortcut(c: &Chord) -> Option<egui::KeyboardShortcut> {
    if !c.has_modifier() {
        return None;
    }
    let name = match c.key.strip_prefix("Num") {
        Some(d) if d.len() == 1 => format!("Digit{d}"),
        _ => c.key.to_string(),
    };
    let key = Key::from_name(&name)?;
    let mut m = Modifiers::NONE;
    if c.command || (c.ctrl && !cfg!(target_os = "macos")) {
        m |= Modifiers::COMMAND;
    }
    if c.ctrl && cfg!(target_os = "macos") {
        m |= Modifiers::CTRL;
    }
    if c.shift {
        m |= Modifiers::SHIFT;
    }
    if c.alt {
        m |= Modifiers::ALT;
    }
    Some(egui::KeyboardShortcut::new(m, key))
}

/// ¿La tecla y los modificadores son exactamente la combinación?
pub(crate) fn chord_matches(c: &Chord, key: Key, m: Modifiers) -> bool {
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
