//! Outlook: archivos de datos `.pst` (Unicode y ANSI) con el lector de Microsoft
//! [`outlook-pst`](https://github.com/microsoft/outlook-pst-rs) (Rust puro, MIT).
//!
//! **Evaluación (2026-10).** `outlook-pst` 1.2 implementa la especificación pública [MS-PST]
//! (NDB, LTP, mensajería, propiedades con nombre, codificaciones *permute*/*cyclic*), solo depende
//! de `byteorder`, `thiserror` y `tracing`, y lee bajo demanda (no carga el archivo en memoria).
//! En contra: algunas rutas usan `expect`/indexación sobre datos del archivo y pueden entrar en
//! pánico con un PST dañado; por eso cada elemento se lee dentro de `catch_unwind` y un pánico se
//! informa como «elemento dañado» sin detener la importación. `outlook-pst-rw` (bifurcación MIT
//! con escritor) solo se usa en las pruebas para generar PST sintéticos.
//!
//! **OST.** Los `.ost` de Outlook 2013+ usan bloques de 4 KiB comprimidos que el lector no
//! admite: se avisa y se propone agregar la cuenta (los datos están en el servidor) o exportar un
//! `.pst` desde Outlook.
//!
//! Correo (con carpetas y adjuntos; el MIME se regenera a partir de las propiedades y, si están,
//! se conservan las cabeceras de transporte originales), contactos, citas (con repeticiones),
//! tareas y notas. Las listas de distribución y el diario se omiten con motivo.

pub mod map;
pub mod recur;
pub mod rtf;

use std::collections::{BTreeMap, HashMap};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::rc::Rc;

use iuredesk_model::FolderRole;
use outlook_pst::ltp::prop_context::PropertyValue;
use outlook_pst::ltp::table_context::TableContext;
use outlook_pst::messaging::attachment::{AnsiAttachment, AttachmentData, UnicodeAttachment};
use outlook_pst::messaging::message::{AnsiMessage, Message, UnicodeMessage};
use outlook_pst::messaging::named_prop::{NamedPropertyGuid, NamedPropertyId};
use outlook_pst::messaging::store::{AnsiStore, EntryId, Store, UnicodeStore};
use outlook_pst::ndb::node_id::NodeId;
use outlook_pst::{AnsiPstFile, UnicodePstFile};

use self::map::{Attachment, Named, Prop, PropSet, PstItem, Recipient};
use crate::checkpoint::unit_key;
use crate::error::Result;
use crate::report::{Notice, SkipReason};
use crate::sink::{MsgMeta, Sink};

/// Carpetas máximas.
const MAX_FOLDERS: usize = 100_000;
/// Profundidad máxima de carpetas.
const MAX_DEPTH: usize = 32;
/// Adjuntos máximos por elemento.
const MAX_ATTACHMENTS: usize = 500;
/// Destinatarios máximos por elemento.
const MAX_RECIPIENTS: usize = 2_000;

/// Archivo PST abierto.
enum Pst {
    Unicode(Rc<UnicodeStore>),
    Ansi(Rc<AnsiStore>),
}

impl Pst {
    fn open(path: &Path) -> std::io::Result<Self> {
        match UnicodePstFile::open(path) {
            Ok(f) => Ok(Self::Unicode(UnicodeStore::read(Rc::new(f))?)),
            Err(_) => Ok(Self::Ansi(AnsiStore::read(Rc::new(AnsiPstFile::open(path)?))?)),
        }
    }

    fn store(&self) -> Rc<dyn Store> {
        match self {
            Self::Unicode(s) => s.clone(),
            Self::Ansi(s) => s.clone(),
        }
    }

