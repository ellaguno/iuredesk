//! Ajustes: una lista de secciones a la izquierda («Cuentas» primero) y su contenido a la
//! derecha. Cuentas: cada una con su proveedor, estado y acciones (editar, volver a iniciar
//! sesión, sincronizar ahora, quitar); el editor cambia nombre, firma, identidades y, en las de
//! IMAP/SMTP, los servidores con «Probar conexión». Apariencia: tema, densidad y tipografía.

use egui::{Context, RichText, Ui};
use iuredesk_engine::session::{ConnectionTest, kind_label, servers_editable};
use iuredesk_engine::{AccountRow, DensityPref, ShortcutScheme, StartPref, ThemePref};
use iuredesk_model::account::{Account, Identity, Protocol};
use iurefficient_ui::fonts::FontChoice;
use iurefficient_ui::{Icon, avatar, icons, lang, pick, section_label, segmented, sidebar_item, theme};

use crate::app::{DeskApp, set_language};

/// Secciones de los ajustes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Section {
    #[default]
    Accounts,
    General,
    Appearance,
    Mail,
    /// UX-5: imágenes remotas y rastreo.
    Privacy,
    /// Reglas, categorías y respuesta automática (F5.3).
    Rules,
    /// Etiquetas y sus colores (F5.3).
    Labels,
    Integrations,
    /// Rendimiento y diagnóstico (R2.1).
    Advanced,
}

impl Section {
    const ALL: [Section; 9] = [
        Section::Accounts,
        Section::General,
        Section::Appearance,
        Section::Mail,
        Section::Privacy,
        Section::Rules,
        Section::Labels,
        Section::Integrations,
        Section::Advanced,
    ];

    fn label(self) -> &'static str {
        match self {
            Section::Accounts => pick("Accounts", "Cuentas"),
            Section::General => pick("General", "General"),
            Section::Appearance => pick("Appearance", "Apariencia"),
            Section::Mail => pick("Mail", "Correo"),
            Section::Privacy => pick("Privacy", "Privacidad"),
            Section::Rules => pick("Rules", "Reglas"),
            Section::Labels => pick("Labels", "Etiquetas"),
            Section::Integrations => pick("Integrations", "Integraciones"),
            Section::Advanced => pick("Advanced", "Avanzado"),
        }
    }

    fn icon(self) -> Icon {
        match self {
            Section::Accounts => Icon::Person,
            Section::General => Icon::Settings,
            Section::Appearance => Icon::Sun,
            Section::Mail => Icon::Mail,
            Section::Privacy => Icon::Lock,
            Section::Rules => Icon::Filter,
            Section::Labels => Icon::Tag,
            Section::Integrations => Icon::Sparkle,
            Section::Advanced => Icon::Bolt,
        }
    }
}

/// Una cuenta en edición.
pub struct AccountEditor {
    pub(crate) account: Account,
    signature: String,
    /// Contraseña nueva (vacía: se conserva la del llavero).
    password: String,
}

/// Estado de los ajustes.
#[derive(Default)]
pub struct AccountsUi {
    pub(crate) section: Section,
    /// Firma en edición por dirección (cuentas sin editor: el buzón de ejemplo).
    signatures: std::collections::HashMap<String, String>,
    /// Cuenta pendiente de confirmar que se quita.
    pub(crate) confirm_remove: Option<String>,
    pub(crate) editor: Option<AccountEditor>,
    /// R1.2: «Abrir al iniciar sesión» leído del sistema (una vez por apertura de Ajustes).
    autostart: Option<iuredesk_desktop::autostart::State>,
}

/// Abre el editor de una cuenta.
pub(crate) fn open_editor(app: &mut DeskApp, id: &str) {
    let Some(account) = app.session.account(id).cloned() else { return };
    let signature = app.session.signature_text(&account.email);
    app.session.clear_connection_test();
    app.settings_ui.editor = Some(AccountEditor { account, signature, password: String::new() });
}

