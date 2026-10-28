//! Correo seguro e impresión en la interfaz (F5.4):
//!
//! - [`badge`]: insignia del panel de lectura («Firmado · Verificado», «Cifrado», «Verificación
//!   fallida»…) con «Detalles».
//! - [`compose_toggles`]: «Firmar» / «Cifrar» del redactor; «Cifrar» se enciende solo cuando
//!   todos los destinatarios tienen clave.
//! - [`command`]: `print.*` (imprimir / guardar como PDF un mensaje o el calendario) y
//!   `security.keys` (ventana de claves). Todo lo lento va en segundo plano.
//!
//! El estado vive en la memoria de egui (no ensucia `DeskApp`).

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use chrono::{Datelike, Duration, Local, NaiveDate, TimeZone};
use egui::{Context, RichText, Ui};
use iuredesk_engine::security::{self, SecurityReport, Tone};
use iuredesk_engine::{ComposeDraft, Notice};
use iuredesk_html_view::print::{self as pdf, Paper, PrintOptions};
use iurefficient_ui::{Icon, icons, pick, theme};

use crate::app::DeskApp;

/// Resultado de un trabajo de fondo.
enum Msg {
    Notice(Notice),
    /// Archivo elegido para importar (y si pide contraseña).
    Import(PathBuf),
}

#[derive(Default)]
struct State {
    keys_open: bool,
    rx: Option<Receiver<Msg>>,
    tx: Option<Sender<Msg>>,
    /// Importación pendiente de contraseña.
    import: Option<PathBuf>,
    password: String,
}

type Shared = Arc<Mutex<State>>;

fn shared(ctx: &Context) -> Shared {
    ctx.data_mut(|d| d.get_temp_or_insert_with(egui::Id::new("iuredesk-security-ui"), Shared::default).clone())
}

fn sender(st: &mut State) -> Sender<Msg> {
    if let Some(tx) = &st.tx {
        return tx.clone();
    }
    let (tx, rx) = channel();
    st.tx = Some(tx.clone());
    st.rx = Some(rx);
    tx
}

fn tone_color(ui: &Ui, t: Tone) -> egui::Color32 {
    let c = theme::colors(ui);
    match t {
        Tone::Good => c.success,
        Tone::Neutral => c.text_muted,
        Tone::Warning => c.warning,
        Tone::Bad => c.danger,
    }
}

/// Insignia de seguridad bajo la cabecera del mensaje.
pub fn badge(ui: &mut Ui, id: impl std::hash::Hash, report: Option<&SecurityReport>) {
    let Some(r) = report else { return };
    let es = iuredesk_engine::lang::spanish();
    let color = tone_color(ui, r.tone());
    let ic = match r.tone() {
        Tone::Good => Icon::Lock,
        Tone::Neutral => Icon::Info,
        Tone::Warning => Icon::Warning,
        Tone::Bad => Icon::Alert,
    };
    let key = egui::Id::new(("security-details", id));
    let mut open = ui.data(|d| d.get_temp::<bool>(key)).unwrap_or(false);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let side = theme::density(ui.ctx()).icon_size();
        let (rect, _) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
        icons::paint(ui.painter(), rect, ic, color, color);
        ui.label(RichText::new(r.title(es)).color(color).strong());
        ui.label(RichText::new(r.technology.label()).color(theme::colors(ui).text_muted).small());
        let label = if open { pick("Hide details", "Ocultar detalles") } else { pick("Details", "Detalles") };
        if ui.link(label).clicked() {
            open = !open;
        }
    });
    if open {
        ui.indent(key, |ui| {
            for line in r.details(es) {
                ui.add(egui::Label::new(RichText::new(line).color(theme::colors(ui).text)).wrap());
            }
        });
    }
    ui.data_mut(|d| d.insert_temp(key, open));
    ui.add_space(6.0);
}

/// Estado de los interruptores de un redactor.
#[derive(Default)]
pub struct ComposeSecurity {
    /// (remitente, destinatarios) → capacidades, para no recalcular en cada cuadro.
    cache: Option<(String, iuredesk_engine::iuredesk_crypto::Capabilities)>,
    /// El usuario tocó «Cifrar»: ya no se enciende solo.
    touched: bool,
}

