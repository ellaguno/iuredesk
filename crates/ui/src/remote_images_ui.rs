//! UX-5: imágenes remotas en la interfaz: el aviso del panel de lectura («Mostrar», «Mostrar
//! siempre de este remitente», «…de este dominio») y Ajustes → Privacidad (modo y lista de
//! permitidos). La decisión y las descargas son del motor (`iuredesk_engine::remote_images`).

use egui::{RichText, Ui};
use iuredesk_engine::Notice;
use iuredesk_engine::remote_images::RemoteImages;
use iuredesk_model::MessageId;
use iurefficient_ui::{Icon, icons, pick, section_label, theme};

use crate::app::DeskApp;

/// Aviso de imágenes bloqueadas (o la carga automática si Ajustes lo permite). `blocked` son las
/// URL remotas que el render no pidió.
pub(crate) fn banner(app: &mut DeskApp, ui: &mut Ui, id: MessageId, html: &str, blocked: Vec<String>) {
    if app.html.remote_ready(id) {
        let n = app.html.trackers_blocked(id);
        if n > 0 {
            let c = theme::colors(ui);
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                icons::paint(ui.painter(), r, Icon::Lock, c.text_muted, c.text_muted);
                ui.label(
                    RichText::new(format!("{n} {}", pick("tracking images blocked", "imágenes de rastreo bloqueadas")))
                        .size(theme::density(ui.ctx()).small_size())
                        .color(c.text_muted),
                );
            });
            ui.add_space(4.0);
        }
        return;
    }
    if blocked.is_empty() {
        return;
    }
    let loading = app.html.remote_loading(id);
    if !loading && app.session.remote_images_auto(id) {
        app.html.allow_remote(&mut app.session, id, blocked, html);
        return;
    }
    let sender = app.session.image_sender(id);
    let c = theme::colors(ui);
    let tone = c.info;
    let fill = theme::blend(c.surface, tone.gamma_multiply(if c.dark { 0.16 } else { 0.09 }));
    let mut show = false;
    let mut always_sender = false;
    let mut always_domain = false;
    let mut settings = false;
    let text = if loading {
        pick("Downloading remote images…", "Descargando imágenes remotas…")
    } else {
        pick(
            "Remote images blocked to protect your privacy.",
            "Imágenes remotas bloqueadas para proteger tu privacidad.",
        )
    };
    let inner = egui::Frame::new()
        .fill(fill)
        .stroke(egui::Stroke::new(1.0, theme::blend(c.surface, tone.gamma_multiply(0.35))))
        .corner_radius(8)
        .inner_margin(egui::Margin { left: 12, right: 8, top: 8, bottom: 8 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                let (r, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
                icons::paint(ui.painter(), r, Icon::Lock, tone, tone);
                ui.add(egui::Label::new(RichText::new(text).color(c.text)).wrap());
            });
            if loading {
                return;
            }
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let body = theme::density(ui.ctx()).body_size();
                let font = theme::medium_font(ui.ctx(), body);
                if ui.button(RichText::new(pick("Show", "Mostrar")).font(font).color(tone)).clicked() {
                    show = true;
                }
                if let Some((addr, domain)) = &sender {
                    let r = ui
                        .button(RichText::new(pick("Always from this sender", "Mostrar siempre de este remitente")))
                        .on_hover_text(addr.as_str());
                    if r.clicked() {
                        always_sender = true;
                    }
                    if let Some(d) = domain {
                        let r = ui
                            .button(RichText::new(format!("{} {d}", pick("Always from", "Mostrar siempre de"))))
                            .on_hover_text(pick(
                                "Always show images from this domain",
                                "Mostrar siempre de este dominio",
                            ));
                        if r.clicked() {
                            always_domain = true;
                        }
                    }
                }
                if icons::icon_button(ui, Icon::Settings, pick("Privacy settings", "Ajustes de privacidad"), true)
                    .clicked()
                {
                    settings = true;
                }
            });
        });
    iurefficient_ui::a11y::label(&inner.response, &format!("{}: {text}", pick("Information", "Información")));
    ui.add_space(8.0);
    if settings {
        app.open_settings(crate::settings::Section::Privacy, None, None);
    }
    let done = if always_sender {
        sender.as_ref().and_then(|(a, _)| app.session.allow_images_sender(a))
    } else if always_domain {
        sender.as_ref().and_then(|(_, d)| d.as_deref()).and_then(|d| app.session.allow_images_domain(d))
    } else {
        None
    };
    if let Some(t) = done {
        app.notice(Notice::success(t.en, t.es));
        show = true;
    }
    if show {
        app.html.allow_remote(&mut app.session, id, blocked, html);
    }
}