pub fn show(app: &mut DeskApp, ctx: &Context) {
    if !app.settings_open {
        app.settings_ui.autostart = None;
        return;
    }
    let mut open = true;
    let mut run: Option<String> = None;
    let max_h = (ctx.content_rect().height() - 150.0).clamp(200.0, 700.0);
    egui::Window::new(pick("Settings", "Ajustes"))
        .id(egui::Id::unique("settings"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_TOP, [0.0, 56.0])
        .default_width(760.0)
        .show(ctx, |ui| {
            ui.set_min_width(720.0);
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(170.0);
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for s in Section::ALL {
                        let count = if s == Section::Accounts {
                            u32::try_from(app.session.accounts_overview().len()).unwrap_or(0)
                        } else {
                            0
                        };
                        if sidebar_item(ui, Some(s.icon()), s.label(), count, app.settings_ui.section == s).clicked() {
                            app.settings_ui.section = s;
                        }
                    }
                });
                ui.separator();
                ui.vertical(|ui| {
                    ui.set_width(520.0);
                    ui.set_min_height(max_h.min(620.0));
                    egui::ScrollArea::vertical().max_height(max_h).auto_shrink([false, false]).show(ui, |ui| match app
                        .settings_ui
                        .section
                    {
                        Section::Accounts => accounts(app, ui, &mut run),
                        Section::General => general(app, ui, &mut run),
                        Section::Appearance => appearance(app, ui, &mut run),
                        Section::Mail => mail(app, ui),
                        Section::Privacy => crate::remote_images_ui::settings_section(app, ui),
                        Section::Rules => crate::rules_ui::settings_section(app, ui),
                        Section::Labels => crate::rules_ui::labels_section(app, ui),
                        Section::Integrations => {
                            crate::iure::settings_section(app, ui);
                            crate::assistant::settings_section(app, ui);
                            crate::import_ui::settings_section(app, ui);
                            crate::suite_ui::settings_section(app, ui);
                        }
                        Section::Advanced => crate::diagnostics::section(app, ui),
                    });
                });
            });
        });
    editor_window(app, ctx);
    if !open {
        app.settings_open = false;
        app.settings_ui.editor = None;
    }
    if let Some(id) = run {
        if id == "app.add_account" {
            app.settings_open = false;
        }
        app.run(ctx, &id);
    }
}

fn muted(ui: &mut Ui, text: &str) {
    let c = theme::colors(ui);
    ui.add(
        egui::Label::new(RichText::new(text).color(c.text_muted).size(theme::density(ui.ctx()).small_size())).wrap(),
    );
}

// ------------------------------------------------------------------------------------ cuentas

/// Letra del proveedor para su avatar.
fn provider_mark(kind: iuredesk_model::account::AccountKind) -> &'static str {
    use iuredesk_model::account::AccountKind;
    match kind {
        AccountKind::Gmail => "G",
        AccountKind::Microsoft365 => "M",
        AccountKind::ICloud => "C",
        AccountKind::Iurefficient => "U",
        AccountKind::Generic => "@",
    }
}

/// Estado de una cuenta en palabras y su color.
fn status_of(r: &AccountRow, c: &iurefficient_ui::Palette) -> (String, egui::Color32) {
    match &r.status {
        Some(iuredesk_engine::AccountSync::Error { reauth: true, .. }) => {
            (pick("Needs you to sign in again", "Hay que volver a iniciar sesión").to_string(), c.warning)
        }
        Some(s @ iuredesk_engine::AccountSync::Error { .. }) => (s.label(), c.danger),
        Some(s @ iuredesk_engine::AccountSync::Offline(_)) => (s.label(), c.warning),
        Some(s @ iuredesk_engine::AccountSync::Ready { .. }) => (format!("OK · {}", s.label()), c.success),
        Some(s) => (s.label(), c.text_muted),
        None => (String::new(), c.text_muted),
    }
}

