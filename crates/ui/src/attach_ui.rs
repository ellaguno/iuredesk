//! UX-3: adjuntos del panel de lectura. Clic = abrir con la app del sistema (el adjunto se escribe
//! antes en la caché privada del usuario); menú: abrir, abrir con…, mostrar en la carpeta, guardar
//! como…, guardar todos, copiar imagen y las acciones de la suite. Los tipos que ejecutan código
//! piden confirmación; nunca se abre nada solo. Miniaturas para las imágenes. Lo lento (extraer,
//! escribir, decodificar, diálogos del sistema) corre en el runtime de fondo.
//!
//! Arrastrar un adjunto al escritorio no se ofrece: winit/egui solo reciben archivos soltados,
//! no inician un arrastre hacia fuera.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use egui::{ColorImage, Context, RichText, Sense, Stroke, TextureHandle, TextureOptions, Ui, vec2};
use iuredesk_engine::attachments::{self as att, AttachError};
use iuredesk_engine::background::Waker;
use iuredesk_engine::{Notice, Session, Text};
use iuredesk_model::MessageId;
use iurefficient_ui::{Icon, icons, pick, theme};

use crate::app::DeskApp;
use crate::widgets::human_size;

/// Adjuntos más grandes no se decodifican para la miniatura.
const MAX_THUMB_SOURCE: usize = 20 * 1024 * 1024;
/// Caja de la miniatura (puntos).
const THUMB_BOX: [u32; 2] = [200, 130];
/// Lado máximo de una imagen que se decodifica (bomba de descompresión).
const MAX_DECODE_SIDE: u32 = 12_000;
/// Lado máximo de la imagen copiada al portapapeles.
const MAX_COPY_SIDE: u32 = 4_096;
/// Cuánto se espera la descarga bajo demanda del mensaje.
const BODY_WAIT: Duration = Duration::from_secs(180);
/// Miniaturas recordadas.
const MAX_THUMBS: usize = 64;

/// Qué hacer con el adjunto.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum How {
    Open,
    OpenWith,
    Reveal,
    SaveAs,
    CopyImage,
}

/// Una petición sobre un adjunto (`index` en el orden de la lista del cuerpo).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Req {
    pub msg: MessageId,
    pub index: usize,
    pub name: String,
    pub mime: String,
    pub how: How,
    /// El usuario ya confirmó abrir un tipo peligroso.
    pub confirmed: bool,
}

/// Lo que vuelve del fondo.
enum Done {
    Ready(Req, PathBuf),
    Saved(Req, PathBuf),
    SavedAll(Req, PathBuf, usize),
    NeedsBody(Req),
    Failed(Req, Text),
    Thumb((MessageId, usize), Option<ColorImage>),
    Image(Req, ColorImage),
    Idle(Req),
}

enum Thumb {
    Loading,
    Ready(TextureHandle),
    None,
}

