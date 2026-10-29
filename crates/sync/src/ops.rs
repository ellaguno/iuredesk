//! Acciones locales primero (offline-first): se aplican al almacén al instante y se encolan para
//! replicarlas en el servidor (PLAN.md §4). La carga de cada operación es [`MailOp`] en JSON.

use std::collections::HashSet;

use chrono::Utc;
use iuredesk_model::{AccountId, Flags, Folder, FolderId, FolderRole, MessageId};
use iuredesk_store::{NewMessage, Store};
use serde::{Deserialize, Serialize};

pub use iuredesk_compose::ops::{OP_SEND, OP_SYNC, SyncOp};

use crate::error::{Result, SyncError};
use crate::parse::{envelope_from_raw, imap_flags};
use crate::state::add_adopt;

/// Operación pendiente de replicar.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum MailOp {
    /// Cambiar banderas.
    Flags {
        /// Mensaje local.
        msg: MessageId,
        /// Buzón remoto.
        folder: String,
        /// UID.
        uid: u32,
        /// Banderas a poner.
        add: Vec<String>,
        /// Banderas a quitar.
        remove: Vec<String>,
    },
    /// Mover a otra carpeta.
    Move {
        /// Mensaje local (ya en la carpeta destino).
        msg: MessageId,
        /// Buzón de origen.
        from: String,
        /// UID en el origen.
        uid: u32,
        /// Buzón destino.
        to: String,
        /// Carpeta local destino.
        to_folder: FolderId,
    },
    /// Borrar definitivamente.
    Expunge {
        /// Buzón.
        folder: String,
        /// UID.
        uid: u32,
    },
    /// Subir un mensaje local (borrador, enviado).
    Append {
        /// Mensaje local (su crudo está en el almacén de blobs).
        msg: MessageId,
        /// Buzón destino.
        folder: String,
        /// Carpeta local.
        folder_id: FolderId,
        /// Banderas.
        flags: Vec<String>,
    },
    /// Poner o quitar etiquetas de Gmail (`UID STORE ±X-GM-LABELS`) sobre una copia conocida.
    Labels {
        /// Mensaje local.
        msg: MessageId,
        /// Buzón donde se conoce el UID.
        folder: String,
        /// UID en ese buzón.
        uid: u32,
        /// Etiquetas a poner (`\\Inbox`, `Trabajo`…).
        add: Vec<String>,
        /// Etiquetas a quitar.
        remove: Vec<String>,
        /// Carpeta local que se añadió (para deshacer).
        #[serde(default)]
        added_folder: Option<FolderId>,
        /// Carpeta local que se quitó (para deshacer) y el UID que tenía allí.
        #[serde(default)]
        removed_folder: Option<(FolderId, Option<String>)>,
    },
    /// Banderas de un mensaje con id remoto opaco (Microsoft Graph).
    RemoteFlags {
        /// Mensaje local.
        msg: MessageId,
        /// Id remoto.
        remote: String,
        /// Banderas a poner (nombres IMAP: `\\Seen`, `\\Flagged`).
        add: Vec<String>,
        /// Banderas a quitar.
        remove: Vec<String>,
    },
    /// Mover un mensaje con id remoto opaco (Graph `move`).
    RemoteMove {
        /// Mensaje local (ya en la carpeta destino).
        msg: MessageId,
        /// Id remoto.
        remote: String,
        /// Carpeta remota destino (id de Graph).
        to: String,
        /// Carpeta local destino.
        to_folder: FolderId,
    },
    /// Borrar definitivamente un mensaje con id remoto opaco.
    RemoteDelete {
        /// Id remoto.
        remote: String,
    },
    /// Crear una carpeta (F7.1). IMAP: `CREATE path`; Graph: `mailFolders` con `name` dentro de
    /// `parent` (id remoto) y se adopta el id que da el servidor.
    CreateFolder {
        /// Carpeta local (ya creada).
        folder_id: FolderId,
        /// Ruta IMAP (UTF-7 modificado, con el separador del servidor).
        path: String,
        /// Nombre visible.
        name: String,
        /// Carpeta padre remota (Graph), si la hay.
        #[serde(default)]
        parent: Option<String>,
    },
    /// Cambiar el nombre de una carpeta. IMAP: `RENAME from to`; Graph: `displayName`.
    RenameFolder {
        /// Carpeta local.
        folder_id: FolderId,
        /// Ruta (IMAP) o id (Graph) actual.
        from: String,
        /// Ruta IMAP nueva.
        to: String,
        /// Nombre visible nuevo.
        name: String,
    },
    /// Borrar una carpeta en el servidor (ya borrada en local). IMAP: `DELETE`; Graph: `DELETE`.
    DeleteFolder {
        /// Ruta (IMAP) o id (Graph).
        remote: String,
    },
}

