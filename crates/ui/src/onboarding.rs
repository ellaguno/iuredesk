//! Bienvenida y asistente de alta: correo → proveedor detectado → un solo botón.

use egui::{Align, Context, Layout, RichText, Stroke, Ui, vec2};
use iuredesk_engine::onboarding::{Flow, WizardStep};
use iuredesk_model::account::{OAuthProvider, Protocol};
use iurefficient_ui::{Banner, Icon, Severity, icons, pick, primary_button, theme};

use crate::app::DeskApp;

/// Pantalla completa de bienvenida (no hay cuentas).
pub fn page(app: &mut DeskApp, ui: &mut Ui) {
    let c = theme::colors(ui);
    egui::CentralPanel::default().frame(egui::Frame::new().fill(c.surface_alt)).show(ui, |ui| {
        let w = 460.0_f32.min(ui.available_width() - 32.0);
        // El formulario manual es alto: arriba, para que quepa casi entero.
        let tall = app.session.wizard.as_ref().is_some_and(|w| w.step == WizardStep::Manual);
        let top = if tall { 24.0 } else { ((ui.available_height() - 520.0) / 2.0).max(24.0) };
        ui.vertical_centered(|ui| {
            ui.add_space(top);
            ui.label(
                RichText::new("IureDesk").font(theme::bold_font(ui.ctx(), 28.0)).color(if ui.visuals().dark_mode {
                    c.accent
                } else {
                    c.brand
                }),
            );
            ui.add_space(4.0);
            ui.label(
                RichText::new(pick(
                    "Mail, calendar, contacts, notes and tasks — all in one place.",
                    "Correo, calendario, contactos, notas y tareas — todo en un lugar.",
                ))
                .color(c.text_muted),
            );
            ui.add_space(18.0);
            card(ui, w, |ui| body(app, ui));
            ui.add_space(8.0);
            crate::import_ui::onboarding_offer(app, ui);
            ui.add_space(4.0);
            ui.label(
                RichText::new(pick(
                    "Just exploring? Start IureDesk with --sample to see a demo mailbox.",
                    "¿Solo quieres ver? Abre IureDesk con --sample para usar un buzón de ejemplo.",
                ))
                .color(c.text_muted)
                .size(theme::density(ui.ctx()).small_size()),
            );
        });
    });
}

/// Ventana del asistente sobre la aplicación (agregar otra cuenta).
pub fn modal(app: &mut DeskApp, ctx: &Context) {
    let mut close = false;
    let resp = egui::Modal::new(egui::Id::unique("add-account")).show(ctx, |ui| {
        ui.set_width(440.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(pick("Add account", "Agregar cuenta")).font(theme::bold_font(ui.ctx(), 18.0)));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icons::icon_button(ui, Icon::Close, pick("Close", "Cerrar"), true).clicked() {
                    close = true;
                }
            });
        });
        ui.add_space(10.0);
        body(app, ui);
    });
    if resp.should_close() || close {
        app.session.wizard_close();
    }
}

fn card(ui: &mut Ui, width: f32, add: impl FnOnce(&mut Ui)) {
    let c = theme::colors(ui);
    egui::Frame::new()
        .fill(c.surface)
        .stroke(Stroke::new(1.0, c.separator))
        .corner_radius(12)
        .inner_margin(egui::Margin::same(24))
        .show(ui, |ui| {
            ui.set_width(width);
            ui.with_layout(Layout::top_down(Align::Min), add);
        });
}