/// Lanza trabajo bloqueante en el runtime de fondo de la sesión.
type Spawner = std::sync::Arc<dyn Fn(Box<dyn FnOnce() + Send + 'static>) + Send + Sync>;

/// Estado de los adjuntos.
pub(crate) struct AttachUi {
    tx: Sender<Done>,
    rx: Receiver<Done>,
    spawner: Spawner,
    waker: Waker,
    store: std::sync::Arc<iuredesk_store::Store>,
    root: PathBuf,
    /// Peticiones que esperan a que baje el mensaje (con el momento y si ya se reintentó).
    waiting: Vec<(Req, Instant)>,
    /// Ya se reintentaron tras llegar el cuerpo (si vuelve a faltar, se avisa y no se insiste).
    retried: HashSet<(MessageId, usize)>,
    /// Adjuntos en curso (indicador en el chip).
    busy: HashSet<(MessageId, usize)>,
    /// Confirmación pendiente de un tipo peligroso.
    pub(crate) confirm: Option<Req>,
    thumbs: HashMap<(MessageId, usize), Thumb>,
    thumb_order: Vec<(MessageId, usize)>,
}

impl AttachUi {
    pub(crate) fn new(session: &Session) -> Self {
        let (tx, rx) = channel();
        let rt = session.runtime();
        let ui = AttachUi {
            tx,
            rx,
            spawner: std::sync::Arc::new(move |f| {
                rt.spawn_blocking(f);
            }),
            waker: session.waker(),
            store: std::sync::Arc::clone(session.store()),
            root: att::cache_root(),
            waiting: Vec::new(),
            retried: HashSet::new(),
            busy: HashSet::new(),
            confirm: None,
            thumbs: HashMap::new(),
            thumb_order: Vec::new(),
        };
        // Limpieza de lo abierto hace más de una semana.
        let root = ui.root.clone();
        (ui.spawner)(Box::new(move || {
            att::prune(&root, att::CACHE_MAX_AGE, std::time::SystemTime::now());
        }));
        ui
    }

    /// Carpeta de la caché (pruebas: una temporal).
    #[doc(hidden)]
    pub fn set_cache_root(&mut self, root: PathBuf) {
        self.root = root;
    }

    pub(crate) fn modal(&self) -> bool {
        self.confirm.is_some()
    }

    /// ¿Trabajo en curso? (capturas y pruebas esperan a que termine).
    pub(crate) fn is_busy(&self) -> bool {
        !self.busy.is_empty() || self.thumbs.values().any(|t| matches!(t, Thumb::Loading))
    }

    /// Terminó (bien o mal) lo pedido sobre ese adjunto.
    fn finish(&mut self, req: &Req) {
        self.busy.remove(&(req.msg, req.index));
        self.retried.remove(&(req.msg, req.index));
    }

    fn spawn(&self, f: impl FnOnce() -> Done + Send + 'static) {
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        (self.spawner)(Box::new(move || {
            let _ = tx.send(f());
            waker();
        }));
    }

    /// Pide la acción (sin confirmar si es peligrosa: la confirmación la pone [`run`]).
    fn start(&mut self, req: Req) {
        self.busy.insert((req.msg, req.index));
        let store = std::sync::Arc::clone(&self.store);
        let root = self.root.clone();
        match req.how {
            How::Open | How::OpenWith | How::Reveal => {
                self.spawn(move || match att::export_to_cache(&store, &root, req.msg, req.index) {
                    Ok(p) => Done::Ready(req, p),
                    Err(AttachError::NotDownloaded) => Done::NeedsBody(req),
                    Err(e) => Done::Failed(req, e.text()),
                })
            }
            How::SaveAs => self.spawn(move || {
                let a = match att::extract_from_store(&store, req.msg, req.index) {
                    Ok(a) => a,
                    Err(AttachError::NotDownloaded) => return Done::NeedsBody(req),
                    Err(e) => return Done::Failed(req, e.text()),
                };
                let Some(path) = rfd::FileDialog::new()
                    .set_title(pick("Save attachment", "Guardar adjunto"))
                    .set_file_name(att::sanitize_filename(&a.name))
                    .save_file()
                else {
                    return Done::Idle(req);
                };
                match std::fs::write(&path, &a.bytes) {
                    Ok(()) => Done::Saved(req, path),
                    Err(e) => Done::Failed(req, AttachError::Io(e.to_string()).text()),
                }
            }),
            How::CopyImage => self.spawn(move || match att::extract_from_store(&store, req.msg, req.index) {
                Ok(a) => match decode(&a.bytes, MAX_COPY_SIDE, MAX_COPY_SIDE) {
                    Some(img) => Done::Image(req, img),
                    None => Done::Failed(req, Text::new("Could not read that image.", "No se pudo leer esa imagen.")),
                },
                Err(AttachError::NotDownloaded) => Done::NeedsBody(req),
                Err(e) => Done::Failed(req, e.text()),
            }),
        }
    }

    /// Miniatura de una imagen (la pide si falta).
    fn thumb(&mut self, msg: MessageId, index: usize) -> Option<&TextureHandle> {
        let key = (msg, index);
        if !self.thumbs.contains_key(&key) {
            if self.thumb_order.len() >= MAX_THUMBS && !self.thumb_order.is_empty() {
                let old = self.thumb_order.remove(0);
                self.thumbs.remove(&old);
            }
            self.thumbs.insert(key, Thumb::Loading);
            self.thumb_order.push(key);
            let store = std::sync::Arc::clone(&self.store);
            self.spawn(move || {
                let img = att::extract_from_store(&store, msg, index)
                    .ok()
                    .filter(|a| a.bytes.len() <= MAX_THUMB_SOURCE)
                    .and_then(|a| decode(&a.bytes, THUMB_BOX[0] * 2, THUMB_BOX[1] * 2));
                Done::Thumb(key, img)
            });
        }
        match self.thumbs.get(&key) {
            Some(Thumb::Ready(t)) => Some(t),
            _ => None,
        }
    }
}

/// Decodifica una imagen hostil con límites y la reduce a `max_w`×`max_h`.
fn decode(bytes: &[u8], max_w: u32, max_h: u32) -> Option<ColorImage> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DECODE_SIDE);
    limits.max_image_height = Some(MAX_DECODE_SIDE);
    limits.max_alloc = Some(512 * 1024 * 1024);
    reader.limits(limits);
    let img = reader.decode().ok()?;
    let img = if img.width() > max_w || img.height() > max_h { img.thumbnail(max_w, max_h) } else { img };
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Some(ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()))
}

