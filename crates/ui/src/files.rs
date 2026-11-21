//! S1.5: «Archivos»: las unidades de IureDav dentro de IureDesk, sin abrir IureDav.
//!
//! - A la izquierda, las unidades (perfiles de IureDav) con su estado: montada o no, edición o
//!   solo lectura. Una unidad sin montar invita a abrir IureDav.
//! - En el centro, la carpeta actual: ruta pulsable, filtro por nombre, carpetas primero.
//!   Doble clic (o Intro) entra a una carpeta o abre un archivo con el programa del sistema.
//! - Acciones del archivo elegido: abrir, mostrar en su carpeta, enviar por correo (adjunto en un
//!   correo nuevo) y copiar la ruta.
//!
//! El disco se lee en otro hilo (una unidad de red puede tardar) y todo está acotado: entradas
//! por carpeta y profundidad. Nunca se sale de la carpeta de la unidad (sin `..` ni enlaces que
//! lleven fuera).

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::SystemTime;

use egui::{Align, Layout, RichText, Sense, Ui, vec2};
use iuredesk_engine::iuredesk_integrations::{DavMount, SisterApp};
use iuredesk_engine::suite::SuiteAction;
use iuredesk_engine::{Notice, Text};
use iurefficient_ui::{Banner, Icon, PaneKind, Severity, icons, pick, split, theme};

use crate::app::DeskApp;

/// Entradas por carpeta como mucho.
pub const MAX_ENTRIES: usize = 5_000;
/// Profundidad máxima de navegación dentro de una unidad.
const MAX_DEPTH: usize = 64;

/// Una entrada de la carpeta.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

/// Estado de la pantalla.
#[derive(Default)]
pub struct FilesState {
    /// Unidad elegida (su `id`).
    mount: Option<String>,
    /// Carpeta actual, relativa a la unidad (componentes normales).
    rel: Vec<String>,
    entries: Vec<Entry>,
    /// La carpeta tenía más de [`MAX_ENTRIES`].
    truncated: bool,
    error: Option<Text>,
    filter: String,
    selected: Option<String>,
    /// Lectura en curso: para qué carpeta y su resultado.
    loading: Option<(PathBuf, Receiver<Listing>)>,
    /// Carpeta que se tiene leída.
    shown: Option<PathBuf>,
    /// Las unidades de IureDav, leídas en otro hilo (mirar si una unidad de red está montada puede
    /// tardar segundos: en el hilo de la interfaz congelaba IureDesk), cuándo y la lectura en curso.
    mounts: Vec<DavMount>,
    mounts_at: Option<std::time::Instant>,
    mounts_rx: Option<Receiver<Vec<DavMount>>>,
}

/// Cada cuánto se vuelven a leer las unidades.
const MOUNTS_EVERY: std::time::Duration = std::time::Duration::from_secs(5);

type Listing = Result<(Vec<Entry>, bool), String>;

/// Lee una carpeta (bloqueante; en otro hilo): carpetas primero, luego por nombre sin mayúsculas.
pub fn read_dir(dir: &Path) -> Listing {
    let mut out = Vec::new();
    let mut truncated = false;
    for e in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        if out.len() >= MAX_ENTRIES {
            truncated = true;
            break;
        }
        let Ok(e) = e else { continue };
        let name = e.file_name().to_string_lossy().into_owned();
        // Ocultos fuera (como hacen los exploradores por omisión).
        if name.starts_with('.') {
            continue;
        }
        let meta = e.metadata().ok();
        let dir = e.file_type().is_ok_and(|t| t.is_dir());
        out.push(Entry {
            name,
            dir,
            size: meta.as_ref().map_or(0, |m| m.len()),
            modified: meta.and_then(|m| m.modified().ok()),
        });
    }
    out.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok((out, truncated))
}