    /// Lee un elemento completo (propiedades, destinatarios y adjuntos).
    fn read_item(&self, entry: &EntryId, named: &NamedMap, max_att: u64) -> std::io::Result<(PstItem, Vec<String>)> {
        macro_rules! read {
            ($store:expr, $msg:ty, $att:ty) => {{
                let msg = <$msg>::read($store.clone(), entry, None)?;
                let mut item = item_of(msg.as_ref(), named);
                let mut dropped = Vec::new();
                if let Some(table) = msg.attachment_table() {
                    for row in table.rows_matrix().take(MAX_ATTACHMENTS) {
                        let node = NodeId::from(u32::from(row.id()));
                        let Ok(a) = <$att>::read(msg.clone(), node, None) else { continue };
                        match attachment_of(a.as_ref(), named, max_att) {
                            Ok(Some(a)) => item.attachments.push(a),
                            Ok(None) => {}
                            Err(name) => dropped.push(name),
                        }
                    }
                }
                Ok((item, dropped))
            }};
        }
        match self {
            Self::Unicode(s) => read!(s, UnicodeMessage, UnicodeAttachment),
            Self::Ansi(s) => read!(s, AnsiMessage, AnsiAttachment),
        }
    }
}

/// id de propiedad con nombre (0x8000+) → clave.
#[derive(Default)]
struct NamedMap(HashMap<u16, Named>);

const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> (u32, u16, u16, [u8; 8]) {
    (d1, d2, d3, d4)
}
const C000_46: [u8; 8] = [0xC0, 0, 0, 0, 0, 0, 0, 0x46];
const SETS: [((u32, u16, u16, [u8; 8]), PropSet); 6] = [
    (guid(0x0006_2002, 0, 0, C000_46), PropSet::Appointment),
    (guid(0x0006_2003, 0, 0, C000_46), PropSet::Task),
    (guid(0x0006_2004, 0, 0, C000_46), PropSet::Address),
    (guid(0x0006_2008, 0, 0, C000_46), PropSet::Common),
    (guid(0x6ED8_DA90, 0x450B, 0x101B, [0x98, 0xDA, 0x00, 0xAA, 0x00, 0x3F, 0x13, 0x05]), PropSet::Meeting),
    (guid(0x0002_0329, 0, 0, C000_46), PropSet::PublicStrings),
];

fn named_map(store: &dyn Store) -> NamedMap {
    let Ok(map) = store.named_property_map() else { return NamedMap::default() };
    let props = map.properties();
    let Ok(entries) = props.stream_entry() else { return NamedMap::default() };
    let guids = props.stream_guid().unwrap_or_default();
    let strings = match props.get(0x0004) {
        Some(PropertyValue::Binary(b)) => b.buffer().to_vec(),
        _ => Vec::new(),
    };
    let mut out = HashMap::new();
    for e in entries.into_iter().take(1_000_000) {
        let set = match e.guid() {
            NamedPropertyGuid::PublicStrings => Some(PropSet::PublicStrings),
            NamedPropertyGuid::GuidIndex(i) => guids.get(usize::from(i)).and_then(|g| {
                SETS.iter()
                    .find(|((d1, d2, d3, d4), _)| {
                        g.data1() == *d1 && g.data2() == *d2 && g.data3() == *d3 && g.data4() == d4
                    })
                    .map(|(_, s)| *s)
            }),
            _ => None,
        };
        let Some(set) = set else { continue };
        let key = match e.id() {
            NamedPropertyId::Number(n) => Named::Id(set, n),
            NamedPropertyId::StringOffset(off) => match string_at(&strings, off) {
                Some(s) => Named::Name(set, s),
                None => continue,
            },
        };
        out.insert(e.prop_id(), key);
    }
    NamedMap(out)
}

/// Cadena del flujo de nombres (longitud u32 + UTF-16).
fn string_at(buf: &[u8], off: u32) -> Option<String> {
    let at = usize::try_from(off).ok()?;
    let len = usize::try_from(u32::from_le_bytes(buf.get(at..at.checked_add(4)?)?.try_into().ok()?)).ok()?;
    let start = at.checked_add(4)?;
    let bytes = buf.get(start..start.checked_add(len.min(4096))?)?;
    let units: Vec<u16> = bytes.chunks_exact(2).filter_map(|c| c.try_into().ok().map(u16::from_le_bytes)).collect();
    Some(String::from_utf16_lossy(&units))
}