impl MailOp {
    /// Etiqueta corta para la cola.
    pub fn kind(&self) -> &'static str {
        match self {
            MailOp::Flags { .. } => "flags",
            MailOp::Move { .. } => "move",
            MailOp::Expunge { .. } => "expunge",
            MailOp::Append { .. } => "append",
            MailOp::Labels { .. } => "labels",
            MailOp::RemoteFlags { .. } => "remote_flags",
            MailOp::RemoteMove { .. } => "remote_move",
            MailOp::RemoteDelete { .. } => "remote_delete",
            MailOp::CreateFolder { .. } => "create_folder",
            MailOp::RenameFolder { .. } => "rename_folder",
            MailOp::DeleteFolder { .. } => "delete_folder",
        }
    }

    /// Mensaje local afectado.
    pub fn message(&self) -> Option<MessageId> {
        match self {
            MailOp::Flags { msg, .. }
            | MailOp::Move { msg, .. }
            | MailOp::Append { msg, .. }
            | MailOp::Labels { msg, .. }
            | MailOp::RemoteFlags { msg, .. }
            | MailOp::RemoteMove { msg, .. } => Some(*msg),
            MailOp::Expunge { .. }
            | MailOp::RemoteDelete { .. }
            | MailOp::CreateFolder { .. }
            | MailOp::RenameFolder { .. }
            | MailOp::DeleteFolder { .. } => None,
        }
    }

    /// Serializa.
    pub fn encode(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self).map_err(|e| SyncError::Store(e.to_string()))
    }

    /// Interpreta (`None` si la carga está dañada).
    pub fn decode(payload: &[u8]) -> Option<Self> {
        serde_json::from_slice(payload).ok()
    }
}

fn enqueue(store: &Store, account: AccountId, op: &MailOp) -> Result<u64> {
    Ok(store.enqueue_op(account, op.kind(), &op.encode()?)?)
}

fn uid_of(remote: Option<&str>) -> Option<u32> {
    remote.and_then(|r| r.parse().ok()).filter(|u| *u > 0)
}

/// Id remoto opaco (no es un UID de IMAP): Microsoft Graph, POP3.
fn opaque_of(remote: Option<&str>) -> Option<String> {
    remote.filter(|r| !r.is_empty() && r.parse::<u32>().is_err()).map(str::to_string)
}

/// Mensajes con operaciones pendientes en una cuenta (el servidor no los pisa: PLAN.md, conflicto
/// «gana el servidor salvo operación local pendiente»).
pub fn pending_messages(store: &Store, account: AccountId) -> HashSet<MessageId> {
    store
        .peek_ops(10_000)
        .unwrap_or_default()
        .into_iter()
        .filter(|o| o.account == account)
        .filter_map(|o| match classify(&o.kind, &o.payload) {
            Some(Some(QueueWork::Mail(m))) => m.message(),
            Some(Some(QueueWork::Compose(SyncOp::MarkAnswered { message } | SyncOp::MarkForwarded { message }))) => {
                Some(message)
            }
            _ => None,
        })
        .collect()
}

/// Trabajo de la cola que le toca a `sync`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueueWork {
    /// Operación de esta capa ([`MailOp`]).
    Mail(MailOp),
    /// Operación del redactor (`mail.sync`, contrato en `iuredesk_compose::ops`).
    Compose(SyncOp),
}

/// Clasifica una operación de la cola: `None` si no es de `sync` (p. ej. `outbox.send`, que
/// consume la bandeja de salida: se deja en la cola); `Some(None)` si es nuestra pero está
/// dañada (se descarta).
pub fn classify(kind: &str, payload: &[u8]) -> Option<Option<QueueWork>> {
    match kind {
        OP_SEND => None,
        OP_SYNC => Some(SyncOp::decode(payload).map(QueueWork::Compose)),
        "flags" | "move" | "expunge" | "append" | "labels" | "remote_flags" | "remote_move" | "remote_delete"
        | "create_folder" | "rename_folder" | "delete_folder" => Some(MailOp::decode(payload).map(QueueWork::Mail)),
        _ => None,
    }
}

