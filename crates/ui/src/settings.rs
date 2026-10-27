//! Ajustes: tema, densidad, esquema de atajos, idioma, conversaciones y cuentas.

use egui::{Context, RichText};
use iuredesk_engine::{DensityPref, ShortcutScheme, ThemePref};
use iurefficient_ui::{lang, pick, section_label, segmented, theme};

use crate::app::{DeskApp, set_language};

/// Lo que se está editando en «Cuentas» (firmas y confirmación de quitar).
#[derive(Default)]
pub struct AccountsUi {
    /// Firma en edición por dirección.
    signatures: std::collections::HashMap<String, String>,
    /// Cuenta pendiente de confirmar que se quita.
    confirm_remove: Option<String>,
}

pub fn show(app: &mut DeskApp, ctx: &Context) {
    if !app.settings_open {
        return;
    }
    let mut open = true;
    let mut run: Option<String> = None;
    let mut remove: Option<String> = None;
    let mut reauth: Option<String> = None;
    let mut save_sig: Option<(String, String)> = None;
    let mut send_prefs = false;
    let rows = app.session.accounts_overview();
    egui::Window::new(pick("Settings", "Ajustes"))
        .id(egui::Id::unique("settings"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(520.0)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().max_height(ctx.content_rect().height() - 140.0).show(ui, |ui| {
                let c = theme::colors(ui);
                section_label(ui, pick("Appearance", "Apariencia"));
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
                    run = Some(format!("view.theme.{}", t.key()));
                }
                ui.add_space(8.0);
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
                    run = Some(format!("view.density.{}", d.key()));
                }
                ui.add_space(14.0);
                section_label(ui, pick("Keyboard shortcuts", "Atajos de teclado"));
                ui.label(
                    RichText::new(pick("Use the keys you already know.", "Usa las teclas que ya conoces."))
                        .color(c.text_muted),
                );
                let mut s = app.session.prefs.scheme;
                if segmented(ui, &mut s, &ShortcutScheme::ALL.map(|s| (s, s.label()))).changed() {
                    run = Some(format!("view.scheme.{}", s.key()));
                }
                ui.add_space(14.0);
                section_label(ui, pick("Language", "Idioma"));
                let mut l = lang::lang();
                if segmented(ui, &mut l, &[(lang::Lang::Es, "Español"), (lang::Lang::En, "English")]).changed() {
                    set_language(l);
                    app.session.prefs.lang = Some(if l == lang::Lang::En { "en".into() } else { "es".into() });
                    app.session.save_prefs();
                }
                ui.add_space(14.0);
                section_label(ui, pick("Mail", "Correo"));
                let mut conv = app.session.prefs.conversations;
                if ui.checkbox(&mut conv, pick("Group by conversation", "Agrupar por conversación")).changed() {
                    app.session.set_conversations(conv);
                }
                let mut mr = app.session.prefs.mark_read_on_open;
                ui.horizontal(|ui| {
                    if ui
                        .checkbox(&mut mr, pick("Mark as read when opened, after", "Marcar como leído al abrir, tras"))
                        .changed()
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
                    .checkbox(
                        &mut nm,
                        pick("Desktop notification for new mail", "Notificación del escritorio con correo nuevo"),
                    )
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
                ui.add_space(14.0);
                crate::iure::settings_section(app, ui);
                section_label(ui, pick("Accounts", "Cuentas"));
                for r in &rows {
                    egui::Frame::new()
                        .fill(c.surface_alt)
                        .stroke(egui::Stroke::new(1.0, c.separator))
                        .corner_radius(8)
                        .inner_margin(egui::Margin::same(10))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(RichText::new(format!("{}  <{}>", r.name, r.email)).strong().color(c.text));
                            let mut detail = r.server.clone();
                            if let Some(st) = &r.status {
                                if !detail.is_empty() {
                                    detail.push_str(" · ");
                                }
                                detail.push_str(&st.label());
                            }
                            if !detail.is_empty() {
                                let bad = matches!(r.status, Some(iuredesk_engine::AccountSync::Error { .. }));
                                ui.add(
                                    egui::Label::new(RichText::new(detail).color(if bad {
                                        c.danger
                                    } else {
                                        c.text_muted
                                    }))
                                    .wrap(),
                                );
                            }
                            ui.add_space(4.0);
                            ui.label(RichText::new(pick("Signature", "Firma")).color(c.text_muted));
                            let text = app
                                .settings_ui
                                .signatures
                                .entry(r.email.clone())
                                .or_insert_with(|| r.signature.clone());
                            let resp = ui.add(
                                egui::TextEdit::multiline(text)
                                    .desired_rows(2)
                                    .desired_width(f32::INFINITY)
                                    .hint_text(pick("Name, title, phone…", "Nombre, cargo, teléfono…")),
                            );
                            if resp.lost_focus() && *text != r.signature {
                                save_sig = Some((r.email.clone(), text.clone()));
                            }
                            if !r.id.is_empty() {
                                ui.horizontal(|ui| {
                                    if ui.button(pick("Sign in again…", "Volver a iniciar sesión…")).clicked() {
                                        reauth = Some(r.id.clone());
                                    }
                                    if app.settings_ui.confirm_remove.as_deref() == Some(r.id.as_str()) {
                                        ui.label(
                                            RichText::new(pick(
                                                "Remove it from this computer?",
                                                "¿Quitarla de este equipo?",
                                            ))
                                            .color(c.danger),
                                        );
                                        if ui.button(pick("Yes, remove", "Sí, quitar")).clicked() {
                                            remove = Some(r.id.clone());
                                        }
                                        if ui.button(pick("Cancel", "Cancelar")).clicked() {
                                            app.settings_ui.confirm_remove = None;
                                        }
                                    } else if ui.button(pick("Remove…", "Quitar…")).clicked() {
                                        app.settings_ui.confirm_remove = Some(r.id.clone());
                                    }
                                });
                            }
                        });
                    ui.add_space(6.0);
                }
                if ui.button(pick("Add account…", "Agregar cuenta…")).clicked() {
                    run = Some("app.add_account".into());
                }
            });
        });
    if let Some((email, text)) = save_sig {
        app.session.set_signature_text(&email, &text);
    }
    if send_prefs {
        app.session.apply_send_prefs();
    }
    if let Some(id) = remove {
        app.settings_ui.confirm_remove = None;
        match app.session.remove_account(&id) {
            Ok(out) => app.outcome(ctx, out),
            Err(e) => app.notice(iuredesk_engine::Notice::error(e.text())),
        }
    }
    if let Some(id) = reauth {
        app.settings_open = false;
        app.session.reauth_account(&id);
    }
    if !open {
        app.settings_open = false;
    }
    if let Some(id) = run {
        if id == "app.add_account" {
            app.settings_open = false;
        }
        app.run(ctx, &id);
    }
}