fn text8(b: &[u8]) -> String {
    let b = b.split(|&c| c == 0).next().unwrap_or(&[]);
    crate::util::decode_text(b)
}

fn text16(b: &[u16]) -> String {
    String::from_utf16_lossy(b).trim_end_matches('\0').to_string()
}

/// `PropertyValue` → [`Prop`].
fn convert(v: &PropertyValue) -> Option<Prop> {
    Some(match v {
        PropertyValue::Integer16(i) => Prop::Int(i64::from(*i)),
        PropertyValue::Integer32(i) => Prop::Int(i64::from(*i)),
        PropertyValue::Integer64(i) => Prop::Int(*i),
        PropertyValue::Boolean(b) => Prop::Bool(*b),
        PropertyValue::Time(t) => Prop::Time(*t),
        PropertyValue::Floating32(f) => Prop::Float(f64::from(*f)),
        PropertyValue::Floating64(f) => Prop::Float(*f),
        PropertyValue::String8(s) => Prop::Str(text8(s.buffer())),
        PropertyValue::Unicode(s) => Prop::Str(text16(s.buffer())),
        PropertyValue::Binary(b) => Prop::Bin(b.buffer().to_vec()),
        PropertyValue::MultipleString8(v) => Prop::Strs(v.iter().take(1_000).map(|s| text8(s.buffer())).collect()),
        PropertyValue::MultipleUnicode(v) => Prop::Strs(v.iter().take(1_000).map(|s| text16(s.buffer())).collect()),
        _ => return None,
    })
}

fn split_props<'a>(
    iter: impl Iterator<Item = (&'a u16, &'a PropertyValue)>,
    named: &NamedMap,
) -> (BTreeMap<u16, Prop>, HashMap<Named, Prop>) {
    let mut props = BTreeMap::new();
    let mut nprops = HashMap::new();
    for (id, v) in iter.take(100_000) {
        let Some(p) = convert(v) else { continue };
        if *id >= 0x8000 {
            if let Some(k) = named.0.get(id) {
                nprops.insert(k.clone(), p);
            }
        } else {
            props.insert(*id, p);
        }
    }
    (props, nprops)
}

/// Columna de una fila de tabla.
fn column(table: &dyn TableContext, row: &outlook_pst::ltp::table_context::TableRowData, id: u16) -> Option<Prop> {
    let ctx = table.context();
    let cols = row.columns(ctx).ok()?;
    let (i, col) = ctx.columns().iter().enumerate().find(|(_, c)| c.prop_id() == id)?;
    let value = cols.get(i)?.as_ref()?;
    convert(&table.read_column(value, col.prop_type()).ok()?)
}

fn recipients_of(msg: &dyn Message) -> Vec<Recipient> {
    let Some(table) = msg.recipient_table() else { return Vec::new() };
    let mut out = Vec::new();
    for row in table.rows_matrix().take(MAX_RECIPIENTS) {
        let s = |id| match column(table.as_ref(), row, id) {
            Some(Prop::Str(s)) => s,
            _ => String::new(),
        };
        let n = |id| match column(table.as_ref(), row, id) {
            Some(Prop::Int(i)) => i,
            _ => 0,
        };
        let smtp = s(0x39FE);
        let email = if smtp.contains('@') { smtp } else { s(0x3003) };
        let email = if email.contains('@') { email } else { String::new() };
        out.push(Recipient { kind: n(0x0C15), name: s(0x3001), email, track: n(0x5FFF) });
    }
    out
}

fn item_of(msg: &dyn Message, named: &NamedMap) -> PstItem {
    let (props, nprops) = split_props(msg.properties().iter(), named);
    let class = match props.get(&0x001A) {
        Some(Prop::Str(s)) => s.clone(),
        _ => String::new(),
    };
    PstItem { class, props, named: nprops, recipients: recipients_of(msg), attachments: Vec::new() }
}

