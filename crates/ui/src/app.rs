//! La aplicación: arranque del estilo, riel de módulos, atajos por esquema, paleta de comandos,
//! avisos con «Deshacer» y las ventanas (redactor, ajustes, alta de cuenta).

use std::sync::Arc;

use egui::{Context, Key, Modifiers, Ui};
use iuredesk_engine::command::{self, Chord};
use iuredesk_engine::{Event, Module, Notice, NoticeKind, Outcome, Session};
use iurefficient_ui::palette::{CommandPalette, PaletteItem};
use iurefficient_ui::widgets::nav_rail::rail_width;
use iurefficient_ui::{NavItem, PaneKind, Severity, Toasts, lang, nav_rail, pick, split, system, theme};

use crate::calendar::CalendarState;
use crate::compose::ComposeWindow;
use crate::html::HtmlCache;
use crate::mail::MailState;
use crate::pim::PimState;
use crate::widgets::{kit_density, kit_theme};

/// Opciones de arranque.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Usar las fuentes del sistema (las capturas sin pantalla también las usan si existen).
    pub system_fonts: bool,
    /// No seguir el tema del escritorio (capturas y pruebas).
    pub no_theme_watch: bool,
    /// Enlace `mailto:` con el que se abrió la aplicación.
    pub mailto: Option<String>,
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
    pub(crate) composers: Vec<ComposeWindow>,
    next_composer: u64,
    pub settings_open: bool,
    applied: Option<(iuredesk_engine::ThemePref, iuredesk_engine::DensityPref)>,
}

impl DeskApp {
    pub fn new(session: Session, options: Options) -> Self {
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
            composers: Vec::new(),
            next_composer: 0,
            settings_open: false,
            applied: None,
        };
        if let Some(m) = app.options.mailto.clone()
            && let Some(d) = iuredesk_engine::ComposeDraft::from_mailto(&m)
        {
            app.open_composer(d);
        }
        app
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
            self.toasts.show(&ctx);
            return;
        }

        let modal = self.palette.is_open() || self.session.wizard.is_some() || self.settings_open;
        if !modal {
            self.shortcuts(&ctx);
        }
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
            Module::Assistant => crate::pim::assistant(self, ui),
        }

        crate::compose::show_all(self, &ctx);
        crate::settings::show(self, &ctx);
        if self.session.wizard.is_some() {
            crate::onboarding::modal(self, &ctx);
        }
        self.show_palette(&ctx);
        if let Some(id) = self.toasts.show(&ctx)
            && Some(id) == self.undo_toast
        {
            self.undo_toast = None;
            self.run(&ctx, "undo");
        }
    }

    /// No queda trabajo de fondo ni renders pendientes (capturas y pruebas deterministas).
    pub fn is_settled(&self) -> bool {
        self.session.is_idle() && !self.html.pending()
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
        self.calendar.view = crate::calendar::CalView::Month;
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
        for ev in self.session.poll() {
            match ev {
                Event::Notice(n) => self.notice(n),
                Event::Custom { id, value } => {
                    if let Some(f) = value.downcast_ref::<iuredesk_engine::Fetched>() {
                        self.html.on_fetched(f.clone());
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
            let id = self.toasts.push(sev, n.text.get(), Some(pick("Undo", "Deshacer")), 10.0);
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
        let mut fire: Option<&'static str> = None;
        ctx.input_mut(|i| {
            let mut consumed = None;
            for (idx, e) in i.events.iter().enumerate() {
                let egui::Event::Key { key, pressed: true, modifiers, .. } = e else { continue };
                for (cmd, chord) in command::bindings(scheme) {
                    if cmd == "app.palette" || (typing && !chord.has_modifier()) {
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
        let sync_label = if self.session.is_sample() {
            pick("Sample mailbox (offline)", "Buzón de ejemplo (sin conexión)")
        } else {
            pick("Sync arrives in F1", "La sincronización llega en F1")
        };
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
            NavItem::new("sync", iurefficient_ui::Icon::Sync, sync_label).bottom(),
        ];
        let w = rail_width(ui.ctx());
        let current = self.session.module.key();
        let mut clicked: Option<String> = None;
        split::left(ui, "rail", PaneKind::Rail, w, w..=w, |ui| {
            if let Some(id) = nav_rail(ui, &items, current) {
                clicked = Some(id.to_string());
            }
        });
        match clicked.as_deref() {
            Some("settings") => self.settings_open = true,
            Some("sync") => self.notice(Notice::info(
                "Server sync arrives in phase F1. Everything you do is kept in the local queue.",
                "La sincronización con servidores llega en la fase F1. Lo que hagas queda en la cola local.",
            )),
            Some(k) => {
                if let Some(m) = Module::from_key(k) {
                    self.session.module = m;
                }
            }
            None => {}
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