/// Pide una acción; los tipos peligrosos pasan antes por la confirmación.
pub(crate) fn run(app: &mut DeskApp, req: Req) {
    if matches!(req.how, How::Open | How::OpenWith) && !req.confirmed && att::is_dangerous(&req.name, &req.mime) {
        app.attach.confirm = Some(req);
        return;
    }
    app.attach.start(req);
}

/// «Guardar todos»: elige carpeta y escribe cada adjunto con un nombre libre.
pub(crate) fn save_all(app: &mut DeskApp, msg: MessageId) {
    let store = std::sync::Arc::clone(&app.attach.store);
    let req =
        Req { msg, index: usize::MAX, name: String::new(), mime: String::new(), how: How::SaveAs, confirmed: true };
    app.attach.busy.insert((msg, usize::MAX));
    app.attach.spawn(move || {
        let raw = match store.get_raw(msg) {
            Ok(r) => r,
            Err(_) => return Done::Failed(req, AttachError::NotDownloaded.text()),
        };
        let all = att::extract_all(&raw, 500);
        let Some(dir) = rfd::FileDialog::new()
            .set_title(pick("Save all attachments in…", "Guardar todos los adjuntos en…"))
            .pick_folder()
        else {
            return Done::Idle(req);
        };
        let mut n = 0usize;
        for a in &all {
            if std::fs::write(att::unique_in(&dir, &a.name), &a.bytes).is_ok() {
                n += 1;
            }
        }
        Done::SavedAll(req, dir, n)
    });
}

/// Cada cuadro: resultados del fondo, descargas bajo demanda y la confirmación.
pub(crate) fn frame(app: &mut DeskApp, ctx: &Context) {
    while let Ok(d) = app.attach.rx.try_recv() {
        match d {
            Done::Ready(req, path) => {
                app.attach.finish(&req);
                if !launch(&path, req.how) {
                    app.notice(Notice::error(Text::new(
                        "No app on this computer could open the attachment. Use «Save as…».",
                        "Ninguna aplicación del equipo pudo abrir el adjunto. Usa «Guardar como…».",
                    )));
                }
            }
            Done::Saved(req, p) => {
                app.attach.finish(&req);
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                app.notice(Notice::success(format!("Saved: {name}"), format!("Guardado: {name}")));
            }
            Done::SavedAll(req, dir, n) => {
                app.attach.finish(&req);
                let d = dir.display();
                app.notice(Notice::success(
                    format!("{n} attachments saved in {d}"),
                    format!("{n} adjuntos guardados en {d}"),
                ));
            }
            Done::NeedsBody(req) => {
                if app.attach.retried.remove(&(req.msg, req.index)) {
                    // Ya llegó el cuerpo y aun así no está el original: no se insiste.
                    app.attach.finish(&req);
                    app.notice(Notice::error(AttachError::NotDownloaded.text()));
                } else if !app.attach.waiting.iter().any(|(r, _)| *r == req) {
                    // Bajo demanda: se pide el mensaje y se reintenta cuando llegue.
                    app.session.request_body(req.msg);
                    app.notice(Notice::info(
                        "Downloading the message to open the attachment…",
                        "Descargando el mensaje para abrir el adjunto…",
                    ));
                    app.attach.waiting.push((req, Instant::now()));
                    if app.attach.waiting.len() > 32 {
                        let (old, _) = app.attach.waiting.remove(0);
                        app.attach.finish(&old);
                    }
                }
            }
            Done::Failed(req, t) => {
                app.attach.finish(&req);
                app.notice(Notice::error(t));
            }
            Done::Thumb(key, img) => {
                let t = match img {
                    Some(img) => Thumb::Ready(ctx.load_texture(
                        format!("att-thumb-{}-{}", key.0.get(), key.1),
                        img,
                        TextureOptions::LINEAR,
                    )),
                    None => Thumb::None,
                };
                if app.attach.thumbs.contains_key(&key) {
                    app.attach.thumbs.insert(key, t);
                }
            }
            Done::Image(req, img) => {
                app.attach.finish(&req);
                ctx.copy_image(img);
                app.notice(Notice::success("Image copied.", "Imagen copiada."));
            }
            Done::Idle(req) => app.attach.finish(&req),
        }
    }
    // Mensajes que estaban en el servidor: cuando llega el cuerpo se reintenta (una vez).
    for (req, since) in std::mem::take(&mut app.attach.waiting) {
        if since.elapsed() > BODY_WAIT {
            app.attach.finish(&req);
            app.notice(Notice::error(AttachError::NotDownloaded.text()));
        } else if app.session.body(req.msg).is_some() {
            app.attach.retried.insert((req.msg, req.index));
            app.attach.start(req);
        } else {
            app.attach.waiting.push((req, since));
        }
    }
    confirm_dialog(app, ctx);
}

