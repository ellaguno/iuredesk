//! UX-6: cada nota es también un archivo Markdown en la carpeta de notas.
//!
//! El almacén sigue siendo el índice (búsqueda, fijadas, orden); el archivo es lo que se edita en
//! IureEditor (o en cualquier editor) y lo que viaja si la carpeta está en una unidad de IureDav.
//!
//! - **Formato**: `# Título`, línea en blanco y el cuerpo. Solo si la nota tiene etiquetas o está
//!   fijada lleva antes un bloque YAML (`tags`, `pinned`), que IureEditor conserva tal cual.
//! - **Nombre**: el título saneado (`Minuta con Ramírez.md`); si ya existe, `… (2).md`.
//! - **Vigilancia por sondeo** (cada [`POLL`]): fecha de modificación y tamaño de cada `.md`. Se
//!   eligió sondear y no `inotify`/`notify` porque las unidades de red (IureDav, WebDAV, SMB) no
//!   avisan de los cambios y porque así no entra ninguna dependencia nueva. Un cambio se lee
//!   cuando el archivo dejó de cambiar entre dos vueltas (antirrebote: nunca se lee un archivo a
//!   medio guardar) y solo cuenta si su huella SHA-256 es otra.
//! - **Todo el disco en un hilo propio** ([`Worker`]): la interfaz nunca espera a una unidad de
//!   red lenta; manda órdenes y recoge los resultados en `Session::poll`.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use iuredesk_model::NoteId;
use sha2::{Digest, Sha256};

/// Cada cuánto se revisa la carpeta.
pub const POLL: Duration = Duration::from_secs(2);
/// Archivo más grande que se lee como nota.
pub const MAX_NOTE_FILE: u64 = 4 * 1024 * 1024;
/// Archivos `.md` que se consideran como máximo.
pub const MAX_FILES: usize = 5_000;
/// Largo máximo del nombre (sin `.md`).
const MAX_STEM_CHARS: usize = 80;

/// Contenido de un archivo de nota ya interpretado.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub pinned: bool,
}

/// Huella SHA-256 (hex) de un contenido.
pub fn content_hash(bytes: &[u8]) -> String {
    let d = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in d {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Escribe una nota como Markdown.
pub fn render(title: &str, body: &str, tags: &[String], pinned: bool) -> String {
    let mut out = String::new();
    if !tags.is_empty() || pinned {
        out.push_str("---\n");
        if !tags.is_empty() {
            let list: Vec<String> = tags.iter().map(|t| yaml_scalar(t)).collect();
            out.push_str(&format!("tags: [{}]\n", list.join(", ")));
        }
        if pinned {
            out.push_str("pinned: true\n");
        }
        out.push_str("---\n\n");
    }
    let title = title.trim();
    if !title.is_empty() {
        out.push_str("# ");
        out.push_str(&title.replace(['\r', '\n'], " "));
        out.push_str("\n\n");
    }
    let body = body.trim_end_matches(['\n', '\r']);
    out.push_str(body);
    if !body.is_empty() {
        out.push('\n');
    }
    out
}

fn yaml_scalar(s: &str) -> String {
    let plain = !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ' ' | '.'));
    if plain && !s.starts_with(' ') && !s.ends_with(' ') {
        s.to_string()
    } else {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

fn yaml_unquote(s: &str) -> String {
    let t = s.trim();
    if let Some(inner) = t.strip_prefix('"').and_then(|x| x.strip_suffix('"')) {
        return inner.replace("\\\"", "\"").replace("\\\\", "\\");
    }
    if let Some(inner) = t.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')) {
        return inner.replace("''", "'");
    }
    t.to_string()
}

/// Lee un archivo de nota: bloque YAML opcional (`tags`, `pinned`; lo demás se ignora), `# Título`
/// como primera línea con texto y el resto como cuerpo. Sin `# ` al principio no hay título (la
/// lista muestra la primera línea).
pub fn parse(text: &str) -> Parsed {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text).replace("\r\n", "\n");
    let mut out = Parsed::default();
    let mut rest: &str = &text;
    if let Some(after) = rest.strip_prefix("---\n")
        && let Some(end) = find_fence(after)
    {
        let (yaml, tail) = after.split_at(end);
        let looks_yaml = yaml.trim().is_empty() || yaml.lines().any(|l| l.contains(':'));
        if looks_yaml {
            for line in yaml.lines().take(200) {
                let Some((k, v)) = line.split_once(':') else { continue };
                match k.trim().to_ascii_lowercase().as_str() {
                    "tags" => {
                        let v = v.trim().trim_start_matches('[').trim_end_matches(']');
                        out.tags = v
                            .split(',')
                            .map(yaml_unquote)
                            .filter(|t| !t.is_empty())
                            .take(64)
                            .map(|t| t.chars().take(100).collect())
                            .collect();
                    }
                    "pinned" => out.pinned = v.trim().eq_ignore_ascii_case("true"),
                    _ => {}
                }
            }
            rest = tail.split_once('\n').map_or("", |(_, t)| t);
        }
    }
    let rest = rest.trim_start_matches('\n');
    let (first, after) = rest.split_once('\n').unwrap_or((rest, ""));
    match first.strip_prefix("# ") {
        Some(t) => {
            out.title = t.trim().to_string();
            out.body = after.trim_start_matches('\n').trim_end_matches('\n').to_string();
        }
        None => out.body = rest.trim_end_matches('\n').to_string(),
    }
    out
}