/// `root` + `rel` sin tocar el disco (sin `..`, `.`, separadores ni demasiada profundidad). Lo que
/// mira el disco ([`inside`]) va en el hilo que lee la carpeta.
fn joined(root: &Path, rel: &[String]) -> Option<PathBuf> {
    if rel.len() > MAX_DEPTH || rel.iter().any(|c| c.is_empty() || c == "." || c == ".." || c.contains(['/', '\\'])) {
        return None;
    }
    let mut p = root.to_path_buf();
    for c in rel {
        p.push(c);
    }
    Some(p)
}

/// La carpeta `rel` dentro de `root`, solo si de verdad queda dentro (sin `..`, sin enlaces que
/// lleven fuera).
pub fn inside(root: &Path, rel: &[String]) -> Option<PathBuf> {
    let p = joined(root, rel)?;
    let real_root = root.canonicalize().ok()?;
    let real = p.canonicalize().ok()?;
    real.starts_with(&real_root).then_some(p)
}

/// Tamaño legible («12 KB»).
fn human_size(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i + 1 < UNITS.len() {
        v /= 1024.0;
        i += 1;
    }
    let unit = UNITS.get(i).copied().unwrap_or("B");
    if i == 0 { format!("{n} {unit}") } else { format!("{v:.1} {unit}") }
}

fn mounts(app: &mut DeskApp, ctx: &egui::Context) -> Vec<DavMount> {
    let st = &mut app.files;
    if let Some(rx) = &st.mounts_rx {
        match rx.try_recv() {
            Ok(m) => {
                st.mounts = m;
                st.mounts_rx = None;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => st.mounts_rx = None,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
            }
        }
    }
    let stale = st.mounts_at.is_none_or(|t| t.elapsed() >= MOUNTS_EVERY);
    if stale
        && st.mounts_rx.is_none()
        && let Some(read) = app.suite.hub.suite().map(|s| s.dav_mounts_reader())
    {
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new().name("files-mounts".into()).spawn(move || {
            let _ = tx.send(read());
        });
        if spawned.is_ok() {
            st.mounts_rx = Some(rx);
        }
        st.mounts_at = Some(std::time::Instant::now());
    }
    st.mounts.clone()
}

