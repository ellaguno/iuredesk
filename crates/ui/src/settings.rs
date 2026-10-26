//! Ajustes: tema, densidad, esquema de atajos, idioma, conversaciones y cuentas.

use egui::{Context, RichText};
use iuredesk_engine::{DensityPref, ShortcutScheme, ThemePref};
use iurefficient_ui::{lang, pick, section_label, segmented, theme};

use crate::app::{DeskApp, set_language};

pub fn show(app: &mut DeskApp, ctx: &Context) {
    if !app.settings_open {
        return;
    }
    let mut open = true;
    let mut run: Option<String> = None;
    egui::Window::new(pick("Settings", "Ajustes"))
        .id(egui::Id::unique("settings"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(460.0)
        .show(ctx, |ui| {
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
            if ui.checkbox(&mut mr, pick("Mark as read when opened", "Marcar como leído al abrir")).changed() {
                app.session.prefs.mark_read_on_open = mr;
                app.session.save_prefs();
            }
            let mut today = app.session.prefs.show_today;
            if ui.checkbox(&mut today, pick("Show the Today panel", "Mostrar el panel Hoy")).changed() {
                app.session.prefs.show_today = today;
                app.session.save_prefs();
            }
            ui.add_space(14.0);
            section_label(ui, pick("Accounts", "Cuentas"));
            for l in app.session.account_labels() {
                ui.label(RichText::new(format!("{}  <{}>", l.name, l.email)).color(c.text));
            }
            if ui.button(pick("Add account…", "Agregar cuenta…")).clicked() {
                run = Some("app.add_account".into());
            }
        });
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