/// Adjunto → [`Attachment`]; `Err(nombre)` si supera el tamaño máximo.
fn attachment_of(
    a: &dyn outlook_pst::messaging::attachment::Attachment,
    named: &NamedMap,
    max: u64,
) -> std::result::Result<Option<Attachment>, String> {
    let (props, _) = split_props(a.properties().iter(), named);
    let s = |id| match props.get(&id) {
        Some(Prop::Str(s)) => s.clone(),
        _ => String::new(),
    };
    let flag = |id| matches!(props.get(&id), Some(Prop::Bool(true)) | Some(Prop::Int(1)));
    let mut filename = s(0x3707);
    if filename.is_empty() {
        filename = s(0x3704);
    }
    if filename.is_empty() {
        filename = s(0x3001);
    }
    let mut out = Attachment {
        filename,
        mime: s(0x370E),
        content_id: Some(s(0x3712)).filter(|c| !c.is_empty()),
        contact_photo: flag(0x7FFF),
        hidden: flag(0x7FFE),
        ..Default::default()
    };
    match a.data() {
        Some(AttachmentData::Binary(b)) => {
            if b.buffer().len() as u64 > max {
                return Err(out.filename);
            }
            out.data = b.buffer().to_vec();
        }
        Some(AttachmentData::Message(m)) => {
            let mut inner = item_of(m.as_ref(), named);
            inner.attachments.clear();
            out.embedded = Some(Box::new(inner));
        }
        None => return Ok(None),
    }
    Ok(Some(out))
}

/// Carpeta del PST a importar.
struct FolderInfo {
    entry: EntryId,
    node: u32,
    path: Vec<String>,
    class: String,
    count: u64,
}

fn node_of_entry_bytes(b: &[u8]) -> Option<u32> {
    // ENTRYID: rgbFlags(4) + uid del proveedor(16) + nid(4).
    let at = b.len().checked_sub(4)?;
    Some(u32::from_le_bytes(b.get(at..)?.try_into().ok()?))
}

fn collect_folders(store: &Rc<dyn Store>) -> std::io::Result<Vec<FolderInfo>> {
    let root = store.properties().ipm_sub_tree_entry_id()?;
    let mut out = Vec::new();
    let mut stack: Vec<(EntryId, Vec<String>, usize)> = vec![(root, Vec::new(), 0)];
    while let Some((entry, path, depth)) = stack.pop() {
        let Ok(folder) = store.open_folder(&entry) else { continue };
        let props = folder.properties();
        if depth > 0 {
            let class = match props.get(0x3613).and_then(convert) {
                Some(Prop::Str(s)) => s,
                _ => String::new(),
            };
            let count = folder.contents_table().map(|t| t.rows_matrix().count() as u64).unwrap_or(0);
            out.push(FolderInfo { entry, node: u32::from(props.node_id()), path: path.clone(), class, count });
            if out.len() >= MAX_FOLDERS {
                break;
            }
        }
        if depth >= MAX_DEPTH {
            continue;
        }
        let Some(h) = folder.hierarchy_table() else { continue };
        let mut children = Vec::new();
        for row in h.rows_matrix().take(10_000) {
            let node = NodeId::from(u32::from(row.id()));
            let Ok(child) = store.properties().make_entry_id(node) else { continue };
            let Ok(cf) = store.open_folder(&child) else { continue };
            let cp = cf.properties();
            if matches!(cp.get(0x10F4), Some(PropertyValue::Boolean(true))) {
                continue;
            }
            let name = cp.display_name().unwrap_or_default().trim_end_matches('\0').to_string();
            let mut p = path.clone();
            p.push(if name.is_empty() { format!("{}", u32::from(node)) } else { name });
            children.push((child, p, depth.saturating_add(1)));
        }
        for c in children.into_iter().rev() {
            stack.push(c);
        }
    }
    Ok(out)
}