/// Pide leer la carpeta actual si cambió.
fn ensure_listing(app: &mut DeskApp, ctx: &egui::Context, root: &Path) {
    let st = &mut app.files;
    // Sin tocar el disco en este hilo: con una unidad de red, cada consulta puede tardar segundos.
    let Some(dir) = joined(root, &st.rel) else {
        st.rel.clear();
        return;
    };
    if st.shown.as_deref() == Some(dir.as_path()) || st.loading.as_ref().is_some_and(|(d, _)| *d == dir) {
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let (root, rel) = (root.to_path_buf(), st.rel.clone());
    let ctx = ctx.clone();
    let spawned = std::thread::Builder::new().name("files-list".into()).spawn(move || {
        // Que de verdad quede dentro de la unidad (un enlace no saca de ella), y luego leerla.
        let listing = match inside(&root, &rel) {
            Some(target) => read_dir(&target),
            None => Err("fuera de la unidad".into()),
        };
        let _ = tx.send(listing);
        ctx.request_repaint();
    });
    if spawned.is_ok() {
        st.loading = Some((dir, rx));
    }
}

fn poll_listing(app: &mut DeskApp) {
    let st = &mut app.files;
    let Some((dir, rx)) = &st.loading else { return };
    let Ok(r) = rx.try_recv() else { return };
    let dir = dir.clone();
    st.loading = None;
    st.shown = Some(dir);
    match r {
        Ok((entries, truncated)) => {
            st.entries = entries;
            st.truncated = truncated;
            st.error = None;
        }
        Err(e) => {
            st.entries.clear();
            st.error = Some(Text::new(
                format!("This folder can't be read right now ({e})."),
                format!("No se puede leer esta carpeta ahora ({e})."),
            ));
        }
    }
}

/// Qué pidió el usuario.
enum Act {
    Mount(String),
    Up(usize),
    Enter(String),
    Open(PathBuf),
    Reveal(PathBuf),
    Mail(PathBuf),
    Copy(PathBuf),
    LaunchDav,
}

/// La pantalla.
pub fn show(app: &mut DeskApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    poll_listing(app);
    let all = mounts(app, &ctx);
    if app.files.mount.as_ref().is_none_or(|id| !all.iter().any(|m| &m.id == id)) {
        app.files.mount = all.iter().find(|m| m.mounted).or(all.first()).map(|m| m.id.clone());
        app.files.rel.clear();
        app.files.shown = None;
    }
    let current = all.iter().find(|m| Some(&m.id) == app.files.mount.as_ref()).cloned();
    let mut act = None;
    split::left(ui, "files-mounts", PaneKind::Sidebar, 260.0, 200.0..=420.0, |ui| {
        let c = theme::colors(ui);
        ui.add_space(12.0);
        ui.label(RichText::new(pick("Drives", "Unidades")).font(theme::semibold_font(ui.ctx(), 15.0)).color(c.text));
        ui.add_space(6.0);
        if all.is_empty() {
            ui.label(
                RichText::new(pick(
                    "No IureDav drives yet. Add one in IureDav and it will show up here.",
                    "Aún no hay unidades de IureDav. Agrega una en IureDav y aparecerá aquí.",
                ))
                .color(c.text_muted),
            );
        }
        for m in &all {
            let on = Some(&m.id) == app.files.mount.as_ref();
            let state = match (m.mounted, m.writable) {
                (false, _) => pick("not mounted", "sin montar"),
                (true, true) => pick("editable", "edición"),
                (true, false) => pick("read only", "solo lectura"),
            };
            let r = ui.selectable_label(on, format!("{}  ·  {state}", m.name));
            if r.clicked() {
                act = Some(Act::Mount(m.id.clone()));
            }
        }
        ui.add_space(10.0);
        if ui.button(pick("Open IureDav", "Abrir IureDav")).clicked() {
            act = Some(Act::LaunchDav);
        }
    });
    split::center(ui, PaneKind::Content, |ui| {
        let c = theme::colors(ui);
        egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 12)).show(ui, |ui| {
            let Some(m) = current.clone() else {
                Banner::new(
                    Severity::Info,
                    pick(
                        "Your Iurefficient files appear here once you set up a drive in IureDav.",
                        "Tus archivos de Iurefficient aparecen aquí cuando configuras una unidad en IureDav.",
                    ),
                )
                .show(ui);
                return;
            };
            if !m.mounted {
                let r = Banner::new(
                    Severity::Info,
                    pick(
                        "This drive isn't mounted. Mount it in IureDav to see its files.",
                        "Esta unidad no está montada. Móntala en IureDav para ver sus archivos.",
                    ),
                )
                .action(pick("Open IureDav", "Abrir IureDav"))
                .show(ui);
                if r.action {
                    act = Some(Act::LaunchDav);
                }
                return;
            }
            let root = PathBuf::from(&m.mount_point);
            ensure_listing(app, &ctx, &root);
            // Ruta: la unidad y cada carpeta, pulsables.
            ui.horizontal_wrapped(|ui| {
                if ui.link(RichText::new(&m.name).strong()).clicked() {
                    act = Some(Act::Up(0));
                }
                for (i, part) in app.files.rel.iter().enumerate() {
                    ui.label(RichText::new("›").color(c.text_muted));
                    if ui.link(part).clicked() {
                        act = Some(Act::Up(i + 1));
                    }
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let r = ui.add(
                    egui::TextEdit::singleline(&mut app.files.filter)
                        .hint_text(pick("Filter by name", "Filtrar por nombre"))
                        .desired_width(260.0),
                );
                crate::widgets::named(&r, egui::Role::SearchInput, pick("Filter by name", "Filtrar por nombre"));
                // Acciones del elegido.
                let sel = app.files.selected.clone().and_then(|n| {
                    let e = app.files.entries.iter().find(|e| e.name == n)?;
                    let mut rel = app.files.rel.clone();
                    rel.push(e.name.clone());
                    // Sin tocar el disco en cada cuadro; se verifica al usar el archivo.
                    joined(&root, &rel).map(|p| (p, e.dir))
                });
                // Al pulsar (una vez, no en cada cuadro): que el archivo siga dentro de la unidad.
                let checked = |p: &Path| {
                    let (r, q) = (root.canonicalize().ok()?, p.canonicalize().ok()?);
                    q.starts_with(&r).then(|| p.to_path_buf())
                };
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if let Some((p, dir)) = sel {
                        if ui.button(pick("Copy path", "Copiar ruta")).clicked() {
                            act = checked(&p).map(Act::Copy);
                        }
                        if !dir && ui.button(pick("Send by email", "Enviar por correo")).clicked() {
                            act = checked(&p).map(Act::Mail);
                        }
                        if ui.button(pick("Show in folder", "Mostrar en la carpeta")).clicked() {
                            act = checked(&p).map(Act::Reveal);
                        }
                        if ui.button(pick("Open", "Abrir")).clicked() {
                            act = Some(if dir {
                                Act::Enter(p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
                            } else {
                                match checked(&p) {
                                    Some(p) => Act::Open(p),
                                    None => return,
                                }
                            });
                        }
                    }
                });
            });
            ui.add_space(6.0);
            if let Some(e) = &app.files.error {
                Banner::new(Severity::Warning, e.get()).show(ui);
                return;
            }
            if app.files.loading.is_some() && app.files.entries.is_empty() {
                ui.label(RichText::new(pick("Reading the folder…", "Leyendo la carpeta…")).color(c.text_muted));
                return;
            }
            let needle = app.files.filter.trim().to_lowercase();
            let rows: Vec<Entry> = app
                .files
                .entries
                .iter()
                .filter(|e| needle.is_empty() || e.name.to_lowercase().contains(&needle))
                .cloned()
                .collect();
            if rows.is_empty() {
                ui.label(RichText::new(pick("Nothing here.", "No hay nada aquí.")).color(c.text_muted));
            }
            if app.files.truncated {
                ui.label(
                    RichText::new(format!(
                        "{} {MAX_ENTRIES} {}",
                        pick("Showing the first", "Se muestran los primeros"),
                        pick("items; use the filter.", "elementos; usa el filtro.")
                    ))
                    .small()
                    .color(c.text_muted),
                );
            }
            let row_h = 28.0;
            egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, rows.len(), |ui, range| {
                for e in rows.get(range).unwrap_or_default() {
                    let on = app.files.selected.as_deref() == Some(e.name.as_str());
                    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                    if on || resp.hovered() {
                        ui.painter().rect_filled(rect, 6.0, if on { c.selection } else { c.hover });
                    }
                    let ic =
                        egui::Rect::from_center_size(egui::pos2(rect.left() + 14.0, rect.center().y), vec2(16.0, 16.0));
                    icons::paint(
                        ui.painter(),
                        ic,
                        if e.dir { Icon::Folder } else { Icon::Attachment },
                        c.text_muted,
                        c.accent,
                    );
                    let font = theme::body_font(ui.ctx());
                    ui.painter().text(
                        egui::pos2(rect.left() + 32.0, rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        &e.name,
                        font.clone(),
                        c.text,
                    );
                    if !e.dir {
                        ui.painter().text(
                            egui::pos2(rect.right() - 8.0, rect.center().y),
                            egui::Align2::RIGHT_CENTER,
                            human_size(e.size),
                            font,
                            c.text_muted,
                        );
                    }
                    iurefficient_ui::a11y::label(&resp, &e.name);
                    if resp.clicked() {
                        app.files.selected = Some(e.name.clone());
                    }
                    if resp.double_clicked() || (on && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                        let mut rel = app.files.rel.clone();
                        rel.push(e.name.clone());
                        act = Some(if e.dir {
                            Act::Enter(e.name.clone())
                        } else {
                            match inside(&root, &rel) {
                                Some(p) => Act::Open(p),
                                None => continue,
                            }
                        });
                    }
                }
            });
        });
    });
    if let Some(a) = act {
        apply(app, &ctx, a);
    }
}