/// Cambia las banderas en local y encola el cambio. Devuelve el número de la operación
/// encolada (`None` si no hizo falta: sin cambios o mensaje que aún no está en el servidor), que
/// sirve para cancelarla al deshacer antes de que salga.
pub fn set_flags(store: &Store, msg: MessageId, flags: Flags) -> Result<Option<u64>> {
    let env = store.get_envelope(msg)?.ok_or_else(|| SyncError::MessageNotFound(msg.to_string()))?;
    let old = env.flags;
    if old == flags {
        return Ok(None);
    }
    store.set_flags(msg, flags)?;
    let (new_l, old_l) = (imap_flags(flags), imap_flags(old));
    let add: Vec<String> = new_l.iter().filter(|f| !old_l.contains(f)).cloned().collect();
    let remove: Vec<String> = old_l.iter().filter(|f| !new_l.contains(f)).cloned().collect();
    let folder = store.get_folder(env.folder)?.ok_or_else(|| SyncError::MessageNotFound(msg.to_string()))?;
    if let Some(remote) = opaque_of(env.remote_id.as_deref()) {
        return Ok(Some(enqueue(store, folder.account, &MailOp::RemoteFlags { msg, remote, add, remove })?));
    }
    let Some(uid) = uid_of(env.remote_id.as_deref()) else { return Ok(None) };
    let seq = enqueue(store, folder.account, &MailOp::Flags { msg, folder: folder.remote_path, uid, add, remove })?;
    Ok(Some(seq))
}

/// Mueve en local y encola el movimiento. Devuelve el número de la operación encolada.
pub fn move_message(store: &Store, msg: MessageId, to: FolderId) -> Result<Option<u64>> {
    let env = store.get_envelope(msg)?.ok_or_else(|| SyncError::MessageNotFound(msg.to_string()))?;
    if env.folder == to {
        return Ok(None);
    }
    let from = store.get_folder(env.folder)?.ok_or_else(|| SyncError::MessageNotFound(msg.to_string()))?;
    let dest = store.get_folder(to)?.ok_or_else(|| SyncError::MessageNotFound(to.to_string()))?;
    if crate::state::is_gmail_labels(store, from.account) {
        return crate::gmail::move_message(store, &env, &from, &dest);
    }
    if let Some(remote) = opaque_of(env.remote_id.as_deref()) {
        // Graph con ids inmutables: el id no cambia al mover.
        store.move_message(msg, to)?;
        store.set_remote_id(msg, Some(remote.clone()))?;
        let op = MailOp::RemoteMove { msg, remote, to: dest.remote_path, to_folder: to };
        return Ok(Some(enqueue(store, from.account, &op)?));
    }
    store.move_message(msg, to)?;
    let mut seq = None;
    if let Some(uid) = uid_of(env.remote_id.as_deref()) {
        seq = Some(enqueue(
            store,
            from.account,
            &MailOp::Move { msg, from: from.remote_path, uid, to: dest.remote_path, to_folder: to },
        )?);
        add_adopt(store, to, msg)?;
    }
    Ok(seq)
}

/// Borra en local y encola el borrado definitivo en el servidor.
pub fn delete_message(store: &Store, msg: MessageId) -> Result<Option<u64>> {
    let Some(env) = store.get_envelope(msg)? else { return Ok(None) };
    let folder = store.get_folder(env.folder)?;
    store.delete_message(msg)?;
    if let (Some(remote), Some(folder)) = (opaque_of(env.remote_id.as_deref()), folder.as_ref()) {
        return Ok(Some(enqueue(store, folder.account, &MailOp::RemoteDelete { remote })?));
    }
    if let (Some(uid), Some(folder)) = (uid_of(env.remote_id.as_deref()), folder) {
        return Ok(Some(enqueue(store, folder.account, &MailOp::Expunge { folder: folder.remote_path, uid })?));
    }
    Ok(None)
}