fn accounts(app: &mut DeskApp, ui: &mut Ui, run: &mut Option<String>) {
    let c = theme::colors(ui);
    let rows = app.session.accounts_overview();
    section_label(ui, pick("Accounts", "Cuentas"));
    muted(
        ui,
        pick(
            "Your mail, calendar and contacts accounts. Right-click an account in the folder list for the same options.",
            "Tus cuentas de correo, calendario y contactos. Con clic derecho sobre una cuenta en la lista de carpetas tienes las mismas opciones.",
        ),
    );
    ui.add_space(8.0);
    let mut edit: Option<String> = None;
    let mut remove: Option<String> = None;
    let mut reauth: Option<String> = None;
    let mut sync: Option<String> = None;
    let mut save_sig: Option<(String, String)> = None;
    for r in &rows {
        egui::Frame::new()
            .fill(c.surface_alt)
            .stroke(egui::Stroke::new(1.0, c.separator))
            .corner_radius(8)
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    let kind = kind_label(r.kind);
                    avatar(ui, provider_mark(r.kind), kind, 36.0);
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&r.email).strong().color(c.text));
                            iurefficient_ui::badge(ui, kind, iurefficient_ui::BadgeKind::Neutral);
                        });
                        let mut detail = r.name.clone();
                        if !r.server.is_empty() {
                            if !detail.is_empty() {
                                detail.push_str(" · ");
                            }
                            detail.push_str(&r.server);
                        }
                        if !detail.is_empty() {
                            ui.label(RichText::new(detail).color(c.text_muted));
                        }
                        let (st, ink) = status_of(r, &c);
                        if !st.is_empty() {
                            ui.add(egui::Label::new(RichText::new(st).color(ink)).wrap());
                        }
                    });
                });
                if r.id.is_empty() {
                    // Buzón de ejemplo: solo la firma.
                    ui.add_space(4.0);
                    ui.label(RichText::new(pick("Signature", "Firma")).color(c.text_muted));
                    let text =
                        app.settings_ui.signatures.entry(r.email.clone()).or_insert_with(|| r.signature.clone());
                    let resp = ui.add(
                        egui::TextEdit::multiline(text)
                            .desired_rows(2)
                            .desired_width(f32::INFINITY)
                            .hint_text(pick("Name, title, phone…", "Nombre, cargo, teléfono…")),
                    );
                    if resp.lost_focus() && *text != r.signature {
                        save_sig = Some((r.email.clone(), text.clone()));
                    }
                    return;
                }
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    if ui.button(pick("Edit…", "Editar…")).clicked() {
                        edit = Some(r.id.clone());
                    }
                    let signin = pick("Sign in again…", "Volver a iniciar sesión…");
                    let b = if r.needs_signin() {
                        ui.add(egui::Button::new(RichText::new(signin).color(c.on_accent)).fill(c.accent))
                    } else {
                        ui.button(signin)
                    };
                    if b.clicked() {
                        reauth = Some(r.id.clone());
                    }
                    if ui.button(pick("Sync now", "Sincronizar ahora")).clicked() {
                        sync = Some(r.id.clone());
                    }
                    if ui.button(RichText::new(pick("Remove account…", "Eliminar cuenta…")).color(c.danger)).clicked() {
                        app.settings_ui.confirm_remove = Some(r.id.clone());
                    }
                });
                if app.settings_ui.confirm_remove.as_deref() == Some(r.id.as_str()) {
                    ui.add_space(6.0);
                    egui::Frame::new()
                        .stroke(egui::Stroke::new(1.0, c.danger))
                        .corner_radius(6)
                        .inner_margin(egui::Margin::same(8))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!(
                                        "{} {}",
                                        pick("Remove", "¿Eliminar"),
                                        r.email
                                    ) + pick(" from IureDesk?", " de IureDesk?"))
                                    .strong()
                                    .color(c.text),
                                )
                                .wrap(),
                            );
                            muted(
                                ui,
                                pick(
                                    "Its mail, folders, calendars, contacts and tasks downloaded to this computer, and its passwords in the system keyring, are deleted. Nothing is deleted on the server: you can add it again at any time. This can't be undone.",
                                    "Se borran de este equipo sus correos, carpetas, calendarios, contactos y tareas descargados, y sus contraseñas del llavero del sistema. En el servidor no se borra nada: puedes volver a agregarla cuando quieras. Esto no se puede deshacer.",
                                ),
                            );
                            ui.horizontal(|ui| {
                                let yes = ui.add(
                                    egui::Button::new(
                                        RichText::new(pick("Yes, remove", "Sí, eliminar")).color(c.on_accent),
                                    )
                                    .fill(c.danger),
                                );
                                if yes.clicked() {
                                    remove = Some(r.id.clone());
                                }
                                if ui.button(pick("Cancel", "Cancelar")).clicked() {
                                    app.settings_ui.confirm_remove = None;
                                }
                            });
                        });
                }
            });
        ui.add_space(6.0);
    }
    if rows.is_empty() {
        muted(ui, pick("There are no accounts yet.", "Todavía no hay cuentas."));
    }
    if ui.button(pick("Add account…", "Agregar cuenta…")).clicked() {
        *run = Some("app.add_account".into());
    }
    let ctx = ui.ctx().clone();
    if let Some((email, text)) = save_sig {
        app.session.set_signature_text(&email, &text);
    }
    if let Some(id) = edit {
        open_editor(app, &id);
    }
    if let Some(id) = sync {
        app.session.sync_account_now(&id);
        app.notice(iuredesk_engine::Notice::info("Syncing…", "Sincronizando…"));
    }
    if let Some(id) = remove {
        app.settings_ui.confirm_remove = None;
        if app.settings_ui.editor.as_ref().is_some_and(|e| e.account.id == id) {
            app.settings_ui.editor = None;
        }
        match app.session.remove_account(&id) {
            Ok(out) => app.outcome(&ctx, out),
            Err(e) => app.notice(iuredesk_engine::Notice::error(e.text())),
        }
    }
    if let Some(id) = reauth {
        app.settings_open = false;
        app.session.reauth_account(&id);
    }
}