/// Función de una carpeta según las entradas especiales del almacén.
fn special_roles(store: &Rc<dyn Store>) -> HashMap<u32, FolderRole> {
    let mut out = HashMap::new();
    if let Ok(e) = store.properties().ipm_wastebasket_entry_id() {
        out.insert(u32::from(e.node_id()), FolderRole::Trash);
    }
    for (id, role) in [(0x35E4u16, FolderRole::Sent), (0x36D7, FolderRole::Drafts)] {
        if let Some(PropertyValue::Binary(b)) = store.properties().get(id)
            && let Some(n) = node_of_entry_bytes(b.buffer())
        {
            out.insert(n, role);
        }
    }
    out
}

/// Importa un `.pst`/`.ost`.
pub(crate) fn import_pst(sink: &mut Sink<'_>, path: &Path) -> Result<()> {
    let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let unit = unit_key("pst", path);
    if sink.checkpoint.is_done(&unit) {
        sink.report.resumed = true;
        sink.advance(file_size);
        return Ok(());
    }
    let is_ost = crate::util::ext(path) == "ost";
    let opened = catch_unwind(|| Pst::open(path));
    let pst = match opened {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => {
            if is_ost {
                sink.notice(Notice::OstUnsupported { path: path.to_path_buf() });
            } else {
                sink.skip(path.display().to_string(), SkipReason::Malformed { detail: e.to_string() });
            }
            sink.advance(file_size);
            return Ok(());
        }
        Err(_) => {
            if is_ost {
                sink.notice(Notice::OstUnsupported { path: path.to_path_buf() });
            } else {
                sink.skip(path.display().to_string(), SkipReason::Damaged);
            }
            sink.advance(file_size);
            return Ok(());
        }
    };
    let store = pst.store();
    let named = catch_unwind(AssertUnwindSafe(|| named_map(store.as_ref()))).unwrap_or_default();
    let folders = match catch_unwind(AssertUnwindSafe(|| collect_folders(&store))) {
        Ok(Ok(f)) => f,
        Ok(Err(e)) => {
            sink.skip(path.display().to_string(), SkipReason::Malformed { detail: e.to_string() });
            return Ok(());
        }
        Err(_) => {
            sink.skip(path.display().to_string(), SkipReason::Damaged);
            return Ok(());
        }
    };
    let roles = special_roles(&store);
    let stem = crate::pim::stem_name(path, "Outlook");
    let store_name = store.properties().display_name().ok().map(|s| s.trim_end_matches('\0').trim().to_string());
    let label = match store_name.filter(|n| !n.is_empty() && n != "Personal Folders" && n != "Carpetas personales") {
        Some(n) => format!("Outlook · {n}"),
        None => format!("Outlook · {stem}"),
    };
    let account = sink.account(&label, "")?;
    let total: u64 = folders.iter().map(|f| f.count).sum::<u64>().max(1);
    let per_item = file_size / total;
    let max_att = sink.opts.max_message_bytes;
    let tz = sink.opts.time_zone;
    let mut modified_total = 0u64;
    for f in &folders {
        sink.check_cancel()?;
        let key = format!("{unit}#{}", f.node);
        if sink.checkpoint.is_done(&key) {
            sink.report.resumed = true;
            sink.advance(per_item.saturating_mul(f.count));
            continue;
        }
        let folder_name = f.path.last().cloned().unwrap_or_default();
        sink.set_current(format!("{label} › {}", f.path.join(" › ")));
        let mut mail_folder = None;
        let rows: Vec<u32> = match catch_unwind(AssertUnwindSafe(|| {
            store
                .open_folder(&f.entry)
                .ok()
                .and_then(|fo| fo.contents_table().map(|t| t.rows_matrix().map(|r| u32::from(r.id())).collect()))
                .unwrap_or_default()
        })) {
            Ok(v) => v,
            Err(_) => {
                sink.skip(format!("{} › {}", path.display(), f.path.join(" › ")), SkipReason::Damaged);
                continue;
            }
        };
        for (n, row) in rows.into_iter().enumerate() {
            sink.check_cancel()?;
            sink.advance(per_item);
            let loc = || format!("{} › {} › {}", path.display(), f.path.join(" › "), n.saturating_add(1));
            let Ok(entry) = store.properties().make_entry_id(NodeId::from(row)) else { continue };
            let read = catch_unwind(AssertUnwindSafe(|| pst.read_item(&entry, &named, max_att)));
            let (item, dropped) = match read {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => {
                    sink.skip(loc(), SkipReason::Malformed { detail: e.to_string() });
                    continue;
                }
                Err(_) => {
                    sink.skip(loc(), SkipReason::Damaged);
                    continue;
                }
            };
            for name in dropped {
                sink.skip(
                    format!("{} › {name}", loc()),
                    SkipReason::TooLarge { size: max_att.saturating_add(1), max: max_att },
                );
            }
            let class = item.class.to_ascii_lowercase();
            if class.starts_with("ipm.contact") {
                let (c, photo) = map::contact(&item);
                sink.contact(account, &folder_name, c, photo)?;
            } else if class.starts_with("ipm.distlist") {
                sink.skip(
                    loc(),
                    SkipReason::Unsupported {
                        what: sink.opts.text("distribution list", "lista de distribución").into(),
                    },
                );
            } else if class.starts_with("ipm.appointment") {
                let (ev, modified) = map::event(&item, tz);
                modified_total = modified_total.saturating_add(u64::from(modified > 0));
                sink.event(account, &folder_name, ev)?;
            } else if class.starts_with("ipm.task") && !class.starts_with("ipm.taskrequest") {
                sink.task(account, &folder_name, map::task(&item))?;
            } else if class.starts_with("ipm.stickynote") {
                sink.note(map::note(&item))?;
            } else if class.starts_with("ipm.activity") {
                sink.skip(
                    loc(),
                    SkipReason::Unsupported { what: sink.opts.text("journal entry", "entrada del diario").into() },
                );
            } else {
                let folder = match mail_folder {
                    Some(id) => id,
                    None => {
                        let role = if f.path.len() == 1 { roles.get(&f.node).copied() } else { None };
                        let id = sink.folder(account, &f.path, role)?;
                        mail_folder = Some(id);
                        id
                    }
                };
                let raw = map::to_rfc822(&item, 0);
                let meta = MsgMeta {
                    flags: Some(map::flags(&item)),
                    keywords: item.categories(),
                    received: item.received(),
                    dedup: Some(map::dedup_key(&item)),
                };
                sink.message(folder, raw, meta, &loc)?;
            }
        }
        if mail_folder.is_none() && f.count == 0 && f.class.to_ascii_lowercase().starts_with("ipf.note") {
            // Carpeta de correo vacía: se crea para conservar el árbol.
            let role = if f.path.len() == 1 { roles.get(&f.node).copied() } else { None };
            sink.folder(account, &f.path, role)?;
        }
        sink.checkpoint.stage_done(key);
        sink.flush()?;
    }
    if modified_total > 0 {
        sink.notice(Notice::RecurrenceExceptionsDropped { count: modified_total });
    }
    sink.checkpoint.stage_done(unit);
    sink.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_node_and_strings() {
        let mut e = vec![0u8; 20];
        e.extend(0x0000_8022u32.to_le_bytes());
        assert_eq!(node_of_entry_bytes(&e), Some(0x8022));
        assert_eq!(node_of_entry_bytes(&[1, 2]), None);
        let mut s = 8u32.to_le_bytes().to_vec();
        for u in "Keys".encode_utf16() {
            s.extend(u.to_le_bytes());
        }
        assert_eq!(string_at(&s, 0).as_deref(), Some("Keys"));
        assert_eq!(string_at(&s, 100), None);
        assert_eq!(text8(b"Pe\xf1a\0basura"), "Peña");
    }
}