fn confirm_dialog(app: &mut DeskApp, ctx: &Context) {
    let Some(req) = app.attach.confirm.clone() else { return };
    let mut answer: Option<Option<How>> = None;
    let modal = egui::Modal::new(egui::Id::unique("attach-danger")).show(ctx, |ui| {
        ui.set_max_width(420.0);
        let c = theme::colors(ui);
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
            icons::paint(ui.painter(), r, Icon::Warning, c.danger, c.danger);
            ui.label(
                RichText::new(pick(
                    "Open a file that can run programs?",
                    "¿Abrir un archivo que puede ejecutar programas?",
                ))
                .font(theme::bold_font(ui.ctx(), 15.0))
                .color(c.text),
            );
        });
        ui.add_space(8.0);
        let name = att::sanitize_filename(&req.name);
        ui.add(
            egui::Label::new(
                RichText::new(format!(
                    "{} «{name}» {}",
                    pick("The attachment", "El adjunto"),
                    pick(
                        "is a type that can run code on your computer (a program, script or document with \
                         macros). Open it only if you expected it and trust the sender.",
                        "es de un tipo que puede ejecutar código en tu equipo (un programa, un script o un \
                         documento con macros). Ábrelo solo si lo esperabas y confías en quien lo envió.",
                    )
                ))
                .color(c.text),
            )
            .wrap(),
        );
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui.button(pick("Cancel", "Cancelar")).clicked() {
                answer = Some(None);
            }
            if ui.button(pick("Save instead…", "Guardar en su lugar…")).clicked() {
                answer = Some(Some(How::SaveAs));
            }
            let open = ui.add(
                egui::Button::new(RichText::new(pick("Open anyway", "Abrir de todos modos")).color(c.on_accent))
                    .fill(c.danger),
            );
            if open.clicked() {
                answer = Some(Some(req.how));
            }
        });
    });
    if modal.should_close() && answer.is_none() {
        answer = Some(None);
    }
    if let Some(a) = answer {
        app.attach.confirm = None;
        if let Some(how) = a {
            app.attach.start(Req { how, confirmed: true, ..req });
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Abrir con el sistema

/// Abre el archivo según `how`. Nunca bloquea. `false` si no se pudo lanzar.
fn launch(path: &Path, how: How) -> bool {
    let tag = match how {
        How::Open => "open",
        How::OpenWith => "open-with",
        How::Reveal => "reveal",
        _ => return true,
    };
    if crate::capture(format!("{tag}:{}", path.display())) {
        return true;
    }
    match how {
        How::OpenWith => open_with(path),
        How::Reveal => reveal(path),
        _ => open::that_detached(path).is_ok(),
    }
}

/// «Abrir con…»: el selector de aplicaciones del sistema.
fn open_with(path: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        // Portal XDG (OpenURI.OpenFile con ask=true): el escritorio pregunta con qué abrirlo.
        let p = path.to_path_buf();
        std::thread::spawn(move || {
            if !portal_open_with(&p) {
                let _ = reveal(&p);
            }
        });
        true
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("rundll32.exe").arg("shell32.dll,OpenAs_RunDLL").arg(path).spawn().is_ok()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        // macOS: sin selector programático sencillo; se muestra en Finder (clic derecho → Abrir con).
        reveal(path)
    }
}

#[cfg(target_os = "linux")]
fn portal_open_with(path: &Path) -> bool {
    use std::os::fd::AsFd;
    let Ok(file) = std::fs::File::open(path) else { return false };
    let Ok(conn) = zbus::blocking::Connection::session() else { return false };
    let mut opts: HashMap<&str, zbus::zvariant::Value<'_>> = HashMap::new();
    opts.insert("ask", zbus::zvariant::Value::from(true));
    conn.call_method(
        Some("org.freedesktop.portal.Desktop"),
        "/org/freedesktop/portal/desktop",
        Some("org.freedesktop.portal.OpenURI"),
        "OpenFile",
        &("", zbus::zvariant::Fd::from(file.as_fd()), opts),
    )
    .is_ok()
}

/// URI `file://` con los bytes fuera de lo seguro escapados.
#[cfg(target_os = "linux")]
fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut s = String::from("file://");
    for b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~') {
            s.push(char::from(*b));
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

/// «Mostrar en la carpeta» (con el archivo seleccionado si el sistema lo permite).
fn reveal(path: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        let p = path.to_path_buf();
        std::thread::spawn(move || {
            let shown = zbus::blocking::Connection::session().is_ok_and(|c| {
                c.call_method(
                    Some("org.freedesktop.FileManager1"),
                    "/org/freedesktop/FileManager1",
                    Some("org.freedesktop.FileManager1"),
                    "ShowItems",
                    &(vec![file_uri(&p)], ""),
                )
                .is_ok()
            });
            if !shown && let Some(dir) = p.parent() {
                let _ = open::that_detached(dir);
            }
        });
        true
    }
    #[cfg(target_os = "windows")]
    {
        let mut arg = std::ffi::OsString::from("/select,");
        arg.push(path.as_os_str());
        std::process::Command::new("explorer.exe").arg(arg).spawn().is_ok()
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg("-R").arg(path).spawn().is_ok()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        path.parent().is_some_and(|d| open::that_detached(d).is_ok())
    }
}

