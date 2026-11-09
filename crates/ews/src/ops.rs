//! Operaciones de correo de EWS: carpetas (`SyncFolderHierarchy`, `GetFolder`, crear, renombrar,
//! borrar), elementos (`SyncFolderItems` solo con ids + `GetItem` por lotes con lo necesario para
//! la lista, `FindItem` por fecha para el historial), MIME, banderas, mover, borrar y crear desde
//! MIME (borrador, copia o envío).
//!
//! `SyncFolderItems` se pide con `IdOnly` (lo admiten todos los servidores) y los datos de la
//! lista se piden después con `GetItem` en lotes: es lo que recomienda Microsoft y evita las
//! propiedades que `SyncFolderItems` rechaza.

use base64::Engine as _;
use chrono::{DateTime, Utc};

use crate::soap::{M, T, clip, esc, folder_id, item_id, messages, single};
use crate::{Element, EwsClient, EwsError, Result};

/// Elementos por lote de `GetItem`/`MoveItem`/`DeleteItem`.
pub const BATCH: usize = 50;
/// Cambios máximos por llamada a `SyncFolderItems` (el servidor acepta hasta 512).
pub const MAX_SYNC_CHANGES: u32 = 512;
/// Tamaño máximo de un MIME.
pub const MAX_MIME: usize = 64 * 1024 * 1024;

/// Propiedades de la lista de mensajes.
const SUMMARY_FIELDS: &[&str] = &[
    "item:Subject",
    "message:From",
    "message:Sender",
    "message:ToRecipients",
    "message:CcRecipients",
    "message:ReplyTo",
    "item:DateTimeReceived",
    "item:DateTimeSent",
    "message:IsRead",
    "item:Flag",
    "item:Categories",
    "item:HasAttachments",
    "message:InternetMessageId",
    "item:ConversationId",
    "item:Size",
    "item:Preview",
    "item:IsDraft",
    "item:ParentFolderId",
];

/// Dirección (nombre y correo).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Addr {
    pub name: String,
    pub email: String,
}

/// Carpeta.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderInfo {
    pub id: String,
    pub change_key: Option<String>,
    pub parent_id: Option<String>,
    pub name: String,
    /// `IPF.Note` para correo; `None` en carpetas antiguas sin clase (también de correo).
    pub class: Option<String>,
    pub child_count: u32,
}

impl FolderInfo {
    /// ¿Es de correo? (sin clase o `IPF.Note…`; no calendario, contactos, tareas…).
    pub fn is_mail(&self) -> bool {
        self.class.as_deref().is_none_or(|c| c.is_empty() || c.starts_with("IPF.Note"))
    }
}

/// Cambios del árbol de carpetas.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HierarchySync {
    /// Creadas o cambiadas (todas las clases; [`FolderInfo::is_mail`] filtra).
    pub changed: Vec<FolderInfo>,
    /// Ids borrados.
    pub deleted: Vec<String>,
    pub state: String,
    /// Ya no quedan cambios.
    pub last: bool,
}

/// Mensaje (lo necesario para la lista).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItemInfo {
    pub id: String,
    pub change_key: Option<String>,
    pub subject: String,
    pub from: Option<Addr>,
    pub sender: Option<Addr>,
    pub to: Vec<Addr>,
    pub cc: Vec<Addr>,
    pub reply_to: Vec<Addr>,
    pub received: Option<DateTime<Utc>>,
    pub sent: Option<DateTime<Utc>>,
    pub is_read: Option<bool>,
    pub flagged: Option<bool>,
    pub categories: Vec<String>,
    pub has_attachments: bool,
    pub internet_message_id: Option<String>,
    pub conversation_id: Option<String>,
    pub size: Option<u64>,
    pub preview: Option<String>,
    pub is_draft: Option<bool>,
    pub parent_folder_id: Option<String>,
}