/// Ventana del editor de una cuenta.
fn editor_window(app: &mut DeskApp, ctx: &Context) {
    let Some(mut ed) = app.settings_ui.editor.take() else { return };
    let mut open = true;
    let mut close = false;
    let mut save = false;
    let mut test = false;
    let test_state = app.session.connection_test(&ed.account.id).cloned();
    let editable = servers_editable(&ed.account);
    // Modal: siempre encima de los ajustes (una ventana normal podía quedar detrás).
    let modal = egui::Modal::new(egui::Id::unique("account-editor")).show(ctx, |ui| {
            ui.set_width(540.0);
            let c = theme::colors(ui);
            ui.heading(pick("Edit account", "Editar cuenta"));
            ui.separator();
            egui::ScrollArea::vertical().max_height((ctx.content_rect().height() - 240.0).max(200.0)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    avatar(ui, provider_mark(ed.account.kind), kind_label(ed.account.kind), 32.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&ed.account.email).strong().color(c.text));
                        ui.label(RichText::new(kind_label(ed.account.kind)).color(c.text_muted));
                    });
                });
                ui.add_space(10.0);
                section_label(ui, pick("Name and signature", "Nombre y firma"));
                egui::Grid::new("acc-name").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                    let l = ui.label(pick("Your name", "Tu nombre"));
                    ui.add(egui::TextEdit::singleline(&mut ed.account.display_name).desired_width(300.0))
                        .labelled_by(l.id);
                    ui.end_row();
                });
                let l = ui.label(RichText::new(pick("Signature", "Firma")).color(c.text_muted));
                ui.add(
                    egui::TextEdit::multiline(&mut ed.signature)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY)
                        .hint_text(pick("Name, title, phone…", "Nombre, cargo, teléfono…")),
                )
                .labelled_by(l.id);
                ui.add_space(10.0);
                section_label(ui, pick("Identities (From:)", "Identidades (De:)"));
                muted(
                    ui,
                    pick(
                        "Other addresses you can send from with this account.",
                        "Otras direcciones desde las que puedes enviar con esta cuenta.",
                    ),
                );
                let mut drop: Option<usize> = None;
                for (i, idn) in ed.account.identities.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut idn.name)
                                .desired_width(150.0)
                                .hint_text(pick("Name", "Nombre")),
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut idn.email)
                                .desired_width(220.0)
                                .hint_text(pick("address@example.com", "direccion@ejemplo.com")),
                        );
                        if icons::icon_button(ui, Icon::Close, pick("Remove identity", "Quitar identidad"), true)
                            .clicked()
                        {
                            drop = Some(i);
                        }
                    });
                }
                if let Some(i) = drop
                    && i < ed.account.identities.len()
                {
                    ed.account.identities.remove(i);
                }
                if ui.button(pick("Add identity", "Agregar identidad")).clicked() && ed.account.identities.len() < 50 {
                    ed.account.identities.push(Identity {
                        name: ed.account.display_name.clone(),
                        email: String::new(),
                        signature: None,
                        reply_to: None,
                    });
                }
                ui.add_space(10.0);
                if editable {
                    let proto = match ed.account.incoming.protocol {
                        Protocol::Pop3 => "POP3",
                        _ => "IMAP",
                    };
                    section_label(ui, &format!("{} ({proto})", pick("Incoming mail", "Correo entrante")));
                    crate::server_form::server_fields(ui, "acc-in", &mut ed.account.incoming);
                    ui.add_space(8.0);
                    section_label(ui, &format!("{} (SMTP)", pick("Outgoing mail", "Correo saliente")));
                    crate::server_form::server_fields(ui, "acc-out", &mut ed.account.outgoing);
                    ui.add_space(8.0);
                    section_label(ui, pick("Calendar and contacts", "Calendario y contactos"));
                    egui::Grid::new("acc-dav").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                        for (label, url) in [
                            (pick("CalDAV address", "Dirección CalDAV"), &mut ed.account.caldav_url),
                            (pick("CardDAV address", "Dirección CardDAV"), &mut ed.account.carddav_url),
                        ] {
                            let l = ui.label(label);
                            let mut s = url.clone().unwrap_or_default();
                            if ui
                                .add(egui::TextEdit::singleline(&mut s).desired_width(300.0).hint_text("https://…"))
                                .labelled_by(l.id)
                                .changed()
                            {
                                *url = Some(s);
                            }
                            ui.end_row();
                        }
                    });
                    ui.add_space(8.0);
                    section_label(ui, pick("Password", "Contraseña"));
                    let pw_label = pick("New password", "Contraseña nueva");
                    ui.add(
                        egui::TextEdit::singleline(&mut ed.password)
                            .password(true)
                            .desired_width(300.0)
                            .hint_text(pick("Leave empty to keep the saved one", "Déjala vacía para conservar la guardada")),
                    )
                    .on_hover_text(pw_label);
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        let running = matches!(test_state, Some(ConnectionTest::Running(_)));
                        if ui.add_enabled(!running, egui::Button::new(pick("Test connection", "Probar conexión"))).clicked()
                        {
                            test = true;
                        }
                        match &test_state {
                            Some(ConnectionTest::Running(_)) => {
                                ui.spinner();
                                ui.label(RichText::new(pick("Testing…", "Probando…")).color(c.text_muted));
                            }
                            Some(ConnectionTest::Ok) => {
                                ui.label(RichText::new(pick("Connection works", "La conexión funciona")).color(c.success));
                            }
                            _ => {}
                        }
                    });
                    if let Some(ConnectionTest::Failed(t)) = &test_state {
                        ui.add(egui::Label::new(RichText::new(t.get()).color(c.danger)).wrap());
                    }
                } else {
                    muted(
                        ui,
                        pick(
                            "The servers of this account are set by the provider. If it stopped working, use «Sign in again».",
                            "Los servidores de esta cuenta los fija el proveedor. Si dejó de funcionar, usa «Volver a iniciar sesión».",
                        ),
                    );
                }
            });
            // Guardar y Cancelar siempre a la vista, fuera del desplazamiento.
            ui.add_space(8.0);
            ui.separator();
            ui.horizontal(|ui| {
                if iurefficient_ui::primary_button(ui, pick("Save", "Guardar")).clicked() {
                    save = true;
                }
                if ui.button(pick("Cancel", "Cancelar")).clicked() {
                    close = true;
                }
            });
        });
    if modal.should_close() {
        open = false;
    }
    if test {
        let pw = (!ed.password.is_empty()).then(|| ed.password.clone());
        app.session.test_account_connection(&ed.account, pw.as_deref());
    }
    if save {
        let pw = (!ed.password.is_empty()).then(|| ed.password.clone());
        let old_sig = app.session.signature_text(&ed.account.email);
        if ed.signature.trim() != old_sig.trim() {
            app.session.set_signature_text(&ed.account.email, &ed.signature);
        }
        match app.session.save_account(&ed.account, pw.as_deref()) {
            Ok(out) => {
                let ok = matches!(&out, iuredesk_engine::Outcome::Notice(n) if n.kind == iuredesk_engine::NoticeKind::Success);
                app.outcome(ctx, out);
                if ok {
                    close = true;
                }
            }
            Err(e) => app.notice(iuredesk_engine::Notice::error(e.text())),
        }
    }
    if open && !close {
        app.settings_ui.editor = Some(ed);
    } else {
        app.session.clear_connection_test();
    }
}