/// Posición del `---`/`...` que cierra el bloque YAML (al principio de una línea).
fn find_fence(s: &str) -> Option<usize> {
    let mut pos = 0usize;
    for line in s.split_inclusive('\n').take(201) {
        let l = line.trim_end_matches(['\n', '\r']).trim_end();
        if l == "---" || l == "..." {
            return Some(pos);
        }
        pos = pos.saturating_add(line.len());
    }
    None
}

/// Nombre de archivo (sin `.md`) para un título: sin caracteres prohibidos en Windows, macOS o
/// Linux, sin puntos ni espacios en los extremos, recortado. Vacío → «Nota»/«Note».
pub fn file_stem(title: &str) -> String {
    let cleaned: String =
        title
            .chars()
            .map(|c| {
                if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() {
                    ' '
                } else {
                    c
                }
            })
            .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut stem: String = collapsed.chars().take(MAX_STEM_CHARS).collect();
    stem = stem.trim_matches(|c: char| c == '.' || c.is_whitespace()).to_string();
    // Nombres reservados de Windows.
    let upper = stem.to_ascii_uppercase();
    let reserved = matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.len() == 4
            && upper.chars().last().is_some_and(|c| c.is_ascii_digit()));
    if stem.is_empty() || reserved {
        return crate::lang::pick("Note", "Nota").to_string();
    }
    stem
}

/// ¿`name` es un archivo de nota (`.md`, no oculto ni temporal)?
pub fn is_note_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".md") && !name.starts_with('.') && !name.starts_with("~$") && lower.len() > 3
}

/// Carpeta de notas por omisión: `Documentos/IureDesk/Notas` (con `IUREDESK_DATA_DIR`, dentro de
/// esa carpeta, para no tocar las notas de la instalación normal).
pub fn default_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("IUREDESK_DATA_DIR").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(d).join("Notas"));
    }
    let docs = dirs::document_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(crate::lang::pick("Documents", "Documentos"))))?;
    Some(docs.join("IureDesk").join(crate::lang::pick("Notes", "Notas")))
}

/// Fecha de modificación y tamaño: lo que se compara en cada vuelta.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    mtime: Option<SystemTime>,
    len: u64,
}

#[derive(Clone, Debug)]
struct Known {
    hash: String,
    stamp: Option<Stamp>,
}

/// Lo que cambió en la carpeta.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FsEvent {
    /// Archivo nuevo o con otro contenido.
    Changed { name: String, content: String, hash: String },
    /// Archivo que ya no está (faltó en dos vueltas seguidas).
    Removed { name: String },
    /// Archivo renombrado fuera (mismo contenido con otro nombre).
    Renamed { from: String, to: String },
}