/// Cambios de los elementos de una carpeta (solo ids: los datos se piden con
/// [`EwsClient::get_summaries`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItemSync {
    /// Nuevos (id, change key).
    pub created: Vec<(String, Option<String>)>,
    /// Cambiados.
    pub updated: Vec<(String, Option<String>)>,
    /// Borrados (o movidos a otra carpeta).
    pub deleted: Vec<String>,
    /// Cambios de «leído» (más baratos que un `Update`).
    pub read_changes: Vec<(String, bool)>,
    pub state: String,
    pub last: bool,
}

/// Qué hacer con un mensaje creado desde MIME.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageDisposition {
    /// Guardarlo en la carpeta (borrador si `draft`, si no un mensaje leído normal).
    Save { draft: bool },
    /// Enviarlo y dejar la copia en «Elementos enviados».
    SendAndSaveCopy,
}

fn t<'a>(e: &'a Element, name: &str) -> Option<&'a Element> {
    e.child(T, name)
}

fn t_text(e: &Element, name: &str, max: usize) -> Option<String> {
    t(e, name).map(|c| clip(c.text_trim(), max)).filter(|s| !s.is_empty())
}

fn t_bool(e: &Element, name: &str) -> Option<bool> {
    t(e, name).map(|c| c.text_trim().eq_ignore_ascii_case("true"))
}

fn t_date(e: &Element, name: &str) -> Option<DateTime<Utc>> {
    t(e, name).and_then(|c| DateTime::parse_from_rfc3339(c.text_trim()).ok()).map(|d| d.with_timezone(&Utc))
}

fn mailbox(e: &Element) -> Option<Addr> {
    let name = t_text(e, "Name", 300).unwrap_or_default();
    let email = t_text(e, "EmailAddress", 320).unwrap_or_default();
    (!name.is_empty() || !email.is_empty()).then_some(Addr { name, email })
}

/// `<t:From><t:Mailbox>…` → una dirección.
fn single_addr(e: &Element, name: &str) -> Option<Addr> {
    t(e, name).and_then(|c| t(c, "Mailbox")).and_then(mailbox)
}

/// `<t:ToRecipients><t:Mailbox>…</t:Mailbox>…` → direcciones.
fn addrs(e: &Element, name: &str) -> Vec<Addr> {
    t(e, name).map(|c| c.children_named(T, "Mailbox").take(500).filter_map(mailbox).collect()).unwrap_or_default()
}

/// Id y change key de un `<t:ItemId>` o `<t:FolderId>` hijo.
fn ids(e: &Element, name: &str) -> Option<(String, Option<String>)> {
    let c = t(e, name)?;
    let id = c.attr("Id").filter(|i| !i.is_empty() && i.len() <= 2_000)?;
    Some((id.to_string(), c.attr("ChangeKey").map(str::to_string)))
}

/// Carpeta desde `<t:Folder>` (o `t:CalendarFolder`…).
fn folder(e: &Element) -> Option<FolderInfo> {
    let (id, change_key) = ids(e, "FolderId")?;
    Some(FolderInfo {
        id,
        change_key,
        parent_id: ids(e, "ParentFolderId").map(|(p, _)| p),
        name: t_text(e, "DisplayName", 255).unwrap_or_else(|| "?".into()),
        class: t_text(e, "FolderClass", 100),
        child_count: t(e, "ChildFolderCount").and_then(|c| c.text_trim().parse().ok()).unwrap_or(0),
    })
}