fn apply(app: &mut DeskApp, ctx: &egui::Context, a: Act) {
    let st = &mut app.files;
    match a {
        Act::Mount(id) => {
            st.mount = Some(id);
            st.rel.clear();
            st.shown = None;
            st.selected = None;
        }
        Act::Up(n) => {
            st.rel.truncate(n);
            st.shown = None;
            st.selected = None;
        }
        Act::Enter(name) => {
            if st.rel.len() < MAX_DEPTH {
                st.rel.push(name);
                st.shown = None;
                st.selected = None;
                st.filter.clear();
            }
        }
        Act::Open(p) => open_path(app, &p),
        Act::Reveal(p) => {
            let dir = p.parent().map(Path::to_path_buf).unwrap_or(p);
            open_path(app, &dir);
        }
        Act::Mail(p) => {
            let mut d = app.session.compose_new(None);
            d.draft.attachments.push(iuredesk_engine::iuredesk_compose::Attachment::file(p));
            app.open_composer(d);
        }
        Act::Copy(p) => {
            ctx.copy_text(p.display().to_string());
            app.notice(Notice::success("Path copied", "Ruta copiada"));
        }
        Act::LaunchDav => app.suite.hub.run(SuiteAction::Launch(SisterApp::Dav)),
    }
}

/// Abre un archivo o una carpeta con el programa del sistema (no bloquea).
fn open_path(app: &mut DeskApp, p: &Path) {
    if crate::capture(format!("open:{}", p.display())) {
        return;
    }
    if iuredesk_desktop::launch::open_detached(p).is_err() {
        app.notice(Notice::error(Text::new("Could not open it.", "No se pudo abrir.")));
    }
}