/// «Firmar» / «Cifrar» en la barra inferior del redactor.
pub fn compose_toggles(ui: &mut Ui, cd: &mut ComposeDraft, st: &mut ComposeSecurity) {
    let from = cd.draft.identity.email.trim().to_ascii_lowercase();
    let rcpts: Vec<String> =
        cd.draft.to.iter().chain(&cd.draft.cc).chain(&cd.draft.bcc).map(|m| m.addr.trim().to_ascii_lowercase()).collect();
    let key = format!("{from}|{}", rcpts.join(","));
    if st.cache.as_ref().is_none_or(|(k, _)| *k != key) {
        st.cache = Some((key, security::capabilities(&from, &rcpts)));
    }
    let Some((_, caps)) = &st.cache else { return };
    let sec = &mut cd.draft.security;
    if !st.touched {
        sec.encrypt = caps.can_encrypt && (caps.all_mutual || sec.encrypt);
        if caps.can_encrypt && caps.all_mutual {
            sec.sign = true;
        }
    }
    if !caps.can_encrypt && sec.encrypt && !st.touched {
        sec.encrypt = false;
    }
    let sign_tip = if caps.can_sign {
        pick("Add a digital signature so recipients can check it's from you", "Agrega una firma digital para que comprueben que es tuyo")
    } else {
        pick("Create or import a key in Encryption keys… (Ctrl+K)", "Crea o importa una clave en «Claves de cifrado…» (Ctrl+K)")
    };
    let enc_tip = if caps.can_encrypt {
        pick("Only the recipients will be able to read it", "Solo los destinatarios podrán leerlo")
    } else if !caps.missing.is_empty() {
        pick("Some recipients have no key yet", "Algunos destinatarios aún no tienen clave")
    } else {
        pick("Add recipients with a known key to encrypt", "Agrega destinatarios con clave conocida para cifrar")
    };
    ui.add_enabled_ui(caps.can_sign, |ui| {
        ui.checkbox(&mut sec.sign, pick("Sign", "Firmar")).on_hover_text(sign_tip).on_disabled_hover_text(sign_tip);
    });
    let before = sec.encrypt;
    ui.add_enabled_ui(caps.can_encrypt || sec.encrypt, |ui| {
        ui.checkbox(&mut sec.encrypt, pick("Encrypt", "Cifrar")).on_hover_text(enc_tip).on_disabled_hover_text(enc_tip);
    });
    if before != sec.encrypt {
        st.touched = true;
    }
    if sec.encrypt && !caps.missing.is_empty() {
        let txt = format!("{} {}", pick("No key for:", "Sin clave:"), caps.missing.join(", "));
        ui.label(RichText::new(txt).color(theme::colors(ui).warning).small());
    }
}

// --- Impresión -----------------------------------------------------------------------------------