/// Deshace un movimiento que aún no salió de la máquina: cancela la operación `seq`, devuelve
/// el mensaje a `folder` con el UID que tenía allí y lo saca de la espera de UID del destino.
pub fn cancel_move(store: &Store, seq: u64, msg: MessageId, folder: FolderId, remote_id: Option<String>) -> Result<()> {
    let queued = store.peek_ops(10_000)?.into_iter().find(|o| o.seq == seq).and_then(|o| MailOp::decode(&o.payload));
    if let Some(MailOp::Labels { added_folder, removed_folder, .. }) = queued {
        // Etiquetas de Gmail: se deshace sobre las pertenencias, no moviendo.
        store.ack_op(seq)?;
        if let Some((f, r)) = removed_folder {
            store.add_to_folder(msg, f, r.as_deref())?;
        }
        if let Some(f) = added_folder
            && f != folder
        {
            store.remove_from_folder(msg, f)?;
        }
        return Ok(());
    }
    let dest = store.get_envelope(msg)?.map(|e| e.folder);
    store.ack_op(seq)?;
    store.move_message(msg, folder)?;
    store.set_remote_id(msg, remote_id)?;
    if let Some(d) = dest {
        let mut list = crate::state::adopt_list(store, d);
        list.retain(|m| *m != msg);
        crate::state::set_adopt(store, d, &list)?;
    }
    Ok(())
}

/// Pone una etiqueta de Gmail (la carpeta `label`) a un mensaje sin quitarle las demás.
pub fn add_label(store: &Store, msg: MessageId, label: FolderId) -> Result<Option<u64>> {
    crate::gmail::change_label(store, msg, label, true)
}

/// Quita una etiqueta de Gmail (la carpeta `label`) a un mensaje.
pub fn remove_label(store: &Store, msg: MessageId, label: FolderId) -> Result<Option<u64>> {
    crate::gmail::change_label(store, msg, label, false)
}

pub(crate) fn enqueue_op(store: &Store, account: AccountId, op: &MailOp) -> Result<u64> {
    enqueue(store, account, op)
}

/// Guarda un mensaje nuevo en una carpeta (borrador, enviado) y encola su subida.
pub fn append_message(store: &Store, folder: FolderId, raw: Vec<u8>, flags: Flags) -> Result<MessageId> {
    let f = store.get_folder(folder)?.ok_or_else(|| SyncError::MessageNotFound(folder.to_string()))?;
    let (mut env, text) = envelope_from_raw(&raw, folder, Utc::now());
    env.flags = flags;
    let ids = store.put_messages(vec![NewMessage { envelope: env, raw: Some(raw), body_text: Some(text) }])?;
    let id = ids.into_iter().next().ok_or_else(|| SyncError::Store("lote vacío".into()))?;
    enqueue(
        store,
        f.account,
        &MailOp::Append { msg: id, folder: f.remote_path, folder_id: folder, flags: imap_flags(flags) },
    )?;
    add_adopt(store, folder, id)?;
    Ok(id)
}

// ------------------------------------------------------------------ carpetas (F7.1)

/// Largo máximo del nombre de una carpeta.
pub const MAX_FOLDER_NAME: usize = 200;
/// Ruta provisional de una carpeta de Graph que aún no tiene id remoto.
pub const LOCAL_FOLDER_PREFIX: &str = "local-folder:";

/// Cómo nombra el servidor las carpetas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderStyle {
    /// Rutas IMAP con separador (`Clientes/2026`) en UTF-7 modificado. En Gmail son etiquetas.
    Imap {
        /// Separador de jerarquía del servidor.
        delimiter: char,
    },
    /// Ids opacos (Microsoft Graph `mailFolders`).
    Graph,
}

/// Clave del separador IMAP de una cuenta (lo guarda la lista de carpetas).
pub(crate) fn delimiter_key(acct: AccountId) -> String {
    format!("imap-delim/{}", acct.get())
}

/// Separador IMAP conocido de la cuenta (`/` si aún no se listó).
pub fn imap_delimiter(store: &Store, acct: AccountId) -> char {
    store
        .sync_state(&delimiter_key(acct))
        .ok()
        .flatten()
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|s| s.chars().next())
        .unwrap_or('/')
}

fn clean_name(name: &str, delimiter: Option<char>) -> Result<String> {
    let n: String = name.trim().chars().take(MAX_FOLDER_NAME).collect();
    let bad = n.is_empty()
        || n.chars().any(|c| c.is_control() || c == '*' || c == '%')
        || delimiter.is_some_and(|d| n.contains(d))
        || n.eq_ignore_ascii_case("INBOX");
    if bad {
        return Err(SyncError::InvalidFolderName(n));
    }
    Ok(n)
}

/// Pendientes de carpetas de una cuenta: carpetas locales que aún no existen (o no con su nombre
/// nuevo) en el servidor y rutas remotas que se están borrando o renombrando. La lista de
/// carpetas no las toca mientras tanto.
#[derive(Clone, Debug, Default)]
pub struct PendingFolders {
    /// Carpetas locales con `CREATE`/`RENAME` en cola.
    pub local: HashSet<FolderId>,
    /// Rutas o ids remotos con `DELETE`/`RENAME` (ruta vieja) en cola.
    pub remote: HashSet<String>,
}