/// Pruebas: entrar a la carpeta `name` y elegir una entrada.
pub fn enter_for_tests(app: &mut DeskApp, name: &str) {
    apply(app, &egui::Context::default(), Act::Enter(name.to_string()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_sorts_folders_first_and_hides_dotfiles() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("b.txt"), b"hola").unwrap();
        std::fs::write(d.path().join("A.pdf"), b"x").unwrap();
        std::fs::write(d.path().join(".oculto"), b"x").unwrap();
        std::fs::create_dir(d.path().join("zeta")).unwrap();
        let (e, truncated) = read_dir(d.path()).unwrap();
        let names: Vec<&str> = e.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["zeta", "A.pdf", "b.txt"]);
        assert!(!truncated && e[0].dir && e[2].size == 4);
        assert!(read_dir(&d.path().join("no-existe")).is_err());
    }

    #[test]
    fn never_leaves_the_drive() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("casos/2026")).unwrap();
        assert!(inside(d.path(), &["casos".into(), "2026".into()]).is_some());
        for bad in [vec![".."], vec!["casos", ".."], vec!["a/b"], vec![""], vec!["no-existe"]] {
            let rel: Vec<String> = bad.iter().map(|s| s.to_string()).collect();
            assert!(inside(d.path(), &rel).is_none(), "{bad:?}");
        }
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().unwrap();
            std::os::unix::fs::symlink(outside.path(), d.path().join("fuga")).unwrap();
            assert!(inside(d.path(), &["fuga".into()]).is_none(), "un enlace no saca de la unidad");
        }
    }

    #[test]
    fn sizes_are_human() {
        assert_eq!(human_size(12), "12 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }
}