/// Ajustes → Privacidad.
pub(crate) fn settings_section(app: &mut DeskApp, ui: &mut Ui) {
    let c = theme::colors(ui);
    let small = theme::density(ui.ctx()).small_size();
    section_label(ui, pick("Remote images", "Imágenes remotas"));
    let cur = app.session.prefs.remote_images;
    for m in RemoteImages::ALL {
        if ui.radio(cur == m, m.label()).clicked() && cur != m {
            app.session.set_remote_images(m);
        }
    }
    ui.add(
        egui::Label::new(
            RichText::new(pick(
                "Images loaded from the internet can tell the sender that you opened the message, when and \
                 from where (tracking pixels). Even with «Always», IureDesk never loads known tracking pixels \
                 (1×1 images and a short list of tracking services). Images from the senders and domains below \
                 are always shown.",
                "Las imágenes que se cargan de internet pueden avisar al remitente de que abriste el correo, \
                 cuándo y desde dónde (píxeles de rastreo). Aun con «Siempre», IureDesk nunca carga los píxeles \
                 de rastreo conocidos (imágenes de 1×1 y una lista corta de servicios de rastreo). Las imágenes \
                 de los remitentes y dominios de abajo se muestran siempre.",
            ))
            .size(small)
            .color(c.text_muted),
        )
        .wrap(),
    );
    ui.add_space(10.0);
    let senders = app.session.prefs.image_senders.clone();
    let domains = app.session.prefs.image_domains.clone();
    section_label(ui, pick("Always show images from", "Mostrar siempre las imágenes de"));
    if senders.is_empty() && domains.is_empty() {
        ui.label(
            RichText::new(pick(
                "Nobody yet. Use «Always from this sender» in a message.",
                "Nadie todavía. Usa «Mostrar siempre de este remitente» en un correo.",
            ))
            .size(small)
            .color(c.text_muted),
        );
    }
    let mut remove_sender = None;
    let mut remove_domain = None;
    for (list, is_domain) in [(&senders, false), (&domains, true)] {
        for v in list {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                let ic = if is_domain { Icon::Hyperlink } else { Icon::Person };
                icons::paint(ui.painter(), r, ic, c.text_muted, c.text_muted);
                ui.label(RichText::new(v).color(c.text));
                if is_domain {
                    ui.label(RichText::new(pick("(domain)", "(dominio)")).size(small).color(c.text_muted));
                }
                if icons::icon_button(ui, Icon::Close, pick("Remove", "Quitar"), true).clicked() {
                    if is_domain {
                        remove_domain = Some(v.clone());
                    } else {
                        remove_sender = Some(v.clone());
                    }
                }
            });
        }
    }
    // Agregar a mano (remitente o dominio).
    let id = ui.scope_id().with("image-allow-new");
    let mut draft: String = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    ui.horizontal(|ui| {
        let r = ui.add(
            egui::TextEdit::singleline(&mut draft)
                .hint_text(pick("name@example.com or example.com", "nombre@ejemplo.com o ejemplo.com"))
                .desired_width(260.0),
        );
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (ui.button(pick("Add", "Agregar")).clicked() || enter) && !draft.trim().is_empty() {
            let v = draft.trim().to_string();
            let ok = if v.contains('@') {
                app.session.allow_images_sender(&v).is_some()
            } else {
                app.session.allow_images_domain(&v).is_some()
            };
            if ok {
                draft.clear();
            } else {
                app.notice(Notice::error(iuredesk_engine::Text::new(
                    "That doesn't look like an address or a domain.",
                    "Eso no parece una dirección ni un dominio.",
                )));
            }
        }
    });
    ui.data_mut(|d| d.insert_temp(id, draft));
    if let Some(v) = remove_sender {
        app.session.remove_image_sender(&v);
    }
    if let Some(v) = remove_domain {
        app.session.remove_image_domain(&v);
    }
}