/// HTML imprimible del mensaje enfocado.
fn message_print(app: &mut DeskApp) -> Option<(String, String, PrintOptions)> {
    let id = app.session.focused()?;
    let env = app.session.envelope(id)?.clone();
    let body = app.session.body(id)?.clone();
    let list = |v: &[iuredesk_model::Mailbox]| {
        v.iter()
            .map(|m| if m.name.is_some() { format!("{} <{}>", m.display(), m.addr) } else { m.addr.clone() })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut headers = vec![
        (pick("From", "De").to_string(), list(&env.from)),
        (pick("To", "Para").to_string(), list(&env.to)),
    ];
    if !env.cc.is_empty() {
        headers.push((pick("Cc", "CC").to_string(), list(&env.cc)));
    }
    headers.push((pick("Date", "Fecha").to_string(), env.date.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string()));
    headers.push((pick("Subject", "Asunto").to_string(), env.subject.clone()));
    if let Some(r) = &body.security {
        headers.push((pick("Security", "Seguridad").to_string(), r.title(iuredesk_engine::lang::spanish())));
    }
    if !body.attachments.is_empty() {
        let names: Vec<String> = body.attachments.iter().map(|a| a.name.clone()).collect();
        headers.push((pick("Attachments", "Adjuntos").to_string(), names.join(", ")));
    }
    let html = pdf::message_html(&headers, body.html.as_deref(), &body.text);
    let opts = PrintOptions {
        paper: paper(),
        title: env.subject.clone(),
        inline_resources: body
            .inline
            .iter()
            .map(|(k, v)| (k.clone(), iuredesk_html_view::Bytes::from(v.clone())))
            .collect(),
        ..Default::default()
    };
    Some((html, file_stem(&env.subject), opts))
}

/// Carta en EE. UU., Canadá y México; A4 en el resto (por la configuración regional).
fn paper() -> Paper {
    let loc = std::env::var("LC_PAPER").or_else(|_| std::env::var("LANG")).unwrap_or_default();
    if ["_US", "_CA", "_MX", "_PH", "_CL", "_CO", "_VE"].iter().any(|r| loc.contains(r)) { Paper::Letter } else { Paper::A4 }
}

fn file_stem(s: &str) -> String {
    let clean: String = s.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { '_' }).collect();
    let t = clean.trim();
    if t.is_empty() { "mensaje".into() } else { t.chars().take(80).collect() }
}

/// HTML imprimible de la semana (o el mes) visible del calendario.
fn agenda_print(app: &mut DeskApp) -> (String, String, PrintOptions) {
    use iuredesk_engine::calendar::nav::CalView;
    let anchor = app.session.cal_anchor();
    let (start, days) = match app.session.cal_view() {
        CalView::Month => {
            let first = NaiveDate::from_ymd_opt(anchor.year(), anchor.month(), 1).unwrap_or(anchor);
            let next = if anchor.month() == 12 {
                NaiveDate::from_ymd_opt(anchor.year() + 1, 1, 1)
            } else {
                NaiveDate::from_ymd_opt(anchor.year(), anchor.month() + 1, 1)
            };
            (first, next.map_or(31, |n| (n - first).num_days()))
        }
        CalView::Day => (anchor, 1),
        _ => (anchor - Duration::days(i64::from(anchor.weekday().num_days_from_monday())), 7),
    };
    let to_utc = |d: NaiveDate| {
        Local
            .from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap_or_default())
            .earliest()
            .map(|t| t.with_timezone(&chrono::Utc))
            .unwrap_or_default()
    };
    let from = to_utc(start);
    let until = to_utc(start + Duration::days(days));
    let mut occ = app.session.occurrences(from, until).to_vec();
    occ.sort_by_key(|o| o.start);
    let es = iuredesk_engine::lang::spanish();
    let title = format!("{} {} – {}", pick("Calendar", "Calendario"), start.format("%Y-%m-%d"), (start + Duration::days(days - 1)).format("%Y-%m-%d"));
    let mut body = format!("<h2>{}</h2>", pdf::escape(&title));
    for i in 0..days {
        let day = start + Duration::days(i);
        let todays: Vec<_> = occ
            .iter()
            .filter(|o| {
                let s = o.start.with_timezone(&Local).date_naive();
                let e = o.end.with_timezone(&Local).date_naive();
                match o.days {
                    Some((a, b)) => a <= day && day <= b,
                    None => s <= day && (day < e || (s == day)),
                }
            })
            .collect();
        let weekday = if es {
            ["lunes", "martes", "miércoles", "jueves", "viernes", "sábado", "domingo"]
        } else {
            ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"]
        };
        let name = weekday.get(day.weekday().num_days_from_monday() as usize).copied().unwrap_or("");
        body.push_str(&format!(
            "<h3 style=\"margin:14px 0 4px;border-bottom:1px solid #bbb\">{name} {}</h3>",
            day.format("%Y-%m-%d")
        ));
        if todays.is_empty() {
            body.push_str(&format!("<p style=\"color:#777\">{}</p>", pick("No events", "Sin eventos")));
        }
        for o in todays {
            let when = if o.all_day {
                pick("All day", "Todo el día").to_string()
            } else {
                format!("{}–{}", o.start.with_timezone(&Local).format("%H:%M"), o.end.with_timezone(&Local).format("%H:%M"))
            };
            let loc = if o.location.trim().is_empty() { String::new() } else { format!(" · {}", pdf::escape(&o.location)) };
            body.push_str(&format!(
                "<p style=\"margin:2px 0\"><span style=\"display:inline-block;width:110px;color:#444\">{when}</span><b>{}</b>{loc}</p>",
                pdf::escape(&o.summary)
            ));
        }
    }
    let html = format!(
        "<html><head><meta charset=\"utf-8\"><style>body{{font-family:sans-serif;font-size:13px;color:#111}}</style></head><body>{body}</body></html>"
    );
    (html, file_stem(&title), PrintOptions { paper: paper(), title, ..Default::default() })
}