// ------------------------------------------------------------------------------------ general

fn general(app: &mut DeskApp, ui: &mut Ui, run: &mut Option<String>) {
    let c = theme::colors(ui);
    section_label(ui, pick("General", "General"));
    ui.label(RichText::new(pick("When IureDesk opens:", "Al abrir IureDesk:")).color(c.text));
    let mut st = app.session.prefs.start;
    if segmented(
        ui,
        &mut st,
        &[
            (StartPref::Today, pick("My day", "Mi día")),
            (StartPref::LastPlace, pick("Where I left off", "Donde lo dejé")),
        ],
    )
    .changed()
    {
        app.session.prefs.start = st;
        app.session.save_prefs();
    }
    ui.add_space(8.0);
    startup(app, ui);
    ui.add_space(14.0);
    section_label(ui, pick("Language", "Idioma"));
    let mut l = lang::lang();
    if segmented(ui, &mut l, &[(lang::Lang::Es, "Español"), (lang::Lang::En, "English")]).changed() {
        set_language(l);
        app.session.prefs.lang = Some(if l == lang::Lang::En { "en".into() } else { "es".into() });
        app.session.save_prefs();
    }
    ui.add_space(14.0);
    section_label(ui, pick("Keyboard shortcuts", "Atajos de teclado"));
    muted(ui, pick("Use the keys you already know.", "Usa las teclas que ya conoces."));
    let mut s = app.session.prefs.scheme;
    if segmented(ui, &mut s, &ShortcutScheme::ALL.map(|s| (s, s.label()))).changed() {
        *run = Some(format!("view.scheme.{}", s.key()));
    }
}