fn body(app: &mut DeskApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let c = theme::colors(ui);
    let Some(step) = app.session.wizard.as_ref().map(|w| w.step) else { return };
    if let Some(n) = app.session.wizard.as_ref().and_then(|w| w.message.clone()) {
        let sev = if n.kind == iuredesk_engine::NoticeKind::Error { Severity::Error } else { Severity::Info };
        Banner::new(sev, n.text.get()).show(ui);
        ui.add_space(10.0);
    }
    match step {
        WizardStep::Email => {
            ui.label(
                RichText::new(pick("Add your email account", "Agrega tu cuenta de correo"))
                    .font(theme::bold_font(&ctx, 17.0))
                    .color(c.text),
            );
            ui.add_space(2.0);
            ui.label(
                RichText::new(pick(
                    "We'll find the right settings for you. Gmail, Outlook, iCloud and any IMAP provider work.",
                    "Buscamos la configuración por ti. Funcionan Gmail, Outlook, iCloud y cualquier proveedor IMAP.",
                ))
                .color(c.text_muted),
            );
            ui.add_space(12.0);
            let mut submit = false;
            if let Some(w) = app.session.wizard.as_mut() {
                ui.label(RichText::new(pick("Email address", "Correo electrónico")).color(c.text));
                let r = ui.add(
                    egui::TextEdit::singleline(&mut w.email)
                        .hint_text(pick("you@example.com", "tu@ejemplo.com"))
                        .desired_width(f32::INFINITY),
                );
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                ui.add_space(8.0);
                ui.label(RichText::new(pick("Your name (optional)", "Tu nombre (opcional)")).color(c.text));
                ui.add(
                    egui::TextEdit::singleline(&mut w.name)
                        .hint_text(pick("As people will see it", "Como lo verán los demás"))
                        .desired_width(f32::INFINITY),
                );
            }
            ui.add_space(16.0);
            let can = app.session.wizard.as_ref().is_some_and(|w| w.can_continue());
            ui.add_enabled_ui(can, |ui| {
                if primary_button(ui, pick("Continue", "Continuar")).clicked() {
                    submit = true;
                }
            });
            if submit && can {
                app.session.wizard_detect();
            }
            ui.add_space(6.0);
            ui.add_enabled_ui(can, |ui| {
                if ui.link(pick("Set up manually", "Configurar manualmente")).clicked()
                    && !app.session.wizard_check_existing()
                {
                    app.session.wizard_manual();
                }
            });
        }
        WizardStep::Manual => manual(app, ui),
        WizardStep::Exists => exists(app, ui),
        WizardStep::Detecting => {
            let domain = app
                .session
                .wizard
                .as_ref()
                .map(|w| w.email.rsplit_once('@').map_or(String::new(), |(_, d)| d.to_string()))
                .unwrap_or_default();
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(16.0));
                ui.label(
                    RichText::new(format!(
                        "{} {domain}…",
                        pick("Looking up the settings for", "Buscando la configuración de")
                    ))
                    .color(c.text),
                );
            });
            ui.add_space(8.0);
            if ui.button(pick("Back", "Atrás")).clicked() {
                app.session.wizard_back();
            }
        }
        WizardStep::Provider => provider(app, ui),
        WizardStep::Working => {
            let oauth = matches!(app.session.wizard.as_ref().and_then(|w| w.flow.clone()), Some(Flow::OAuth(_)));
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(16.0));
                ui.label(
                    RichText::new(if oauth {
                        pick("Finish signing in in your browser…", "Termina de iniciar sesión en tu navegador…")
                    } else {
                        pick("Testing the connection…", "Probando la conexión…")
                    })
                    .color(c.text),
                );
            });
            if oauth {
                ui.add_space(6.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(pick(
                            "If the page shows «Access blocked» or an error, close it and press Cancel: we'll show you other ways in.",
                            "Si la página muestra «Acceso bloqueado» o un error, ciérrala y pulsa Cancelar: te mostramos otras formas de entrar.",
                        ))
                        .color(c.text_muted)
                        .size(theme::density(&ctx).small_size()),
                    )
                    .wrap(),
                );
                if let Some(link) = app.session.wizard.as_ref().and_then(|w| w.oauth_link()) {
                    ui.horizontal(|ui| {
                        if ui.link(pick("Open the page again", "Volver a abrir la página")).clicked()
                            && !crate::open_url(&link)
                        {
                            ui.ctx().copy_text(link.clone());
                        }
                        if ui.link(pick("Copy the link", "Copiar el enlace")).clicked() {
                            ui.ctx().copy_text(link.clone());
                            app.notice(iuredesk_engine::Notice::info(
                                "Link copied: paste it in your browser.",
                                "Enlace copiado: pégalo en tu navegador.",
                            ));
                        }
                    });
                }
            }
            ui.add_space(8.0);
            if ui.button(pick("Cancel", "Cancelar")).clicked() {
                app.session.wizard_cancel_working();
            }
        }
        WizardStep::Done => {
            let email =
                app.session.wizard.as_ref().and_then(|w| w.saved.as_ref().map(|a| a.email.clone())).unwrap_or_default();
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(22.0, 22.0), egui::Sense::hover());
                ui.painter().circle_filled(r.center(), 11.0, c.success);
                icons::paint(ui.painter(), r.shrink(5.0), Icon::Check, c.on_accent, c.on_accent);
                ui.label(
                    RichText::new(format!("{email} {}", pick("is ready", "está lista")))
                        .font(theme::bold_font(&ctx, 16.0))
                        .color(c.text),
                );
            });
            ui.add_space(6.0);
            // Avance de la primera sincronización de la cuenta recién agregada.
            let status = app.session.wizard_account().and_then(|a| app.session.account_sync(a).cloned());
            match status {
                Some(iuredesk_engine::AccountSync::Ready { .. }) => {
                    ui.label(
                        RichText::new(pick("Your inbox is up to date.", "Tu bandeja está al día.")).color(c.text_muted),
                    );
                }
                Some(iuredesk_engine::AccountSync::Error { text, .. }) => {
                    ui.add(egui::Label::new(RichText::new(text.get()).color(c.danger)).wrap());
                }
                Some(iuredesk_engine::AccountSync::Offline(text)) => {
                    ui.add(egui::Label::new(RichText::new(text.get()).color(c.warning)).wrap());
                }
                Some(s) => {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(14.0));
                        let label = match s {
                            iuredesk_engine::AccountSync::Working { done, total, .. } if total > 0 => format!(
                                "{} {done}/{total}",
                                pick("Downloading recent mail…", "Descargando correos recientes…")
                            ),
                            _ => pick("Downloading recent mail…", "Descargando correos recientes…").to_string(),
                        };
                        ui.label(RichText::new(label).color(c.text_muted));
                    });
                }
                None => {
                    ui.label(
                        RichText::new(pick(
                            "You can start using it now; mail keeps arriving in the background.",
                            "Ya puedes usarla; el correo sigue llegando en segundo plano.",
                        ))
                        .color(c.text_muted),
                    );
                }
            }
            ui.add_space(14.0);
            if primary_button(ui, pick("Go to my inbox", "Ir a mi bandeja")).clicked() {
                app.session.wizard_close();
            }
        }
    }
}