/// Imprime un PDF por el sistema: `lp` (CUPS) en Linux/BSD; si no está, o en Windows/macOS, se abre
/// con el visor del sistema para imprimir desde ahí.
fn print_pdf(path: &std::path::Path) -> Notice {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Ok(out) = std::process::Command::new("lp").arg("--").arg(path).output()
            && out.status.success()
        {
            return Notice::info("Sent to the printer.", "Enviado a la impresora.");
        }
    }
    match open::that_detached(path) {
        Ok(()) => Notice::info(
            "The PDF opened in your viewer: print it from there.",
            "El PDF se abrió en tu visor: imprímelo desde ahí.",
        ),
        Err(e) => Notice::error(iuredesk_engine::Text::new(
            format!("Couldn't print ({e})."),
            format!("No se pudo imprimir ({e})."),
        )),
    }
}

fn run_print(app: &mut DeskApp, ctx: &Context, job: Option<(String, String, PrintOptions)>, save: bool) {
    let Some((html, stem, opts)) = job else {
        app.notice(Notice::info(
            "Open the message first (its content is still loading).",
            "Abre primero el mensaje (su contenido aún se está cargando).",
        ));
        return;
    };
    let st = shared(ctx);
    let tx = st.lock().map(|mut s| sender(&mut s)).ok();
    let waker = app.session.waker();
    app.session.runtime().spawn_blocking(move || {
        let notice = match pdf::html_to_pdf(&html, &opts) {
            Err(e) => Notice::error(iuredesk_engine::Text::new(
                format!("The PDF couldn't be created ({e})."),
                format!("No se pudo crear el PDF ({e})."),
            )),
            Ok(bytes) if save => {
                let file = rfd::FileDialog::new()
                    .set_title(pick("Save as PDF", "Guardar como PDF"))
                    .set_file_name(format!("{stem}.pdf"))
                    .add_filter("PDF", &["pdf"])
                    .save_file();
                match file {
                    None => return,
                    Some(p) => match std::fs::write(&p, bytes) {
                        Ok(()) => Notice::info(format!("Saved {}", p.display()), format!("Guardado {}", p.display())),
                        Err(e) => Notice::error(iuredesk_engine::Text::new(
                            format!("Couldn't save the PDF ({e})."),
                            format!("No se pudo guardar el PDF ({e})."),
                        )),
                    },
                }
            }
            Ok(bytes) => {
                let p = std::env::temp_dir().join(format!("iuredesk-{}.pdf", stem.replace(' ', "_")));
                match std::fs::write(&p, bytes) {
                    Ok(()) => print_pdf(&p),
                    Err(e) => Notice::error(iuredesk_engine::Text::new(
                        format!("Couldn't prepare the print job ({e})."),
                        format!("No se pudo preparar la impresión ({e})."),
                    )),
                }
            }
        };
        if let Some(tx) = tx {
            let _ = tx.send(Msg::Notice(notice));
        }
        waker();
    });
}