/// R1.2: «Abrir IureDesk al iniciar sesión» e «Iniciar minimizado» (XDG Autostart, `HKCU\…\Run`
/// o un `LaunchAgent`; el estado se lee del sistema, no de las preferencias).
fn startup(app: &mut DeskApp, ui: &mut Ui) {
    use iuredesk_desktop::autostart;
    let st = *app.settings_ui.autostart.get_or_insert_with(autostart::state);
    let (mut on, mut min) = (st.enabled, st.minimized);
    let a = ui.checkbox(&mut on, pick("Open IureDesk when I sign in", "Abrir IureDesk al iniciar sesión")).changed();
    let b = ui
        .add_enabled_ui(on, |ui| {
            ui.indent("autostart-min", |ui| {
                ui.checkbox(&mut min, pick("Start minimized", "Iniciar minimizado")).changed()
            })
            .inner
        })
        .inner;
    if a || b {
        match autostart::set(on, min) {
            Ok(()) => app.settings_ui.autostart = Some(autostart::State { enabled: on, minimized: min }),
            Err(e) => {
                let (en, es) = e.human();
                app.notice(iuredesk_engine::Notice::error(iuredesk_engine::Text::new(en, es)));
            }
        }
    }
}

// ------------------------------------------------------------------------------------ apariencia

/// Nombre de una tipografía para el menú.
fn font_label(choice: &FontChoice, desktop: Option<&str>) -> String {
    match choice {
        FontChoice::Bundled => pick("Inter (included, default)", "Inter (incluida, por defecto)").to_string(),
        FontChoice::Desktop => match desktop {
            Some(d) => format!("{} ({d})", pick("System font", "Fuente del sistema")),
            None => pick("System font", "Fuente del sistema").to_string(),
        },
        FontChoice::Family(f) => f.clone(),
    }
}

