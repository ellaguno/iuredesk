//! Gmail con `X-GM-EXT-1`: las carpetas son etiquetas. Un mensaje con varias etiquetas es un solo
//! mensaje local (`X-GM-MSGID`) que pertenece a varias carpetas; archivar es quitar `\Inbox`,
//! mover a una etiqueta es ponerla y quitar la de la carpeta de origen; solo la Papelera y el
//! Spam son movimientos de verdad (Gmail quita entonces todas las etiquetas).

use iuredesk_model::{Folder, FolderId, FolderRole, MessageEnvelope, MessageId};
use iuredesk_store::Store;

use crate::error::{Result, SyncError};
use crate::ops::{MailOp, enqueue_op};
use crate::state::{add_adopt, is_gmail_labels};

/// Id del proveedor de un mensaje de Gmail.
pub(crate) fn provider_id(msgid: u64) -> String {
    format!("gm:{msgid}")
}

/// Pista de hilo de Gmail.
pub(crate) fn thread_hint(thrid: u64) -> String {
    format!("gmthr:{thrid}")
}

/// ¿Es una de las carpetas de sistema de Gmail (`[Gmail]/…`, `[Google Mail]/…`)?
fn is_system(path: &str) -> bool {
    path.starts_with("[Gmail]/") || path.starts_with("[Google Mail]/")
}

/// Etiqueta de Gmail de una carpeta (`None` para «Todos», Papelera y Spam, que no son etiquetas).
pub(crate) fn folder_label(f: &Folder) -> Option<String> {
    match f.role {
        FolderRole::Inbox => Some("\\Inbox".into()),
        FolderRole::Sent => Some("\\Sent".into()),
        FolderRole::Drafts => Some("\\Draft".into()),
        FolderRole::Archive | FolderRole::Trash | FolderRole::Junk => None,
        FolderRole::Custom if is_system(&f.remote_path) => {
            let leaf = f.remote_path.rsplit('/').next().unwrap_or_default().to_ascii_lowercase();
            match leaf.as_str() {
                "important" | "importantes" => Some("\\Important".into()),
                "starred" | "destacados" => Some("\\Starred".into()),
                _ => None,
            }
        }
        FolderRole::Custom => Some(f.remote_path.clone()),
    }
}

/// Etiquetas de usuario (las de sistema, `\Inbox`, `\Important`…, no se muestran como etiquetas).
pub(crate) fn user_labels(labels: &[String]) -> Vec<String> {
    labels.iter().filter(|l| !l.starts_with('\\') && !l.is_empty()).take(500).cloned().collect()
}

/// Copia del mensaje con UID conocido sobre la que hacer `STORE` (preferida: «Todos»).
fn handle(store: &Store, msg: MessageId) -> Result<Option<(String, u32)>> {
    let mut best: Option<(String, u32, bool)> = None;
    for (fid, remote) in store.memberships(msg)? {
        let Some(uid) = remote.and_then(|r| r.parse::<u32>().ok()).filter(|u| *u > 0) else { continue };
        let Some(f) = store.get_folder(fid)? else { continue };
        let all = f.role == FolderRole::Archive;
        if best.as_ref().is_none_or(|b| all && !b.2) {
            best = Some((f.remote_path, uid, all));
        }
    }
    Ok(best.map(|(p, u, _)| (p, u)))
}

fn member_remote(store: &Store, msg: MessageId, folder: FolderId) -> Result<Option<Option<String>>> {
    Ok(store.memberships(msg)?.into_iter().find(|(f, _)| *f == folder).map(|(_, r)| r))
}