/// Comandos `print.*` y `security.*`.
pub fn command(app: &mut DeskApp, ctx: &Context, id: &str) {
    match id {
        "print.message" | "print.message_pdf" => {
            let job = message_print(app);
            run_print(app, ctx, job, id.ends_with("_pdf"));
        }
        "print.agenda" | "print.agenda_pdf" => {
            let job = agenda_print(app);
            run_print(app, ctx, Some(job), id.ends_with("_pdf"));
        }
        "security.keys" => {
            if let Ok(mut s) = shared(ctx).lock() {
                s.keys_open = true;
            }
        }
        _ => {}
    }
}

// --- Ventana de claves ---------------------------------------------------------------------------

fn import_file(path: &std::path::Path, password: &str) -> Notice {
    let Some(c) = security::get() else { return Notice::info("Secure mail isn't available.", "El correo seguro no está disponible.") };
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            return Notice::error(iuredesk_engine::Text::new(format!("Couldn't read the file ({e})."), format!("No se pudo leer el archivo ({e}).")));
        }
    };
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let fail = |e: iuredesk_engine::iuredesk_crypto::CryptoError| Notice::error(iuredesk_engine::Text::new(e.message(false), e.message(true)));
    if ext == "p12" || ext == "pfx" {
        return match c.keys.import_p12(&data, password) {
            Ok(i) => Notice::info(format!("Certificate added: {}", i.label), format!("Certificado agregado: {}", i.label)),
            Err(e) => fail(e),
        };
    }
    let text = String::from_utf8_lossy(&data);
    if text.contains("PRIVATE KEY BLOCK") {
        return match c.keys.import_pgp_secret(&data, password) {
            Ok(f) => Notice::info(format!("Key added: {f}"), format!("Clave agregada: {f}")),
            Err(e) => fail(e),
        };
    }
    match c.keys.import_public(&data) {
        Ok(a) => Notice::info(format!("Key added for {}", a.join(", ")), format!("Clave agregada para {}", a.join(", "))),
        Err(e) => fail(e),
    }
}

fn needs_password(path: &std::path::Path) -> bool {
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    ext == "p12" || ext == "pfx" || std::fs::read(path).is_ok_and(|d| String::from_utf8_lossy(&d).contains("PRIVATE KEY BLOCK"))
}