/// Mensaje desde `<t:Message>` (u otro elemento: `t:MeetingRequest`, `t:Item`…).
pub fn item(e: &Element) -> Option<ItemInfo> {
    let (id, change_key) = ids(e, "ItemId")?;
    Some(ItemInfo {
        id,
        change_key,
        subject: t_text(e, "Subject", 1_000).unwrap_or_default(),
        from: single_addr(e, "From"),
        sender: single_addr(e, "Sender"),
        to: addrs(e, "ToRecipients"),
        cc: addrs(e, "CcRecipients"),
        reply_to: addrs(e, "ReplyTo"),
        received: t_date(e, "DateTimeReceived"),
        sent: t_date(e, "DateTimeSent"),
        is_read: t_bool(e, "IsRead"),
        flagged: t(e, "Flag").and_then(|f| t(f, "FlagStatus")).map(|s| s.text_trim().eq_ignore_ascii_case("Flagged")),
        categories: t(e, "Categories")
            .map(|c| {
                c.children_named(T, "String")
                    .take(100)
                    .map(|s| clip(s.text_trim(), 255))
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        has_attachments: t_bool(e, "HasAttachments").unwrap_or(false),
        internet_message_id: t_text(e, "InternetMessageId", 1_000),
        conversation_id: t(e, "ConversationId").and_then(|c| c.attr("Id")).map(|c| clip(c, 1_000)),
        size: t(e, "Size").and_then(|c| c.text_trim().parse().ok()),
        preview: t_text(e, "Preview", 2_000),
        is_draft: t_bool(e, "IsDraft"),
        parent_folder_id: ids(e, "ParentFolderId").map(|(p, _)| p),
    })
}

/// Los elementos dentro de `<m:Items>` de un mensaje de respuesta.
fn items_of(m: &Element) -> Vec<&Element> {
    m.child(M, "Items").map(|i| i.children.iter().collect()).unwrap_or_default()
}

fn field_uris(fields: &[&str]) -> String {
    fields.iter().map(|f| format!(r#"<t:FieldURI FieldURI="{f}"/>"#)).collect()
}

impl EwsClient {
    // ---------------------------------------------------------------- carpetas

    /// Ids de las carpetas con nombre (`inbox`, `sentitems`…); `None` si el servidor no la tiene
    /// (p. ej. `archive` en servidores antiguos).
    pub async fn distinguished_folders(&self, names: &[&str]) -> Result<Vec<(String, Option<String>)>> {
        let list: String = names.iter().map(|n| folder_id(n)).collect();
        let r = self
            .call(&format!(
                "<m:GetFolder><m:FolderShape><t:BaseShape>IdOnly</t:BaseShape></m:FolderShape><m:FolderIds>{list}</m:FolderIds></m:GetFolder>"
            ))
            .await?;
        let mut out = Vec::new();
        for (name, msg) in names.iter().zip(messages(&r)) {
            let id = match msg {
                Ok(m) => m
                    .child(M, "Folders")
                    .and_then(|f| f.children.first())
                    .and_then(|f| ids(f, "FolderId"))
                    .map(|(i, _)| i),
                Err(e) if e.is_not_found() || e.code() == Some("ErrorInvalidRequest") => None,
                Err(e) => return Err(e),
            };
            out.push(((*name).to_string(), id));
        }
        Ok(out)
    }

    /// Cambios del árbol de carpetas desde `state` (vacío: todo).
    pub async fn sync_folder_hierarchy(&self, state: Option<&str>) -> Result<HierarchySync> {
        let st = state.filter(|s| !s.is_empty()).map(|s| format!("<m:SyncState>{}</m:SyncState>", esc(s)));
        let r = self
            .call(&format!(
                "<m:SyncFolderHierarchy><m:FolderShape><t:BaseShape>IdOnly</t:BaseShape><t:AdditionalProperties>{}</t:AdditionalProperties></m:FolderShape><m:SyncFolderId>{}</m:SyncFolderId>{}</m:SyncFolderHierarchy>",
                field_uris(&["folder:ParentFolderId", "folder:DisplayName", "folder:FolderClass", "folder:ChildFolderCount"]),
                folder_id("msgfolderroot"),
                st.unwrap_or_default()
            ))
            .await?;
        let m = single(&r)?;
        let mut out = HierarchySync {
            state: m.child(M, "SyncState").map(|s| s.text_trim().to_string()).unwrap_or_default(),
            last: m.child(M, "IncludesLastFolderInRange").is_none_or(|l| l.text_trim().eq_ignore_ascii_case("true")),
            ..Default::default()
        };
        if let Some(changes) = m.child(M, "Changes") {
            for c in changes.children.iter().take(100_000) {
                match c.name.as_str() {
                    "Create" | "Update" => {
                        if let Some(f) = c.children.first().and_then(folder) {
                            out.changed.push(f);
                        }
                    }
                    "Delete" => {
                        if let Some((id, _)) = ids(c, "FolderId") {
                            out.deleted.push(id);
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(out)
    }

    /// Crea una carpeta de correo dentro de `parent` (id o `msgfolderroot`). Devuelve su id.
    pub async fn create_folder(&self, parent: &str, name: &str) -> Result<String> {
        let r = self
            .call(&format!(
                "<m:CreateFolder><m:ParentFolderId>{}</m:ParentFolderId><m:Folders><t:Folder><t:FolderClass>IPF.Note</t:FolderClass><t:DisplayName>{}</t:DisplayName></t:Folder></m:Folders></m:CreateFolder>",
                folder_id(parent),
                esc(name)
            ))
            .await?;
        single(&r)?
            .child(M, "Folders")
            .and_then(|f| f.children.first())
            .and_then(|f| ids(f, "FolderId"))
            .map(|(i, _)| i)
            .ok_or_else(|| EwsError::Invalid("CreateFolder sin id".into()))
    }

    /// Cambia el nombre de una carpeta.
    pub async fn rename_folder(&self, id: &str, name: &str) -> Result<()> {
        let r = self
            .call(&format!(
                r#"<m:UpdateFolder><m:FolderChanges><t:FolderChange>{}<t:Updates><t:SetFolderField><t:FieldURI FieldURI="folder:DisplayName"/><t:Folder><t:DisplayName>{}</t:DisplayName></t:Folder></t:SetFolderField></t:Updates></t:FolderChange></m:FolderChanges></m:UpdateFolder>"#,
                folder_id(id),
                esc(name)
            ))
            .await?;
        single(&r).map(|_| ())
    }

    /// Borra una carpeta (a «Elementos eliminados»). Si ya no existe, da igual.
    pub async fn delete_folder(&self, id: &str) -> Result<()> {
        let r = self
            .call(&format!(
                r#"<m:DeleteFolder DeleteType="MoveToDeletedItems"><m:FolderIds>{}</m:FolderIds></m:DeleteFolder>"#,
                folder_id(id)
            ))
            .await?;
        match single(&r) {
            Err(e) if e.is_not_found() => Ok(()),
            other => other.map(|_| ()),
        }
    }

    // ---------------------------------------------------------------- elementos

    /// Cambios de los elementos de `folder` desde `state` (vacío: todos), hasta `max` por llamada.
    pub async fn sync_folder_items(&self, folder: &str, state: Option<&str>, max: u32) -> Result<ItemSync> {
        let st = state.filter(|s| !s.is_empty()).map(|s| format!("<m:SyncState>{}</m:SyncState>", esc(s)));
        let r = self
            .call(&format!(
                "<m:SyncFolderItems><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape></m:ItemShape><m:SyncFolderId>{}</m:SyncFolderId>{}<m:MaxChangesReturned>{}</m:MaxChangesReturned></m:SyncFolderItems>",
                folder_id(folder),
                st.unwrap_or_default(),
                max.clamp(1, MAX_SYNC_CHANGES)
            ))
            .await?;
        let m = single(&r)?;
        let mut out = ItemSync {
            state: m.child(M, "SyncState").map(|s| s.text_trim().to_string()).unwrap_or_default(),
            last: m.child(M, "IncludesLastItemInRange").is_none_or(|l| l.text_trim().eq_ignore_ascii_case("true")),
            ..Default::default()
        };
        if let Some(changes) = m.child(M, "Changes") {
            for c in changes.children.iter().take(100_000) {
                match c.name.as_str() {
                    "Create" | "Update" => {
                        let Some(pair) = c.children.first().and_then(|i| ids(i, "ItemId")) else { continue };
                        if c.name == "Create" { out.created.push(pair) } else { out.updated.push(pair) }
                    }
                    "Delete" => {
                        if let Some((id, _)) = ids(c, "ItemId") {
                            out.deleted.push(id);
                        }
                    }
                    "ReadFlagChange" => {
                        if let (Some((id, _)), Some(read)) = (ids(c, "ItemId"), t_bool(c, "IsRead")) {
                            out.read_changes.push((id, read));
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(out)
    }

    /// Datos de lista de hasta [`BATCH`] mensajes. Los que ya no existen se omiten.
    pub async fn get_summaries(&self, ids: &[String]) -> Result<Vec<ItemInfo>> {
        let mut out = Vec::new();
        for chunk in ids.chunks(BATCH) {
            let list: String = chunk.iter().map(|i| item_id(i, None)).collect();
            let r = self
                .call(&format!(
                    "<m:GetItem><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape><t:AdditionalProperties>{}</t:AdditionalProperties></m:ItemShape><m:ItemIds>{list}</m:ItemIds></m:GetItem>",
                    field_uris(SUMMARY_FIELDS)
                ))
                .await?;
            for msg in messages(&r) {
                match msg {
                    Ok(m) => out.extend(items_of(m).into_iter().filter_map(item)),
                    Err(e) if e.is_not_found() => {}
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(out)
    }

    /// Ids de hasta `max` mensajes de `folder` recibidos antes de `before`, del más reciente al
    /// más antiguo (historial).
    pub async fn find_before(
        &self,
        folder: &str,
        before: DateTime<Utc>,
        max: u32,
    ) -> Result<Vec<(String, Option<DateTime<Utc>>)>> {
        let r = self
            .call(&format!(
                r#"<m:FindItem Traversal="Shallow"><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape><t:AdditionalProperties><t:FieldURI FieldURI="item:DateTimeReceived"/></t:AdditionalProperties></m:ItemShape><m:IndexedPageItemView MaxEntriesReturned="{}" Offset="0" BasePoint="Beginning"/><m:Restriction><t:IsLessThan><t:FieldURI FieldURI="item:DateTimeReceived"/><t:FieldURIOrConstant><t:Constant Value="{}"/></t:FieldURIOrConstant></t:IsLessThan></m:Restriction><m:SortOrder><t:FieldOrder Order="Descending"><t:FieldURI FieldURI="item:DateTimeReceived"/></t:FieldOrder></m:SortOrder><m:ParentFolderIds>{}</m:ParentFolderIds></m:FindItem>"#,
                max.clamp(1, 1_000),
                before.format("%Y-%m-%dT%H:%M:%SZ"),
                folder_id(folder)
            ))
            .await?;
        let m = single(&r)?;
        let items = m.child(M, "RootFolder").and_then(|f| t(f, "Items"));
        Ok(items
            .map(|i| {
                i.children
                    .iter()
                    .take(1_000)
                    .filter_map(|e| ids(e, "ItemId").map(|(id, _)| (id, t_date(e, "DateTimeReceived"))))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// MIME completo de un mensaje.
    pub async fn get_mime(&self, id: &str) -> Result<Vec<u8>> {
        let r = self
            .call(&format!(
                "<m:GetItem><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape><t:IncludeMimeContent>true</t:IncludeMimeContent></m:ItemShape><m:ItemIds>{}</m:ItemIds></m:GetItem>",
                item_id(id, None)
            ))
            .await?;
        let m = single(&r)?;
        let b64 = items_of(m)
            .into_iter()
            .find_map(|i| t(i, "MimeContent"))
            .map(|c| c.text.chars().filter(|c| !c.is_whitespace()).collect::<String>())
            .ok_or_else(|| EwsError::Invalid("GetItem sin MimeContent".into()))?;
        if b64.len() > MAX_MIME.saturating_mul(4) / 3 + 4 {
            return Err(EwsError::TooLarge);
        }
        base64::engine::general_purpose::STANDARD.decode(b64.as_bytes()).map_err(|e| EwsError::Invalid(e.to_string()))
    }

    /// Cambia «leído», la bandera y las categorías (lo que sea `Some`). Si el mensaje ya no
    /// existe, da igual.
    pub async fn update_flags(
        &self,
        id: &str,
        read: Option<bool>,
        flagged: Option<bool>,
        categories: Option<&[String]>,
    ) -> Result<()> {
        let mut updates = String::new();
        if let Some(r) = read {
            updates.push_str(&format!(
                r#"<t:SetItemField><t:FieldURI FieldURI="message:IsRead"/><t:Message><t:IsRead>{r}</t:IsRead></t:Message></t:SetItemField>"#
            ));
        }
        if let Some(f) = flagged {
            let status = if f { "Flagged" } else { "NotFlagged" };
            updates.push_str(&format!(
                r#"<t:SetItemField><t:FieldURI FieldURI="item:Flag"/><t:Message><t:Flag><t:FlagStatus>{status}</t:FlagStatus></t:Flag></t:Message></t:SetItemField>"#
            ));
        }
        if let Some(cats) = categories {
            if cats.is_empty() {
                updates.push_str(r#"<t:DeleteItemField><t:FieldURI FieldURI="item:Categories"/></t:DeleteItemField>"#);
            } else {
                let list: String =
                    cats.iter().take(100).map(|c| format!("<t:String>{}</t:String>", esc(&clip(c, 255)))).collect();
                updates.push_str(&format!(
                    r#"<t:SetItemField><t:FieldURI FieldURI="item:Categories"/><t:Message><t:Categories>{list}</t:Categories></t:Message></t:SetItemField>"#
                ));
            }
        }
        if updates.is_empty() {
            return Ok(());
        }
        let r = self
            .call(&format!(
                r#"<m:UpdateItem ConflictResolution="AlwaysOverwrite" MessageDisposition="SaveOnly" SuppressReadReceipts="true"><m:ItemChanges><t:ItemChange>{}<t:Updates>{updates}</t:Updates></t:ItemChange></m:ItemChanges></m:UpdateItem>"#,
                item_id(id, None)
            ))
            .await?;
        match single(&r) {
            Err(e) if e.is_not_found() => Ok(()),
            other => other.map(|_| ()),
        }
    }

    /// Mueve un mensaje a `folder`. En EWS el id **cambia** al mover: devuelve el nuevo (o `None`
    /// si el servidor no lo dio). Si el mensaje ya no existe, `Ok(None)`.
    pub async fn move_item(&self, id: &str, folder: &str) -> Result<Option<String>> {
        let r = self
            .call(&format!(
                "<m:MoveItem><m:ToFolderId>{}</m:ToFolderId><m:ItemIds>{}</m:ItemIds></m:MoveItem>",
                folder_id(folder),
                item_id(id, None)
            ))
            .await?;
        match single(&r) {
            Ok(m) => Ok(items_of(m).into_iter().find_map(|i| ids(i, "ItemId")).map(|(i, _)| i)),
            Err(e) if e.is_not_found() => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Borra un mensaje: `hard` lo borra del todo; si no, va a «Elementos eliminados». Si ya no
    /// existe, da igual.
    pub async fn delete_item(&self, id: &str, hard: bool) -> Result<()> {
        let kind = if hard { "HardDelete" } else { "MoveToDeletedItems" };
        let r = self
            .call(&format!(
                r#"<m:DeleteItem DeleteType="{kind}" SuppressReadReceipts="true"><m:ItemIds>{}</m:ItemIds></m:DeleteItem>"#,
                item_id(id, None)
            ))
            .await?;
        match single(&r) {
            Err(e) if e.is_not_found() => Ok(()),
            other => other.map(|_| ()),
        }
    }

    /// Crea un mensaje desde MIME en `folder` (id o nombre distinguido), o lo envía. Devuelve el
    /// id del creado (al enviar, EWS no lo da).
    pub async fn create_from_mime(
        &self,
        mime: &[u8],
        folder: &str,
        disposition: MessageDisposition,
    ) -> Result<Option<String>> {
        if mime.len() > MAX_MIME {
            return Err(EwsError::TooLarge);
        }
        let b64 = base64::engine::general_purpose::STANDARD.encode(mime);
        let (disp, extra) = match disposition {
            MessageDisposition::SendAndSaveCopy => ("SendAndSaveCopy", ""),
            MessageDisposition::Save { draft: true } => ("SaveOnly", ""),
            // `PR_MESSAGE_FLAGS` = MSGFLAG_READ: un mensaje normal (no un borrador) y leído.
            MessageDisposition::Save { draft: false } => (
                "SaveOnly",
                r#"<t:ExtendedProperty><t:ExtendedFieldURI PropertyTag="3591" PropertyType="Integer"/><t:Value>1</t:Value></t:ExtendedProperty>"#,
            ),
        };
        let r = self
            .call(&format!(
                r#"<m:CreateItem MessageDisposition="{disp}"><m:SavedItemFolderId>{}</m:SavedItemFolderId><m:Items><t:Message><t:MimeContent CharacterSet="UTF-8">{b64}</t:MimeContent>{extra}</t:Message></m:Items></m:CreateItem>"#,
                folder_id(folder)
            ))
            .await?;
        let m = single(&r)?;
        Ok(items_of(m).into_iter().find_map(|i| ids(i, "ItemId")).map(|(i, _)| i))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_to_info() {
        let xml = format!(
            r#"<t:Message xmlns:t="{T}"><t:ItemId Id="AAMk=" ChangeKey="CQ"/><t:ParentFolderId Id="F1"/><t:Subject>Contrato &amp; anexo</t:Subject>
<t:DateTimeReceived>2026-10-01T10:00:00Z</t:DateTimeReceived><t:Size>2048</t:Size><t:Categories><t:String>Rojo</t:String><t:String>Cliente</t:String></t:Categories>
<t:HasAttachments>true</t:HasAttachments><t:ConversationId Id="CONV"/><t:Flag><t:FlagStatus>Flagged</t:FlagStatus></t:Flag><t:Preview>Hola Ana</t:Preview>
<t:IsDraft>false</t:IsDraft><t:ToRecipients><t:Mailbox><t:Name>Yo</t:Name><t:EmailAddress>yo@contoso.com</t:EmailAddress></t:Mailbox></t:ToRecipients>
<t:IsRead>false</t:IsRead><t:From><t:Mailbox><t:Name>Ana</t:Name><t:EmailAddress>ana@contoso.com</t:EmailAddress></t:Mailbox></t:From>
<t:InternetMessageId>&lt;a@contoso.com&gt;</t:InternetMessageId></t:Message>"#
        );
        let e = iuredesk_dav::dav::xml::parse(xml.as_bytes()).unwrap();
        let i = item(&e).unwrap();
        assert_eq!(i.id, "AAMk=");
        assert_eq!(i.subject, "Contrato & anexo");
        assert_eq!(i.from.as_ref().unwrap().email, "ana@contoso.com");
        assert_eq!(i.to.len(), 1);
        assert_eq!((i.is_read, i.flagged), (Some(false), Some(true)));
        assert_eq!(i.categories, vec!["Rojo".to_string(), "Cliente".to_string()]);
        assert_eq!(i.internet_message_id.as_deref(), Some("<a@contoso.com>"));
        assert_eq!(i.conversation_id.as_deref(), Some("CONV"));
        assert_eq!(i.parent_folder_id.as_deref(), Some("F1"));
        assert_eq!(i.size, Some(2048));
        assert!(i.received.is_some() && i.has_attachments);
        // Sin id no es un elemento.
        let e = iuredesk_dav::dav::xml::parse(format!(r#"<t:Message xmlns:t="{T}"/>"#).as_bytes()).unwrap();
        assert!(item(&e).is_none());
    }

    #[test]
    fn folder_classes() {
        let f = |c: Option<&str>| FolderInfo { class: c.map(str::to_string), ..Default::default() };
        assert!(f(None).is_mail() && f(Some("IPF.Note")).is_mail() && f(Some("IPF.Note.OutlookHomepage")).is_mail());
        assert!(!f(Some("IPF.Appointment")).is_mail() && !f(Some("IPF.Contact")).is_mail());
    }
}