fn provider(app: &mut DeskApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let c = theme::colors(ui);
    let Some(w) = app.session.wizard.as_ref() else { return };
    let (name, server) = match &w.reauth {
        Some(a) => (
            pick("Sign in again", "Volver a iniciar sesión").to_string(),
            format!("{}:{}", a.incoming.host, a.incoming.port),
        ),
        None => (w.provider_name(), w.server_summary().unwrap_or_default()),
    };
    let email = w.email.clone();
    let flow = w.flow.clone();
    let oauth_ready = w.oauth_ready;
    let issue = w.oauth_issue;
    let reauth = w.reauth.is_some();
    let notes: Vec<String> = w
        .discovery
        .as_ref()
        .map(|d| d.notes.iter().map(|n| n.text(iuredesk_engine::lang::spanish()).to_string()).collect())
        .unwrap_or_default();
    // Tarjeta del proveedor.
    egui::Frame::new()
        .fill(c.surface_alt)
        .stroke(Stroke::new(1.0, c.separator))
        .corner_radius(10)
        .inner_margin(egui::Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                iurefficient_ui::avatar(ui, &name, "", 40.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new(&name).font(theme::bold_font(&ctx, 16.0)).color(c.text));
                    ui.label(RichText::new(&email).color(c.text_muted));
                    if !server.is_empty() {
                        ui.label(RichText::new(&server).color(c.text_muted).size(theme::density(&ctx).small_size()));
                    }
                });
            });
        });
    for n in notes.iter().take(3) {
        ui.add_space(6.0);
        ui.label(RichText::new(n).color(c.text_muted).size(theme::density(&ctx).small_size()));
    }
    ui.add_space(14.0);
    match flow {
        Some(Flow::OAuth(p)) => {
            let who = match p {
                OAuthProvider::Google => "Google",
                OAuthProvider::Microsoft => "Microsoft",
            };
            if let Some(issue) = issue.filter(|_| oauth_ready) {
                // Falló el navegador: el mensaje ya explica el motivo; aquí, los siguientes pasos.
                ui.horizontal_wrapped(|ui| {
                    if issue.offers_retry() && primary_button(ui, pick("Try again", "Intentar de nuevo")).clicked() {
                        app.session.wizard_oauth();
                    }
                    if issue.offers_app_password()
                        && ui.button(pick("Use an app password", "Usar una contraseña de app")).clicked()
                    {
                        app.session.wizard_use_app_password();
                    }
                });
            } else if oauth_ready {
                if primary_button(ui, &format!("{} {who}", pick("Continue with", "Continuar con"))).clicked() {
                    app.session.wizard_oauth();
                }
                ui.add_space(6.0);
                ui.label(
                    RichText::new(pick(
                        "Your browser will open so you can sign in. IureDesk never sees your password.",
                        "Se abrirá tu navegador para que inicies sesión. IureDesk nunca ve tu contraseña.",
                    ))
                    .color(c.text_muted),
                );
            } else {
                Banner::new(Severity::Info, iuredesk_engine::onboarding::oauth_unavailable(p).get()).show(ui);
                ui.add_space(10.0);
                if primary_button(ui, pick("Use an app password", "Usar una contraseña de app")).clicked() {
                    app.session.wizard_use_app_password();
                }
            }
        }
        Some(Flow::AppPassword { help_url, steps }) => {
            ui.label(
                RichText::new(pick("This provider needs an app password", "Este proveedor pide una contraseña de app"))
                    .strong()
                    .color(c.text),
            );
            ui.add_space(4.0);
            ui.label(RichText::new(steps.get()).color(c.text_muted));
            if ui.link(pick("Open the page to create it", "Abrir la página para crearla")).clicked() {
                crate::open_url(help_url);
            }
            ui.add_space(8.0);
            password_and_test(app, ui, pick("App password", "Contraseña de app"));
        }
        Some(Flow::Password) => {
            password_and_test(app, ui, pick("Password", "Contraseña"));
        }
        None => {}
    }
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        if !reauth && ui.button(pick("Use another address", "Usar otra dirección")).clicked() {
            app.session.wizard_back();
        }
        if !reauth && ui.link(pick("Set up manually", "Configurar manualmente")).clicked() {
            app.session.wizard_manual();
        }
    });
}