/// Resultado de escribir una nota.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteOutcome {
    /// Escrita (con su nombre final y su huella).
    Written { name: String, hash: String },
    /// El archivo cambió fuera desde la última vez: no se tocó. Trae lo que hay en disco.
    Conflict { name: String, content: String, hash: String },
}

/// La carpeta de notas (sin hilos: lo usa [`Worker`] y las pruebas).
#[derive(Debug)]
pub struct NotesDir {
    dir: PathBuf,
    known: HashMap<String, Known>,
    pending: HashMap<String, Stamp>,
    gone: HashSet<String>,
    first: bool,
}

fn stamp_of(meta: &std::fs::Metadata) -> Stamp {
    Stamp { mtime: meta.modified().ok(), len: meta.len() }
}

fn read_capped(path: &Path) -> std::io::Result<Vec<u8>> {
    let f = std::fs::File::open(path)?;
    let mut buf = Vec::new();
    f.take(MAX_NOTE_FILE.saturating_add(1)).read_to_end(&mut buf)?;
    if buf.len() as u64 > MAX_NOTE_FILE {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "archivo demasiado grande"));
    }
    Ok(buf)
}

impl NotesDir {
    /// `known`: (nombre, huella) de los archivos que el almacén ya conoce.
    pub fn new(dir: PathBuf, known: impl IntoIterator<Item = (String, String)>) -> NotesDir {
        NotesDir {
            dir,
            known: known.into_iter().map(|(n, hash)| (n, Known { hash, stamp: None })).collect(),
            pending: HashMap::new(),
            gone: HashSet::new(),
            first: true,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Ruta de un archivo de la carpeta.
    pub fn path_of(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// `true` mientras quede algo esperando a dejar de cambiar.
    pub fn settling(&self) -> bool {
        !self.pending.is_empty() || !self.gone.is_empty()
    }

    fn list(&self) -> std::io::Result<Vec<(String, Stamp)>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let Ok(entry) = entry else { continue };
            let Some(name) = entry.file_name().to_str().map(str::to_string) else { continue };
            if !is_note_file(&name) {
                continue;
            }
            let Ok(meta) = std::fs::metadata(entry.path()) else { continue };
            if !meta.is_file() {
                continue;
            }
            out.push((name, stamp_of(&meta)));
            if out.len() >= MAX_FILES {
                break;
            }
        }
        Ok(out)
    }

    /// Una vuelta: lo que cambió desde la anterior. En la primera vuelta todo se lee enseguida (el
    /// antirrebote es para los cambios que ocurren con la app abierta).
    pub fn scan(&mut self) -> std::io::Result<Vec<FsEvent>> {
        if !self.dir.is_dir() {
            return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "la carpeta de notas no está"));
        }
        let listing = self.list()?;
        let first = std::mem::replace(&mut self.first, false);
        let mut changed = Vec::new();
        let mut present = HashSet::new();
        for (name, stamp) in listing {
            present.insert(name.clone());
            self.gone.remove(&name);
            if self.known.get(&name).is_some_and(|k| k.stamp == Some(stamp)) {
                self.pending.remove(&name);
                continue;
            }
            let stable = first || self.pending.get(&name) == Some(&stamp);
            if !stable {
                self.pending.insert(name, stamp);
                continue;
            }
            self.pending.remove(&name);
            let Ok(bytes) = read_capped(&self.path_of(&name)) else { continue };
            let hash = content_hash(&bytes);
            let new_file = !self.known.contains_key(&name);
            let same = self.known.get(&name).is_some_and(|k| k.hash == hash);
            self.known.insert(name.clone(), Known { hash: hash.clone(), stamp: Some(stamp) });
            if !same {
                changed.push((name, String::from_utf8_lossy(&bytes).into_owned(), hash, new_file));
            }
        }
        let mut removed = Vec::new();
        let missing: Vec<String> = self.known.keys().filter(|n| !present.contains(*n)).cloned().collect();
        for name in missing {
            if first || self.gone.contains(&name) {
                self.gone.remove(&name);
                if let Some(k) = self.known.remove(&name) {
                    removed.push((name, k.hash));
                }
            } else {
                self.gone.insert(name);
            }
        }
        self.pending.retain(|n, _| present.contains(n));
        // Renombrado fuera: desaparece uno y aparece otro nuevo con la misma huella.
        let mut out = Vec::new();
        let mut used = HashSet::new();
        for (from, hash) in removed {
            let twin = changed.iter().position(|(n, _, h, new)| *new && *h == hash && !used.contains(n));
            match twin.and_then(|i| changed.get(i)) {
                Some((to, ..)) => {
                    used.insert(to.clone());
                    out.push(FsEvent::Renamed { from, to: to.clone() });
                }
                None => out.push(FsEvent::Removed { name: from }),
            }
        }
        for (name, content, hash, _) in changed {
            if !used.contains(&name) {
                out.push(FsEvent::Changed { name, content, hash });
            }
        }
        Ok(out)
    }

    fn unique_name(&self, stem: &str, current: Option<&str>) -> String {
        for i in 1..=999u32 {
            let name = if i == 1 { format!("{stem}.md") } else { format!("{stem} ({i}).md") };
            if Some(name.as_str()) == current {
                return name;
            }
            let taken = self.known.contains_key(&name) || self.path_of(&name).exists();
            if !taken {
                return name;
            }
        }
        format!("{stem} ({}).md", Utc::now().timestamp_millis())
    }

    /// ¿El nombre actual ya corresponde al título (`stem.md` o `stem (n).md`)?
    fn name_fits(current: &str, stem: &str) -> bool {
        let Some(base) = current.strip_suffix(".md").or_else(|| current.strip_suffix(".MD")) else { return false };
        if base == stem {
            return true;
        }
        base.strip_prefix(stem)
            .and_then(|r| r.strip_prefix(" ("))
            .and_then(|r| r.strip_suffix(')'))
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
    }

    /// Escribe una nota. `current`: su archivo actual; `base`: la huella que tenía la última vez
    /// (si el disco ya no la tiene, alguien lo cambió fuera: [`WriteOutcome::Conflict`]). Si el
    /// título cambió, el archivo se renombra.
    pub fn write(
        &mut self,
        current: Option<&str>,
        stem: &str,
        content: &str,
        base: Option<&str>,
    ) -> std::io::Result<WriteOutcome> {
        std::fs::create_dir_all(&self.dir)?;
        let hash = content_hash(content.as_bytes());
        if let Some(cur) = current {
            let path = self.path_of(cur);
            if path.is_file() {
                let disk = read_capped(&path)?;
                let disk_hash = content_hash(&disk);
                if disk_hash != hash && base.is_some_and(|b| b != disk_hash) {
                    // Quien llama recibe lo de disco: ya no es un cambio pendiente de avisar.
                    let stamp = std::fs::metadata(&path).ok().map(|m| stamp_of(&m));
                    self.pending.remove(cur);
                    self.known.insert(cur.to_string(), Known { hash: disk_hash.clone(), stamp });
                    return Ok(WriteOutcome::Conflict {
                        name: cur.to_string(),
                        content: String::from_utf8_lossy(&disk).into_owned(),
                        hash: disk_hash,
                    });
                }
            }
        }
        let target = match current {
            Some(cur) if Self::name_fits(cur, stem) => cur.to_string(),
            _ => self.unique_name(stem, current),
        };
        let path = self.path_of(&target);
        // Primero a un temporal oculto y luego se renombra: un lector nunca ve medio archivo.
        let tmp = self.path_of(&format!(".{target}.iuredesk-tmp"));
        std::fs::write(&tmp, content.as_bytes())?;
        if let Err(e) = std::fs::rename(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        if let Some(cur) = current.filter(|c| *c != target) {
            let _ = std::fs::remove_file(self.path_of(cur));
            self.known.remove(cur);
            self.gone.remove(cur);
        }
        let stamp = std::fs::metadata(&path).ok().map(|m| stamp_of(&m));
        self.pending.remove(&target);
        self.known.insert(target.clone(), Known { hash: hash.clone(), stamp });
        Ok(WriteOutcome::Written { name: target, hash })
    }

    /// Borra el archivo de una nota borrada. Si cambió fuera desde `base`, se deja (volverá como
    /// nota: nunca se pierde lo escrito en otro editor). Devuelve `true` si se borró.
    pub fn remove(&mut self, name: &str, base: Option<&str>) -> std::io::Result<bool> {
        let path = self.path_of(name);
        self.known.remove(name);
        self.pending.remove(name);
        self.gone.remove(name);
        if !path.is_file() {
            return Ok(false);
        }
        let disk = read_capped(&path)?;
        if base.is_some_and(|b| b != content_hash(&disk)) {
            return Ok(false);
        }
        std::fs::remove_file(&path)?;
        Ok(true)
    }
}