// ---------------------------------------------------------------------------------------------
// Chips

/// Un adjunto para mostrar: nombre, tamaño y tipo declarados.
pub(crate) struct Item {
    pub name: String,
    pub size: usize,
    pub mime: String,
}

/// Los adjuntos de un mensaje: miniaturas para las imágenes (si el mensaje ya está en el equipo),
/// chips para lo demás y «Guardar todos».
pub(crate) fn show(app: &mut DeskApp, ui: &mut Ui, msg: MessageId, items: &[Item], downloaded: bool) {
    if items.is_empty() {
        return;
    }
    let mut act: Option<Req> = None;
    let mut all = false;
    let mut suite: Option<iuredesk_engine::suite::SuiteAction> = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
        for (i, it) in items.iter().enumerate().take(200) {
            let busy = app.attach.busy.contains(&(msg, i));
            let image = att::is_image(&it.name, &it.mime);
            let thumb = if image && downloaded && it.size <= MAX_THUMB_SOURCE {
                app.attach.thumb(msg, i).cloned()
            } else {
                None
            };
            let r = match thumb {
                Some(t) => thumb_card(ui, &t, it, busy),
                None => chip(ui, it, busy),
            };
            let req =
                |how: How| Req { msg, index: i, name: it.name.clone(), mime: it.mime.clone(), how, confirmed: false };
            if r.clicked() {
                act = Some(req(How::Open));
            }
            r.context_menu(|ui| {
                if icons::menu_item(ui, Some(Icon::Open), pick("Open", "Abrir")).clicked() {
                    act = Some(req(How::Open));
                    ui.close();
                }
                if !cfg!(target_os = "macos")
                    && icons::menu_item(ui, Some(Icon::Open), pick("Open with…", "Abrir con…")).clicked()
                {
                    act = Some(req(How::OpenWith));
                    ui.close();
                }
                let reveal_label = if cfg!(target_os = "macos") {
                    pick("Show in Finder", "Mostrar en Finder")
                } else {
                    pick("Show in folder", "Mostrar en la carpeta")
                };
                if icons::menu_item(ui, Some(Icon::Folder), reveal_label).clicked() {
                    act = Some(req(How::Reveal));
                    ui.close();
                }
                if icons::menu_item(ui, Some(Icon::SaveAs), pick("Save as…", "Guardar como…")).clicked() {
                    act = Some(req(How::SaveAs));
                    ui.close();
                }
                if items.len() > 1
                    && icons::menu_item(ui, Some(Icon::Save), pick("Save all…", "Guardar todos…")).clicked()
                {
                    all = true;
                    ui.close();
                }
                if image && icons::menu_item(ui, Some(Icon::Export), pick("Copy image", "Copiar imagen")).clicked() {
                    act = Some(req(How::CopyImage));
                    ui.close();
                }
                ui.separator();
                crate::suite_ui::attachment_items(ui, msg, &it.name, &mut suite);
            });
        }
        if items.len() > 1 {
            let r = ui
                .add(egui::Button::new(
                    RichText::new(pick("Save all…", "Guardar todos…")).size(theme::density(ui.ctx()).small_size()),
                ))
                .on_hover_text(pick("Save every attachment in a folder", "Guardar todos los adjuntos en una carpeta"));
            if r.clicked() {
                all = true;
            }
        }
    });
    if let Some(r) = act {
        run(app, r);
    }
    if all {
        save_all(app, msg);
    }
    if let Some(a) = suite {
        app.suite.hub.run(a);
    }
}