/// «Esta cuenta ya está en IureDesk»: volver a iniciar sesión, ir a sus ajustes u otra dirección.
fn exists(app: &mut DeskApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let c = theme::colors(ui);
    let Some(w) = app.session.wizard.as_ref() else { return };
    let email = w.email.trim().to_string();
    let id = w.existing.clone();
    ui.label(
        RichText::new(pick("This account is already in IureDesk", "Esta cuenta ya está en IureDesk"))
            .font(theme::bold_font(&ctx, 17.0))
            .color(c.text),
    );
    ui.add_space(4.0);
    ui.add(
        egui::Label::new(
            RichText::new(format!(
                "{email} {}",
                pick(
                    "was added before. If it stopped working, sign in again; to change its servers or name, open its settings.",
                    "ya se había agregado. Si dejó de funcionar, vuelve a iniciar sesión; para cambiar sus servidores o su nombre, abre sus ajustes.",
                )
            ))
            .color(c.text_muted),
        )
        .wrap(),
    );
    ui.add_space(14.0);
    let mut reauth = false;
    let mut settings = false;
    ui.horizontal_wrapped(|ui| {
        if primary_button(ui, pick("Sign in again", "Volver a iniciar sesión")).clicked() {
            reauth = true;
        }
        if ui.button(pick("Go to Settings → Accounts", "Ir a Ajustes → Cuentas")).clicked() {
            settings = true;
        }
    });
    ui.add_space(8.0);
    if ui.button(pick("Use another address", "Usar otra dirección")).clicked() {
        app.session.wizard_back();
    }
    let Some(id) = id else { return };
    if reauth {
        app.session.reauth_account(&id);
    } else if settings {
        app.session.wizard_close();
        app.settings_open = true;
        app.settings_ui.section = crate::settings::Section::Accounts;
        crate::settings::open_editor(app, &id);
    }
}