fn appearance(app: &mut DeskApp, ui: &mut Ui, run: &mut Option<String>) {
    section_label(ui, pick("Theme", "Tema"));
    let mut t = app.session.prefs.theme;
    if segmented(
        ui,
        &mut t,
        &[
            (ThemePref::System, pick("System", "Sistema")),
            (ThemePref::Light, pick("Light", "Claro")),
            (ThemePref::Dark, pick("Dark", "Oscuro")),
            (ThemePref::HighContrast, pick("High contrast", "Alto contraste")),
        ],
    )
    .changed()
    {
        *run = Some(format!("view.theme.{}", t.key()));
    }
    ui.add_space(8.0);
    section_label(ui, pick("Density", "Densidad"));
    let mut d = app.session.prefs.density;
    if segmented(
        ui,
        &mut d,
        &[
            (DensityPref::Comfortable, pick("Comfortable", "Cómoda")),
            (DensityPref::Compact, pick("Compact", "Compacta")),
            (DensityPref::Dense, pick("Dense", "Densa")),
        ],
    )
    .changed()
    {
        *run = Some(format!("view.density.{}", d.key()));
    }
    ui.add_space(14.0);
    typography(app, ui);
    ui.add_space(14.0);
    section_label(ui, pick("Window", "Ventana"));
    let mut native = app.session.prefs.native_decorations;
    if ui.checkbox(&mut native, pick("System title bar", "Barra de título del sistema")).changed() {
        app.session.prefs.native_decorations = native;
        app.session.save_prefs();
    }
    muted(
        ui,
        pick(
            "Use the window frame of your desktop instead of the integrated bar. Takes effect when IureDesk restarts.",
            "Usa el marco de ventana de tu escritorio en lugar de la barra integrada. Se aplica al reiniciar IureDesk.",
        ),
    );
}

/// «Tipografía»: familia, tamaño del texto y nitidez. Se aplica en vivo.
fn typography(app: &mut DeskApp, ui: &mut Ui) {
    section_label(ui, pick("Typography", "Tipografía"));
    let desktop = iurefficient_ui::fonts::desktop_font().map(|(f, _)| f);
    let mut choices = vec![FontChoice::Bundled, FontChoice::Desktop];
    choices.extend(iurefficient_ui::fonts::available_families().into_iter().map(FontChoice::Family));
    let current = FontChoice::from_key(&app.session.prefs.font);
    if !choices.contains(&current) {
        choices.push(current.clone());
    }
    let mut chosen = current.clone();
    egui::Grid::new("typography").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label(pick("Font", "Fuente"));
        egui::ComboBox::from_id_salt("font-choice")
            .width(280.0)
            .selected_text(font_label(&current, desktop.as_deref()))
            .show_ui(ui, |ui| {
                for ch in &choices {
                    ui.selectable_value(&mut chosen, ch.clone(), font_label(ch, desktop.as_deref()));
                }
            });
        ui.end_row();
        ui.label(pick("Text size", "Tamaño del texto"));
        // Se aplica al soltar: cambiar el zoom mientras se arrastra movería el control.
        let drag_id = egui::Id::unique("text-scale-drag");
        let saved = app.session.prefs.text_scale_pct.clamp(90, 130);
        let mut pct = ui.ctx().data(|d| d.get_temp::<u32>(drag_id)).unwrap_or(saved);
        ui.spacing_mut().slider_width = 200.0;
        let r = ui.add(egui::Slider::new(&mut pct, 90..=130).step_by(5.0).suffix(" %"));
        if r.dragged() {
            ui.ctx().data_mut(|d| d.insert_temp(drag_id, pct));
        } else if pct != saved || r.drag_stopped() {
            ui.ctx().data_mut(|d| d.remove::<u32>(drag_id));
            app.session.prefs.text_scale_pct = pct;
            app.session.save_prefs();
        }
        ui.end_row();
    });
    if chosen != current {
        app.session.prefs.font = chosen.key();
        app.session.save_prefs();
    }
    let mut crisp = app.session.prefs.crisp_text;
    if ui.checkbox(&mut crisp, pick("Sharper text", "Nitidez")).changed() {
        app.session.prefs.crisp_text = crisp;
        app.session.save_prefs();
    }
    muted(
        ui,
        pick(
            "Puts every letter on whole pixels and fits the strokes to the pixel grid: crisper text, especially with fractional scaling (125 %, 150 %), at the cost of slightly less even spacing.",
            "Coloca cada letra en píxeles enteros y ajusta los trazos a la rejilla de píxeles: texto más nítido, sobre todo con escalado fraccionario (125 %, 150 %), a cambio de un espaciado un poco menos uniforme.",
        ),
    );
    if let Some(name) = iurefficient_ui::system::installed_font(ui.ctx()) {
        muted(ui, &format!("{} {name}", pick("In use:", "En uso:")));
    }
}

