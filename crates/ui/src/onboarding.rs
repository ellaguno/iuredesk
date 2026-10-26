//! Bienvenida y asistente de alta: correo → proveedor detectado → un solo botón.

use egui::{Align, Context, Layout, RichText, Stroke, Ui, vec2};
use iuredesk_engine::onboarding::{Flow, WizardStep};
use iuredesk_model::account::OAuthProvider;
use iurefficient_ui::{Banner, Icon, Severity, icons, pick, primary_button, theme};

use crate::app::DeskApp;

/// Pantalla completa de bienvenida (no hay cuentas).
pub fn page(app: &mut DeskApp, ui: &mut Ui) {
    let c = theme::colors(ui);
    egui::CentralPanel::default().frame(egui::Frame::new().fill(c.surface_alt)).show(ui, |ui| {
        let w = 460.0_f32.min(ui.available_width() - 32.0);
        let top = ((ui.available_height() - 520.0) / 2.0).max(24.0);
        ui.vertical_centered(|ui| {
            ui.add_space(top);
            ui.label(RichText::new("IureDesk").font(theme::bold_font(ui.ctx(), 28.0)).color(c.brand));
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
            ui.add_space(12.0);
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
            ui.label(RichText::new(pick("Add your email account", "Agrega tu cuenta de correo")).font(theme::bold_font(&ctx, 17.0)).color(c.text));
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
        }
        WizardStep::Detecting => {
            let domain = app.session.wizard.as_ref().map(|w| w.email.rsplit_once('@').map_or(String::new(), |(_, d)| d.to_string())).unwrap_or_default();
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(16.0));
                ui.label(RichText::new(format!("{} {domain}…", pick("Looking up the settings for", "Buscando la configuración de"))).color(c.text));
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
            ui.add_space(8.0);
            if ui.button(pick("Cancel", "Cancelar")).clicked() {
                app.session.wizard_back();
            }
        }
        WizardStep::Done => {
            let email = app.session.wizard.as_ref().and_then(|w| w.saved.as_ref().map(|a| a.email.clone())).unwrap_or_default();
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(22.0, 22.0), egui::Sense::hover());
                ui.painter().circle_filled(r.center(), 11.0, c.success);
                icons::paint(ui.painter(), r.shrink(5.0), Icon::Check, c.on_accent, c.on_accent);
                ui.label(RichText::new(format!("{email} {}", pick("is ready", "está lista"))).font(theme::bold_font(&ctx, 16.0)).color(c.text));
            });
            ui.add_space(6.0);
            ui.label(
                RichText::new(pick(
                    "Your mail will start syncing when sync arrives (F1). Meanwhile, everything is ready.",
                    "Tu correo empezará a sincronizarse cuando llegue la sincronización (F1). Mientras, todo está listo.",
                ))
                .color(c.text_muted),
            );
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
    let name = w.provider_name();
    let server = w.server_summary().unwrap_or_default();
    let email = w.email.clone();
    let flow = w.flow.clone();
    let oauth_ready = w.oauth_ready;
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
            if oauth_ready {
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
                Banner::new(
                    Severity::Info,
                    &format!(
                        "{} {who} {}",
                        pick("Signing in with", "El inicio de sesión con"),
                        pick(
                            "is not available yet in this build. You can use an app password instead.",
                            "aún no está disponible en esta versión. Mientras, puedes usar una contraseña de app.",
                        )
                    ),
                )
                .show(ui);
                ui.add_space(10.0);
                if primary_button(ui, pick("Use an app password", "Usar una contraseña de app")).clicked() {
                    app.session.wizard_use_app_password();
                }
            }
        }
        Some(Flow::AppPassword { help_url, steps }) => {
            ui.label(RichText::new(pick("This provider needs an app password", "Este proveedor pide una contraseña de app")).strong().color(c.text));
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
    if ui.button(pick("Use another address", "Usar otra dirección")).clicked() {
        app.session.wizard_back();
    }
}

fn password_and_test(app: &mut DeskApp, ui: &mut Ui, label: &str) {
    let c = theme::colors(ui);
    let mut go = false;
    if let Some(w) = app.session.wizard.as_mut() {
        ui.label(RichText::new(label).color(c.text));
        let r = ui.add(egui::TextEdit::singleline(&mut w.password).password(true).hint_text(label).desired_width(f32::INFINITY));
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