/// Ver [`PendingFolders`].
pub fn pending_folders(store: &Store, account: AccountId) -> PendingFolders {
    let mut p = PendingFolders::default();
    for o in store.peek_ops(10_000).unwrap_or_default().into_iter().filter(|o| o.account == account) {
        match classify(&o.kind, &o.payload) {
            Some(Some(QueueWork::Mail(MailOp::CreateFolder { folder_id, .. }))) => {
                p.local.insert(folder_id);
            }
            Some(Some(QueueWork::Mail(MailOp::RenameFolder { folder_id, from, .. }))) => {
                p.local.insert(folder_id);
                p.remote.insert(from);
            }
            Some(Some(QueueWork::Mail(MailOp::DeleteFolder { remote }))) => {
                p.remote.insert(remote);
            }
            _ => {}
        }
    }
    p
}

/// Crea una carpeta (en local al instante; en el servidor por la cola). En Gmail es una
/// etiqueta. Devuelve la carpeta local.
pub fn create_folder(
    store: &Store,
    account: AccountId,
    parent: Option<FolderId>,
    name: &str,
    style: FolderStyle,
) -> Result<FolderId> {
    let delim = match style {
        FolderStyle::Imap { delimiter } => Some(delimiter),
        FolderStyle::Graph => None,
    };
    let name = clean_name(name, delim)?;
    let parent_f = match parent {
        Some(p) => Some(store.get_folder(p)?.ok_or_else(|| SyncError::MessageNotFound(p.to_string()))?),
        None => None,
    };
    if parent_f.as_ref().is_some_and(|p| p.account != account) {
        return Err(SyncError::InvalidFolderName(name));
    }
    if store.folders(account)?.iter().any(|f| f.parent == parent && f.name.eq_ignore_ascii_case(&name)) {
        return Err(SyncError::FolderExists(name));
    }
    let path = match (style, &parent_f) {
        (FolderStyle::Imap { delimiter }, Some(p)) => {
            format!("{}{delimiter}{}", p.remote_path, iuredesk_imap::mutf7::encode(&name))
        }
        (FolderStyle::Imap { .. }, None) => iuredesk_imap::mutf7::encode(&name),
        // Graph: el id llega al crearla; mientras tanto, una ruta local única.
        (FolderStyle::Graph, _) => String::new(),
    };
    let mut f = Folder { account, name: name.clone(), remote_path: path, parent, ..Default::default() };
    f.id = store.put_folder(&f)?;
    if style == FolderStyle::Graph {
        f.remote_path = format!("{LOCAL_FOLDER_PREFIX}{}", f.id.get());
        store.put_folder(&f)?;
    }
    let parent_remote = parent_f.map(|p| p.remote_path).filter(|r| !r.is_empty());
    let op = MailOp::CreateFolder {
        folder_id: f.id,
        path: f.remote_path.clone(),
        name,
        parent: if style == FolderStyle::Graph { parent_remote } else { None },
    };
    enqueue(store, account, &op)?;
    Ok(f.id)
}

/// Cambia el nombre de una carpeta propia (no las de sistema).
pub fn rename_folder(store: &Store, folder: FolderId, name: &str, style: FolderStyle) -> Result<()> {
    let mut f = store.get_folder(folder)?.ok_or_else(|| SyncError::MessageNotFound(folder.to_string()))?;
    if f.role != FolderRole::Custom {
        return Err(SyncError::SystemFolder);
    }
    let delim = match style {
        FolderStyle::Imap { delimiter } => Some(delimiter),
        FolderStyle::Graph => None,
    };
    let name = clean_name(name, delim)?;
    if f.name == name {
        return Ok(());
    }
    let from = f.remote_path.clone();
    let to = match style {
        FolderStyle::Imap { delimiter } => match from.rsplit_once(delimiter) {
            Some((p, _)) => format!("{p}{delimiter}{}", iuredesk_imap::mutf7::encode(&name)),
            None => iuredesk_imap::mutf7::encode(&name),
        },
        FolderStyle::Graph => from.clone(),
    };
    f.name = name.clone();
    store.put_folder(&f)?;
    // Aún sin crear en el servidor: se corrige la creación pendiente en lugar de renombrar.
    for o in store.peek_ops(10_000)?.into_iter().filter(|o| o.account == f.account) {
        if let Some(MailOp::CreateFolder { folder_id, parent, .. }) = MailOp::decode(&o.payload)
            && folder_id == folder
        {
            store.ack_op(o.seq)?;
            if style != FolderStyle::Graph {
                f.remote_path = to.clone();
                store.put_folder(&f)?;
            }
            let op = MailOp::CreateFolder { folder_id, path: f.remote_path.clone(), name, parent };
            enqueue(store, f.account, &op)?;
            return Ok(());
        }
    }
    enqueue(store, f.account, &MailOp::RenameFolder { folder_id: folder, from, to, name })?;
    Ok(())
}