/// «Configurar manualmente»: tipo de cuenta, servidores, contraseña(s) y «Probar conexión».
fn manual(app: &mut DeskApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let c = theme::colors(ui);
    let small = theme::density(&ctx).small_size();
    let mut test = false;
    let mut back = false;
    let Some(w) = app.session.wizard.as_mut() else { return };
    let email = w.email.trim().to_string();
    let Some(m) = w.manual.as_mut() else { return };
    let busy = m.testing();
    ui.label(
        RichText::new(pick("Set up manually", "Configurar manualmente"))
            .font(theme::bold_font(&ctx, 17.0))
            .color(c.text),
    );
    ui.label(RichText::new(&email).color(c.text_muted));
    if let Some(h) = &m.hint {
        ui.add_space(2.0);
        ui.add(egui::Label::new(RichText::new(h.get()).color(c.text_muted).size(small)).wrap());
    }
    ui.add_space(8.0);
    // Lo que quede de alto menos la contraseña y los botones, que siempre se ven.
    let max_h = (ui.available_height() - 200.0).clamp(140.0, 560.0);
    egui::ScrollArea::vertical().max_height(max_h).auto_shrink([false, true]).id_salt("wizard-manual").show(ui, |ui| {
        ui.add_enabled_ui(!busy, |ui| {
            ui.horizontal(|ui| {
                ui.label(pick("Account type", "Tipo de cuenta"));
                let mut p = m.incoming.protocol;
                iurefficient_ui::segmented(ui, &mut p, &[(Protocol::Imap, "IMAP"), (Protocol::Pop3, "POP3")]);
                m.set_protocol(p);
            });
            ui.add_space(6.0);
            let proto = if m.incoming.protocol == Protocol::Pop3 { "POP3" } else { "IMAP" };
            iurefficient_ui::section_label(ui, &format!("{} ({proto})", pick("Incoming mail", "Correo entrante")));
            if crate::server_form::server_fields(ui, "wiz-in", &mut m.incoming) {
                m.incoming_test = None;
            }
            crate::server_form::probe_line(ui, m.incoming_test.as_ref());
            ui.add_space(6.0);
            iurefficient_ui::section_label(ui, &format!("{} (SMTP)", pick("Outgoing mail", "Correo saliente")));
            if crate::server_form::server_fields(ui, "wiz-out", &mut m.outgoing) {
                m.outgoing_test = None;
            }
            ui.checkbox(&mut m.same_password, pick("Use the same password", "Usar la misma contraseña"));
            if !m.same_password {
                crate::widgets::secret_field(
                    ui,
                    &mut m.outgoing_password,
                    "wizard-out-password",
                    pick("Outgoing password", "Contraseña de salida"),
                    260.0,
                );
            }
            crate::server_form::probe_line(ui, m.outgoing_test.as_ref());
            ui.add_space(6.0);
            egui::CollapsingHeader::new(pick("Calendar and contacts (optional)", "Calendario y contactos (opcional)"))
                .id_salt("wiz-dav")
                .default_open(!m.caldav_url.is_empty() || !m.carddav_url.is_empty())
                .show(ui, |ui| {
                    egui::Grid::new("wiz-dav-grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                        for (label, url) in [
                            (pick("CalDAV address", "Dirección CalDAV"), &mut m.caldav_url),
                            (pick("CardDAV address", "Dirección CardDAV"), &mut m.carddav_url),
                        ] {
                            let l = ui.label(label);
                            ui.add(egui::TextEdit::singleline(url).desired_width(260.0).hint_text("https://…"))
                                .labelled_by(l.id);
                            ui.end_row();
                        }
                    });
                });
        });
    });
    ui.add_space(8.0);
    ui.label(RichText::new(pick("Password", "Contraseña")).color(c.text));
    let r = crate::widgets::secret_field(
        ui,
        &mut w.password,
        "wizard-password",
        pick("Password", "Contraseña"),
        f32::INFINITY,
    );
    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
        test = true;
    }
    let problem = w.manual.as_ref().and_then(|m| m.problem());
    if let Some(p) = &problem {
        ui.label(RichText::new(p.get()).color(c.warning).size(small));
    }
    ui.add_space(10.0);
    let can = w.can_continue();
    ui.horizontal(|ui| {
        if ui.button(pick("Back", "Atrás")).clicked() {
            back = true;
        }
        ui.add_enabled_ui(can, |ui| {
            if primary_button(ui, pick("Test connection", "Probar conexión")).clicked() {
                test = true;
            }
        });
        if busy {
            ui.spinner();
        }
    });
    if back {
        app.session.wizard_manual_back();
    } else if test && can {
        app.session.wizard_manual_test();
    }
}

fn password_and_test(app: &mut DeskApp, ui: &mut Ui, label: &str) {
    let c = theme::colors(ui);
    let mut go = false;
    if let Some(w) = app.session.wizard.as_mut() {
        ui.label(RichText::new(label).color(c.text));
        let r = crate::widgets::secret_field(ui, &mut w.password, "wizard-password", label, f32::INFINITY);
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !w.password.is_empty() {
            go = true;
        }
    }
    ui.add_space(10.0);
    let can = app.session.wizard.as_ref().is_some_and(|w| w.can_continue());
    ui.add_enabled_ui(can, |ui| {
        if primary_button(ui, pick("Test connection", "Probar conexión")).clicked() {
            go = true;
        }
    });
    if go && can {
        app.session.wizard_test_and_save();
    }
}