// ------------------------------------------------------------------ hilo

/// Orden para el hilo de la carpeta.
#[derive(Debug)]
pub enum Cmd {
    Write {
        id: NoteId,
        current: Option<String>,
        stem: String,
        content: String,
        base: Option<String>,
        updated: DateTime<Utc>,
    },
    Remove {
        name: String,
        base: Option<String>,
    },
    /// Responde [`Evt::Flushed`] tras una vuelta completa.
    Flush(u64),
    Stop,
}

/// Resultado del hilo.
#[derive(Debug)]
pub enum Evt {
    Fs(FsEvent),
    Written {
        id: NoteId,
        name: String,
        hash: String,
        updated: DateTime<Utc>,
    },
    Conflict {
        id: NoteId,
        name: String,
        content: String,
        hash: String,
    },
    WriteFailed {
        id: NoteId,
        error: String,
    },
    /// Terminó una vuelta; `settled` = nada esperando a dejar de cambiar.
    Scanned {
        settled: bool,
    },
    /// La carpeta no está (unidad sin montar, borrada…).
    Unavailable(String),
    Flushed(u64),
}

/// Hilo que hace todo el trabajo de disco de la carpeta de notas.
#[derive(Debug)]
pub struct Worker {
    tx: Sender<Cmd>,
    rx: Receiver<Evt>,
    dir: PathBuf,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Worker {
    /// Arranca el hilo. `create`: crear la carpeta si falta (solo la de omisión: una carpeta
    /// elegida que falta suele ser una unidad sin montar, y crearla escribiría debajo del punto
    /// de montaje).
    pub fn spawn(
        dir: PathBuf,
        known: Vec<(String, String)>,
        create: bool,
        poll: Duration,
        wake: Option<crate::background::Waker>,
    ) -> std::io::Result<Worker> {
        if create {
            let _ = std::fs::create_dir_all(&dir);
        }
        let (tx, cmd_rx) = channel::<Cmd>();
        let (evt_tx, rx) = channel::<Evt>();
        let mut nd = NotesDir::new(dir.clone(), known);
        let join = std::thread::Builder::new().name("iuredesk-notes".into()).spawn(move || {
            let wake = || {
                if let Some(w) = &wake {
                    w();
                }
            };
            let mut unavailable = false;
            let mut scan = |nd: &mut NotesDir, tx: &Sender<Evt>| -> bool {
                match nd.scan() {
                    Ok(evs) => {
                        let news = !evs.is_empty() || unavailable;
                        unavailable = false;
                        for e in evs {
                            if tx.send(Evt::Fs(e)).is_err() {
                                return false;
                            }
                        }
                        let ok = tx.send(Evt::Scanned { settled: !nd.settling() }).is_ok();
                        if news {
                            wake();
                        }
                        ok
                    }
                    Err(e) => {
                        if !unavailable {
                            unavailable = true;
                            let ok = tx.send(Evt::Unavailable(e.to_string())).is_ok();
                            wake();
                            return ok;
                        }
                        true
                    }
                }
            };
            if !scan(&mut nd, &evt_tx) {
                return;
            }
            let mut next = std::time::Instant::now() + poll;
            loop {
                let left = next.saturating_duration_since(std::time::Instant::now());
                let ok = match cmd_rx.recv_timeout(left) {
                    Ok(Cmd::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                    Err(RecvTimeoutError::Timeout) => {
                        next = std::time::Instant::now() + poll;
                        scan(&mut nd, &evt_tx)
                    }
                    Ok(Cmd::Flush(seq)) => {
                        let ok = scan(&mut nd, &evt_tx) && evt_tx.send(Evt::Flushed(seq)).is_ok();
                        wake();
                        ok
                    }
                    Ok(Cmd::Write { id, current, stem, content, base, updated }) => {
                        let r = match nd.write(current.as_deref(), &stem, &content, base.as_deref()) {
                            Ok(WriteOutcome::Written { name, hash }) => Evt::Written { id, name, hash, updated },
                            Ok(WriteOutcome::Conflict { name, content, hash }) => {
                                Evt::Conflict { id, name, content, hash }
                            }
                            Err(e) => Evt::WriteFailed { id, error: e.to_string() },
                        };
                        let ok = evt_tx.send(r).is_ok();
                        wake();
                        ok
                    }
                    Ok(Cmd::Remove { name, base }) => {
                        let _ = nd.remove(&name, base.as_deref());
                        true
                    }
                };
                if !ok {
                    return;
                }
                // Órdenes seguidas (guardado automático al escribir) no aplazan la revisión.
                if std::time::Instant::now() >= next {
                    next = std::time::Instant::now() + poll;
                    if !scan(&mut nd, &evt_tx) {
                        return;
                    }
                }
            }
        })?;
        Ok(Worker { tx, rx, dir, join: Some(join) })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Manda una orden (si el hilo terminó, no pasa nada).
    pub fn send(&self, c: Cmd) {
        let _ = self.tx.send(c);
    }

    /// Lo que llegó del hilo, sin esperar.
    pub fn drain(&self) -> Vec<Evt> {
        self.rx.try_iter().take(10_000).collect()
    }

    /// Espera un resultado hasta `deadline` (para [`crate::session::Session::notes_fs_flush`]).
    pub fn recv_until(&self, deadline: std::time::Instant) -> Option<Evt> {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        self.rx.recv_timeout(left).ok()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Stop);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_and_parse_round_trip() {
        let tags = vec!["cliente".to_string(), "a, b".to_string()];
        let md = render("Minuta", "- acuerdo 1\n- acuerdo 2\n\n", &tags, true);
        assert!(md.starts_with("---\ntags: [cliente, \"a, b\"]\npinned: true\n---\n\n# Minuta\n\n- acuerdo 1"));
        assert!(md.ends_with("acuerdo 2\n"));
        let p = parse(&md);
        assert_eq!(p.title, "Minuta");
        assert_eq!(p.body, "- acuerdo 1\n- acuerdo 2");
        assert!(p.pinned);
        // (La coma dentro de una etiqueta entrecomillada no se conserva: YAML mínimo.)
        assert_eq!(p.tags.first().map(String::as_str), Some("cliente"));
        // Idempotente: lo que se lee y se vuelve a escribir es lo mismo.
        let plain = render("Ideas", "texto", &[], false);
        assert_eq!(plain, "# Ideas\n\ntexto\n");
        let p = parse(&plain);
        assert_eq!(render(&p.title, &p.body, &p.tags, p.pinned), plain);
        // Sin título; CRLF y BOM de otros editores.
        let p = parse("\u{feff}Primera línea\r\nsegunda\r\n");
        assert_eq!((p.title.as_str(), p.body.as_str()), ("", "Primera línea\nsegunda"));
        // Un `---` de regla horizontal no es YAML.
        let p = parse("---\nsolo texto\n---\nmás");
        assert_eq!(p.body, "---\nsolo texto\n---\nmás");
        assert_eq!(render("", "", &[], false), "");
    }

    #[test]
    fn file_names() {
        assert_eq!(file_stem("Minuta: cliente/Ramírez?"), "Minuta cliente Ramírez");
        assert_eq!(file_stem("  ..  "), crate::lang::pick("Note", "Nota"));
        assert_eq!(file_stem("CON"), crate::lang::pick("Note", "Nota"));
        assert_eq!(file_stem(&"x".repeat(300)).chars().count(), MAX_STEM_CHARS);
        assert!(is_note_file("a.md") && is_note_file("B.MD"));
        assert!(!is_note_file(".a.md") && !is_note_file("a.txt") && !is_note_file(".a.md.iuredesk-tmp"));
        assert!(NotesDir::name_fits("Ideas (3).md", "Ideas"));
        assert!(!NotesDir::name_fits("Ideas nuevas.md", "Ideas"));
    }

    /// Crear, renombrar (cambia el título) y borrar; el nombre repetido recibe `(2)`.
    #[test]
    fn create_rename_delete() {
        let tmp = tempfile::tempdir().unwrap();
        let mut nd = NotesDir::new(tmp.path().join("Notas"), Vec::new());
        let WriteOutcome::Written { name, hash } = nd.write(None, "Ideas", "# Ideas\n", None).unwrap() else {
            panic!("conflicto")
        };
        assert_eq!(name, "Ideas.md");
        assert_eq!(std::fs::read_to_string(nd.path_of("Ideas.md")).unwrap(), "# Ideas\n");
        let WriteOutcome::Written { name: n2, .. } = nd.write(None, "Ideas", "otra", None).unwrap() else { panic!() };
        assert_eq!(n2, "Ideas (2).md");
        // Mismo título: mismo archivo.
        let WriteOutcome::Written { name: same, hash: h2 } =
            nd.write(Some(&name), "Ideas", "# Ideas\n\nmás", Some(&hash)).unwrap()
        else {
            panic!()
        };
        assert_eq!(same, "Ideas.md");
        // Título nuevo: se renombra.
        let WriteOutcome::Written { name: renamed, hash: h3 } =
            nd.write(Some(&same), "Plan", "# Plan\n", Some(&h2)).unwrap()
        else {
            panic!()
        };
        assert_eq!(renamed, "Plan.md");
        assert!(!nd.path_of("Ideas.md").exists());
        // Nada de esto se reporta como cambio externo.
        assert!(nd.scan().unwrap().is_empty());
        assert!(nd.remove(&renamed, Some(&h3)).unwrap());
        assert!(!nd.path_of("Plan.md").exists());
        assert!(nd.scan().unwrap().is_empty());
    }

    /// Un cambio de fuera se lee solo cuando el archivo dejó de cambiar (antirrebote) y una vez.
    #[test]
    fn external_change_is_debounced_and_picked_up() {
        let tmp = tempfile::tempdir().unwrap();
        let mut nd = NotesDir::new(tmp.path().to_path_buf(), Vec::new());
        let WriteOutcome::Written { name, .. } = nd.write(None, "Ideas", "# Ideas\n", None).unwrap() else { panic!() };
        assert!(nd.scan().unwrap().is_empty(), "la primera vuelta ve lo propio");
        std::fs::write(nd.path_of(&name), "# Ideas\n\nescrito en IureEditor que es más largo\n").unwrap();
        assert!(nd.scan().unwrap().is_empty(), "aún puede estar guardándose");
        assert!(nd.settling());
        let ev = nd.scan().unwrap();
        assert!(
            matches!(ev.as_slice(), [FsEvent::Changed { name: n, content, .. }] if *n == name && content.contains("IureEditor"))
        );
        assert!(nd.scan().unwrap().is_empty(), "una sola vez");
        // Mismo contenido reescrito (otra fecha): no cuenta.
        std::fs::write(nd.path_of(&name), "# Ideas\n\nescrito en IureEditor que es más largo\n").unwrap();
        let _ = nd.scan().unwrap();
        assert!(nd.scan().unwrap().is_empty());
        // Un archivo nuevo de fuera.
        std::fs::write(tmp.path().join("Nueva.md"), "hola").unwrap();
        std::fs::write(tmp.path().join("ignorar.txt"), "x").unwrap();
        let _ = nd.scan().unwrap();
        let ev = nd.scan().unwrap();
        assert!(matches!(ev.as_slice(), [FsEvent::Changed { name, .. }] if name == "Nueva.md"));
    }

    /// Borrado y renombrado fuera; la primera vuelta tras abrir compara con lo que sabe el almacén.
    #[test]
    fn external_remove_rename_and_startup() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("A.md"), "a").unwrap();
        std::fs::write(tmp.path().join("B.md"), "b").unwrap();
        std::fs::write(tmp.path().join("C.md"), "c cambiado con la app cerrada").unwrap();
        let known = vec![
            ("A.md".to_string(), content_hash(b"a")),
            ("B.md".to_string(), content_hash(b"b")),
            ("C.md".to_string(), content_hash(b"c")),
            ("Borrado.md".to_string(), content_hash(b"z")),
        ];
        let mut nd = NotesDir::new(tmp.path().to_path_buf(), known);
        let mut ev = nd.scan().unwrap();
        ev.sort_by_key(|e| format!("{e:?}"));
        assert_eq!(ev.len(), 2, "{ev:?}");
        assert!(ev.iter().any(|e| matches!(e, FsEvent::Changed { name, .. } if name == "C.md")));
        assert!(ev.iter().any(|e| matches!(e, FsEvent::Removed { name } if name == "Borrado.md")));
        std::fs::rename(tmp.path().join("A.md"), tmp.path().join("A2.md")).unwrap();
        std::fs::remove_file(tmp.path().join("B.md")).unwrap();
        assert!(nd.scan().unwrap().is_empty(), "esperar una vuelta");
        let ev = nd.scan().unwrap();
        assert!(ev.contains(&FsEvent::Renamed { from: "A.md".into(), to: "A2.md".into() }), "{ev:?}");
        assert!(ev.contains(&FsEvent::Removed { name: "B.md".into() }), "{ev:?}");
        assert!(NotesDir::new(tmp.path().join("no-existe"), Vec::new()).scan().is_err());
    }

    /// Editada fuera y aquí a la vez: no se pisa el archivo.
    #[test]
    fn conflict_keeps_disk_version() {
        let tmp = tempfile::tempdir().unwrap();
        let mut nd = NotesDir::new(tmp.path().to_path_buf(), Vec::new());
        let WriteOutcome::Written { name, hash } = nd.write(None, "Ideas", "v1", None).unwrap() else { panic!() };
        std::fs::write(nd.path_of(&name), "v2 de IureEditor").unwrap();
        let r = nd.write(Some(&name), "Ideas", "v2 de IureDesk", Some(&hash)).unwrap();
        assert!(matches!(r, WriteOutcome::Conflict { ref content, .. } if content == "v2 de IureEditor"));
        assert_eq!(std::fs::read_to_string(nd.path_of(&name)).unwrap(), "v2 de IureEditor");
        // Borrar tampoco pisa lo de fuera.
        assert!(!nd.remove(&name, Some(&hash)).unwrap());
        assert!(nd.path_of(&name).exists());
    }

    #[test]
    fn worker_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let w = Worker::spawn(tmp.path().join("n"), Vec::new(), true, Duration::from_millis(20), None).unwrap();
        w.send(Cmd::Write {
            id: NoteId(1),
            current: None,
            stem: "Hola".into(),
            content: "# Hola\n".into(),
            base: None,
            updated: Utc::now(),
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut written = None;
        while let Some(e) = w.recv_until(deadline) {
            if let Evt::Written { name, .. } = e {
                written = Some(name);
                break;
            }
        }
        assert_eq!(written.as_deref(), Some("Hola.md"));
        assert!(tmp.path().join("n").join("Hola.md").is_file());
    }

    proptest::proptest! {
        #[test]
        fn parse_never_panics(s in "\\PC{0,300}") {
            let p = parse(&s);
            let again = render(&p.title, &p.body, &p.tags, p.pinned);
            let _ = parse(&again);
            let _ = file_stem(&s);
        }
    }
}