fn tip() -> &'static str {
    pick("Click to open · right-click for more", "Clic para abrir · clic derecho para más opciones")
}

fn chip(ui: &mut Ui, it: &Item, busy: bool) -> egui::Response {
    let c = theme::colors(ui);
    let danger = att::is_dangerous(&it.name, &it.mime);
    let r = egui::Frame::new()
        .fill(c.surface_alt)
        .stroke(Stroke::new(1.0, if danger { c.warning } else { c.separator }))
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(8, 5))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if busy {
                    ui.add(egui::Spinner::new().size(14.0));
                } else {
                    let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                    let ic = if danger { Icon::Warning } else { Icon::Attachment };
                    icons::paint(ui.painter(), r, ic, if danger { c.warning } else { c.text_muted }, c.accent);
                }
                ui.add(egui::Label::new(RichText::new(&it.name).color(c.text)).truncate().selectable(false));
                ui.label(
                    RichText::new(human_size(it.size)).color(c.text_muted).size(theme::density(ui.ctx()).small_size()),
                );
            });
        })
        .response
        .interact(Sense::click());
    if r.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    iurefficient_ui::a11y::button(&r, &format!("{} {}", pick("Attachment", "Adjunto"), it.name));
    r.on_hover_text(tip())
}

fn thumb_card(ui: &mut Ui, t: &TextureHandle, it: &Item, busy: bool) -> egui::Response {
    let c = theme::colors(ui);
    let [w, h] = t.size();
    let scale = (THUMB_BOX[0] as f32 / w.max(1) as f32).min(THUMB_BOX[1] as f32 / h.max(1) as f32).min(1.0);
    let size = vec2(w as f32 * scale, h as f32 * scale);
    let r = egui::Frame::new()
        .fill(c.surface_alt)
        .stroke(Stroke::new(1.0, c.separator))
        .corner_radius(6)
        .inner_margin(egui::Margin::same(6))
        .show(ui, |ui| {
            ui.set_width(THUMB_BOX[0] as f32);
            ui.vertical(|ui| {
                ui.vertical_centered(|ui| {
                    ui.add(egui::Image::new((t.id(), size)).corner_radius(4));
                });
                ui.horizontal(|ui| {
                    if busy {
                        ui.add(egui::Spinner::new().size(12.0));
                    }
                    ui.add(egui::Label::new(RichText::new(&it.name).color(c.text)).truncate().selectable(false));
                });
                ui.label(
                    RichText::new(human_size(it.size)).color(c.text_muted).size(theme::density(ui.ctx()).small_size()),
                );
            });
        })
        .response
        .interact(Sense::click());
    if r.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    iurefficient_ui::a11y::button(&r, &format!("{} {}", pick("Attachment", "Adjunto"), it.name));
    r.on_hover_text(tip())
}