// ------------------------------------------------------------------------------------ correo

fn mail(app: &mut DeskApp, ui: &mut Ui) {
    let c = theme::colors(ui);
    let mut send_prefs = false;
    section_label(ui, pick("Mail", "Correo"));
    let mut conv = app.session.prefs.conversations;
    if ui.checkbox(&mut conv, pick("Group by conversation", "Agrupar por conversación")).changed() {
        app.session.set_conversations(conv);
    }
    let mut mr = app.session.prefs.mark_read_on_open;
    ui.horizontal(|ui| {
        if ui.checkbox(&mut mr, pick("Mark as read when opened, after", "Marcar como leído al abrir, tras")).changed()
        {
            app.session.prefs.mark_read_on_open = mr;
            app.session.save_prefs();
        }
        let mut secs = app.session.prefs.mark_read_delay_ms as f32 / 1000.0;
        let r = ui.add_enabled(
            mr,
            egui::DragValue::new(&mut secs).range(0.0..=10.0).speed(0.1).fixed_decimals(1).suffix(" s"),
        );
        if r.changed() {
            app.session.prefs.mark_read_delay_ms = (secs * 1000.0).round().clamp(0.0, 10_000.0) as u32;
            app.session.save_prefs();
        }
    });
    ui.horizontal(|ui| {
        ui.label(RichText::new(pick("Undo send for", "Deshacer el envío durante")).color(c.text));
        let mut secs = app.session.prefs.send_undo_secs;
        if ui.add(egui::DragValue::new(&mut secs).range(0..=30).suffix(" s")).changed() {
            app.session.prefs.send_undo_secs = secs;
            send_prefs = true;
        }
    });
    let mut nm = app.session.prefs.notify_new_mail;
    if ui
        .checkbox(&mut nm, pick("Desktop notification for new mail", "Notificación del escritorio con correo nuevo"))
        .changed()
    {
        app.session.prefs.notify_new_mail = nm;
        app.session.save_prefs();
    }
    let mut today = app.session.prefs.show_today;
    if ui.checkbox(&mut today, pick("Show the Today panel", "Mostrar el panel Hoy")).changed() {
        app.session.prefs.show_today = today;
        app.session.save_prefs();
    }
    // UX-3: dónde va el contexto (Iurefficient, contacto).
    ui.horizontal(|ui| {
        ui.label(RichText::new(pick("Context panel", "Panel contextual")).color(c.text));
        let cur = app.session.prefs.context_layout;
        egui::ComboBox::from_id_salt("context-layout").selected_text(cur.label()).show_ui(ui, |ui| {
            for l in iuredesk_engine::ContextLayout::ALL {
                if ui.selectable_label(cur == l, l.label()).clicked() && cur != l {
                    app.session.prefs.context_layout = l;
                    app.session.save_prefs();
                }
            }
        });
    });
    muted(
        ui,
        pick(
            "Automatic: a column on wide windows, a compact strip under the header below 1300 px.",
            "Automático: columna en ventanas anchas y franja compacta bajo la cabecera por debajo de 1300 px.",
        ),
    );
    let mut collapsed = app.session.prefs.context_collapsed;
    if ui.checkbox(&mut collapsed, pick("Collapse the context column", "Plegar la columna de contexto")).changed() {
        app.session.prefs.context_collapsed = collapsed;
        app.session.save_prefs();
    }
    if send_prefs {
        app.session.apply_send_prefs();
    }
}