/// Cada cuadro: resultados de fondo y la ventana de claves.
pub fn frame(app: &mut DeskApp, ctx: &Context) {
    let st = shared(ctx);
    let Ok(mut s) = st.lock() else { return };
    let mut msgs = Vec::new();
    if let Some(rx) = &s.rx {
        while let Ok(m) = rx.try_recv() {
            msgs.push(m);
        }
    }
    for m in msgs {
        match m {
            Msg::Notice(n) => app.notice(n),
            Msg::Import(p) => {
                if needs_password(&p) {
                    s.import = Some(p);
                    s.password.clear();
                } else {
                    let n = import_file(&p, "");
                    app.notice(n);
                }
            }
        }
    }
    if !s.keys_open {
        return;
    }
    let mut open = true;
    let mut actions: Vec<Box<dyn FnOnce(&mut DeskApp) + Send>> = Vec::new();
    let identities = app.session.identities();
    let import_tx = sender(&mut s);
    egui::Window::new(pick("Encryption keys", "Claves de cifrado"))
        .open(&mut open)
        .default_width(520.0)
        .collapsible(false)
        .show(ctx, |ui| {
            let Some(c) = security::get() else {
                ui.label(pick("Secure mail isn't available.", "El correo seguro no está disponible."));
                return;
            };
            let col = theme::colors(ui);
            ui.label(RichText::new(pick("Your keys", "Tus claves")).strong());
            let own = c.keys.own_keys();
            if own.is_empty() {
                ui.label(RichText::new(pick(
                    "You have no keys yet. Create one to sign and receive encrypted mail.",
                    "Aún no tienes claves. Crea una para firmar y recibir correo cifrado.",
                )).color(col.text_muted));
            }
            for k in &own {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(k.technology.label()).color(col.text_muted));
                    ui.label(&k.label);
                    ui.label(
                        RichText::new(iuredesk_engine::iuredesk_crypto::openpgp::pretty_fingerprint(k.fingerprint.get(..40).unwrap_or(&k.fingerprint)))
                            .monospace()
                            .small(),
                    );
                });
            }
            ui.add_space(6.0);
            for (_, ident) in &identities {
                let addr = ident.email.trim().to_ascii_lowercase();
                let has = own.iter().any(|k| k.addrs.contains(&addr));
                ui.horizontal(|ui| {
                    if !has {
                        if ui.button(format!("{} {addr}", pick("Create OpenPGP key for", "Crear clave OpenPGP para"))).clicked() {
                            let (name, a) = (ident.name.clone(), addr.clone());
                            actions.push(Box::new(move |app: &mut DeskApp| {
                                let n = match security::get().map(|c| c.keys.generate_pgp(&name, &a)) {
                                    Some(Ok(_)) => Notice::info(
                                        format!("Key created for {a}. It travels with your messages (Autocrypt)."),
                                        format!("Clave creada para {a}. Viaja con tus mensajes (Autocrypt)."),
                                    ),
                                    Some(Err(e)) => Notice::error(iuredesk_engine::Text::new(e.message(false), e.message(true))),
                                    None => return,
                                };
                                app.notice(n);
                            }));
                        }
                    } else if ui.button(format!("{} {addr}…", pick("Export public key of", "Exportar clave pública de"))).clicked() {
                        let a = addr.clone();
                        actions.push(Box::new(move |app: &mut DeskApp| {
                            let Some(armored) = security::get().and_then(|c| c.keys.export_pgp_public(&a).ok()) else {
                                return;
                            };
                            let waker = app.session.waker();
                            app.session.runtime().spawn_blocking(move || {
                                if let Some(p) = rfd::FileDialog::new().set_file_name(format!("{a}.asc")).save_file() {
                                    let _ = std::fs::write(p, armored);
                                }
                                waker();
                            });
                        }));
                    }
                });
            }
            ui.add_space(6.0);
            if ui.button(pick("Import key or certificate (.asc, .p12)…", "Importar clave o certificado (.asc, .p12)…")).clicked() {
                let ctx_tx = Some(import_tx.clone());
                actions.push(Box::new(move |app: &mut DeskApp| {
                    let waker = app.session.waker();
                    app.session.runtime().spawn_blocking(move || {
                        let p = rfd::FileDialog::new()
                            .set_title(pick("Import key", "Importar clave"))
                            .add_filter(pick("Keys and certificates", "Claves y certificados"), &["asc", "gpg", "pgp", "key", "p12", "pfx"])
                            .pick_file();
                        if let (Some(p), Some(tx)) = (p, ctx_tx) {
                            let _ = tx.send(Msg::Import(p));
                        }
                        waker();
                    });
                }));
            }
            ui.add_space(8.0);
            let peers = c.keys.peers();
            ui.label(RichText::new(format!("{} ({})", pick("Contacts with a key", "Contactos con clave"), peers.len())).strong());
            egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                for p in peers.iter().take(500) {
                    ui.horizontal(|ui| {
                        ui.label(&p.addr);
                        ui.label(
                            RichText::new(iuredesk_engine::iuredesk_crypto::openpgp::pretty_fingerprint(&p.fingerprint))
                                .monospace()
                                .small()
                                .color(col.text_muted),
                        );
                    });
                }
            });
        });
    if let Some(path) = s.import.clone() {
        let mut done = false;
        let mut cancel = false;
        egui::Window::new(pick("Password", "Contraseña")).collapsible(false).show(ctx, |ui| {
            ui.label(format!("{} {}", pick("Password of", "Contraseña de"), path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
            let r = ui.add(egui::TextEdit::singleline(&mut s.password).password(true));
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                done = true;
            }
            ui.horizontal(|ui| {
                done |= ui.button(pick("Import", "Importar")).clicked();
                cancel = ui.button(pick("Cancel", "Cancelar")).clicked();
            });
        });
        if done {
            let n = import_file(&path, &s.password);
            s.password.clear();
            s.import = None;
            app.notice(n);
        } else if cancel {
            s.password.clear();
            s.import = None;
        }
    }
    s.keys_open = open;
    drop(s);
    for a in actions {
        a(app);
    }
}