/// «Mover» en Gmail.
pub(crate) fn move_message(store: &Store, env: &MessageEnvelope, from: &Folder, dest: &Folder) -> Result<Option<u64>> {
    let msg = env.id;
    if matches!(dest.role, FolderRole::Trash | FolderRole::Junk) {
        // Movimiento real: Gmail quita todas las etiquetas.
        let h = handle(store, msg)?;
        store.move_message(msg, dest.id)?;
        store.clear_secondary(msg)?;
        let Some((path, uid)) = h else { return Ok(None) };
        add_adopt(store, dest.id, msg)?;
        let op = MailOp::Move { msg, from: path, uid, to: dest.remote_path.clone(), to_folder: dest.id };
        return Ok(Some(enqueue_op(store, from.account, &op)?));
    }
    let members = store.memberships(msg)?;
    // Etiqueta que se deja: la de la carpeta principal si es etiqueta; si no, la bandeja de entrada.
    let source: Option<Folder> = if from.id != dest.id && folder_label(from).is_some() {
        Some(from.clone())
    } else {
        let mut found = None;
        for (fid, _) in &members {
            if let Some(f) = store.get_folder(*fid)?
                && f.role == FolderRole::Inbox
                && f.id != dest.id
            {
                found = Some(f);
            }
        }
        found
    };
    let add = folder_label(dest);
    let remove = source.as_ref().and_then(folder_label);
    if add.is_none() && remove.is_none() {
        return Ok(None);
    }
    let h = handle(store, msg)?;
    let newly_added = !members.iter().any(|(f, _)| *f == dest.id);
    let removed = match &source {
        Some(s) => member_remote(store, msg, s.id)?.map(|r| (s.id, r)),
        None => None,
    };
    if newly_added {
        store.add_to_folder(msg, dest.id, None)?;
    }
    if let Some(s) = &source {
        store.remove_from_folder(msg, s.id)?;
    }
    let Some((path, uid)) = h else { return Ok(None) };
    let op = MailOp::Labels {
        msg,
        folder: path,
        uid,
        add: add.into_iter().collect(),
        remove: remove.into_iter().collect(),
        added_folder: newly_added.then_some(dest.id),
        removed_folder: removed,
    };
    Ok(Some(enqueue_op(store, from.account, &op)?))
}

/// Pone o quita una etiqueta sin tocar las demás.
pub(crate) fn change_label(store: &Store, msg: MessageId, label: FolderId, add: bool) -> Result<Option<u64>> {
    let env = store.get_envelope(msg)?.ok_or_else(|| SyncError::MessageNotFound(msg.to_string()))?;
    let folder = store.get_folder(label)?.ok_or_else(|| SyncError::MessageNotFound(label.to_string()))?;
    if !is_gmail_labels(store, folder.account) || folder.account != env.account {
        return Err(SyncError::NotALabel(folder.name));
    }
    let name = folder_label(&folder).ok_or_else(|| SyncError::NotALabel(folder.name.clone()))?;
    let members = store.memberships(msg)?;
    let member = members.iter().find(|(f, _)| *f == label).map(|(_, r)| r.clone());
    if add == member.is_some() {
        return Ok(None);
    }
    let h = handle(store, msg)?;
    if add {
        store.add_to_folder(msg, label, None)?;
    } else {
        if members.len() <= 1 {
            // Sin etiquetas el mensaje sigue en «Todos».
            let all = store.folders(env.account)?.into_iter().find(|f| f.role == FolderRole::Archive);
            if let Some(all) = all {
                store.add_to_folder(msg, all.id, None)?;
            }
        }
        store.remove_from_folder(msg, label)?;
    }
    let Some((path, uid)) = h else { return Ok(None) };
    let (a, r) = if add { (vec![name], Vec::new()) } else { (Vec::new(), vec![name]) };
    let op = MailOp::Labels {
        msg,
        folder: path,
        uid,
        add: a,
        remove: r,
        added_folder: add.then_some(label),
        removed_folder: if add { None } else { member.map(|r| (label, r)) },
    };
    Ok(Some(enqueue_op(store, env.account, &op)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use iuredesk_model::AccountId;

    fn f(role: FolderRole, path: &str) -> Folder {
        Folder { account: AccountId(1), role, remote_path: path.into(), name: path.into(), ..Default::default() }
    }

    #[test]
    fn labels_of_folders() {
        assert_eq!(folder_label(&f(FolderRole::Inbox, "INBOX")).as_deref(), Some("\\Inbox"));
        assert_eq!(folder_label(&f(FolderRole::Archive, "[Gmail]/All Mail")), None);
        assert_eq!(folder_label(&f(FolderRole::Trash, "[Gmail]/Trash")), None);
        assert_eq!(folder_label(&f(FolderRole::Custom, "Trabajo/2026")).as_deref(), Some("Trabajo/2026"));
        assert_eq!(folder_label(&f(FolderRole::Custom, "[Gmail]/Important")).as_deref(), Some("\\Important"));
        assert_eq!(user_labels(&["\\Inbox".into(), "Trabajo".into()]), vec!["Trabajo".to_string()]);
        assert_eq!(provider_id(7), "gm:7");
        assert_eq!(thread_hint(9), "gmthr:9");
    }
}