/// Borra una carpeta propia y su contenido (en local al instante; en el servidor por la cola),
/// con sus subcarpetas locales. Si aún no existía en el servidor basta con cancelar su creación.
pub fn delete_folder(store: &Store, folder: FolderId) -> Result<()> {
    let f = store.get_folder(folder)?.ok_or_else(|| SyncError::MessageNotFound(folder.to_string()))?;
    if f.role != FolderRole::Custom {
        return Err(SyncError::SystemFolder);
    }
    let all = store.folders(f.account)?;
    let mut gone = vec![f.id];
    let mut i = 0usize;
    while let Some(id) = gone.get(i).copied() {
        i = i.saturating_add(1);
        if gone.len() > 10_000 {
            break;
        }
        let kids: Vec<FolderId> =
            all.iter().filter(|c| c.parent == Some(id) && !gone.contains(&c.id)).map(|c| c.id).collect();
        gone.extend(kids);
    }
    let queued = store.peek_ops(10_000)?.into_iter().find(|o| {
        o.account == f.account
            && matches!(MailOp::decode(&o.payload), Some(MailOp::CreateFolder { folder_id, .. }) if folder_id == folder)
    });
    for id in gone.iter().rev() {
        store.delete_folder(*id)?;
    }
    if let Some(op) = queued {
        store.ack_op(op.seq)?;
    }
    // Aunque la creación siguiera en cola pudo estar saliendo ya: borrar lo que no existe no
    // es un error (salvo en Graph sin id todavía, donde no hay nada que borrar).
    if !f.remote_path.starts_with(LOCAL_FOLDER_PREFIX) {
        enqueue(store, f.account, &MailOp::DeleteFolder { remote: f.remote_path })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use iuredesk_model::MessageEnvelope;

    #[test]
    fn local_first_then_queued() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let mk = |name: &str, role| {
            s.put_folder(&Folder {
                account: AccountId(1),
                name: name.into(),
                remote_path: name.into(),
                role,
                ..Default::default()
            })
            .unwrap()
        };
        let inbox = mk("INBOX", FolderRole::Inbox);
        let arch = mk("Archive", FolderRole::Archive);
        let env = MessageEnvelope { folder: inbox, remote_id: Some("7".into()), ..Default::default() };
        let id = s.put_message(env, b"Subject: x\r\n\r\ny").unwrap();

        set_flags(&s, id, Flags { seen: true, ..Default::default() }).unwrap();
        assert!(s.get_envelope(id).unwrap().unwrap().flags.seen);
        move_message(&s, id, arch).unwrap();
        assert_eq!(s.get_envelope(id).unwrap().unwrap().folder, arch);
        let ops = s.peek_ops(10).unwrap();
        assert_eq!(ops.len(), 2);
        assert_eq!(
            MailOp::decode(&ops[0].payload),
            Some(MailOp::Flags { msg: id, folder: "INBOX".into(), uid: 7, add: vec!["\\Seen".into()], remove: vec![] })
        );
        assert!(matches!(MailOp::decode(&ops[1].payload), Some(MailOp::Move { uid: 7, .. })));
        assert!(pending_messages(&s, AccountId(1)).contains(&id));
        assert!(pending_messages(&s, AccountId(2)).is_empty());

        let d =
            append_message(&s, arch, b"Subject: borrador\r\nMessage-ID: <d@x>\r\n\r\nhola".to_vec(), Flags::default())
                .unwrap();
        assert_eq!(s.get_envelope(d).unwrap().unwrap().subject, "borrador");
        assert_eq!(s.ops_len().unwrap(), 3);
        // Sin UID (aún no subido): borrar no encola nada.
        delete_message(&s, d).unwrap();
        assert_eq!(s.ops_len().unwrap(), 3);
        assert!(MailOp::decode(b"basura").is_none());
    }
}
