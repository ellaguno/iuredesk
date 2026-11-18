//! Calendario por EWS (F8.4): carpetas de calendario, apariciones en una ventana
//! (`FindItem` con `CalendarView`: el servidor expande las series), datos completos de cada
//! elemento, maestro de una serie, crear, cambiar solo los campos dados (con `ChangeKey`: si el
//! elemento cambió en el servidor, [`EwsError::Soap`] con `ErrorIrresolvableConflict`), borrar y
//! responder a una invitación (el servidor manda el correo).
//!
//! Este módulo no conoce el modelo de IureDesk: entrega elementos XML (`t:CalendarItem`) y recibe
//! XML ya armado; la traducción vive en `pim-sync`.

use crate::ops::{field_uris, folder, ids, items_of, t, t_bool};
use crate::soap::{M, T, esc, folder_id, item_id, messages, single};
use crate::{Element, EwsClient, EwsError, FolderInfo, Result};

/// Elementos por lote de `GetItem`.
pub const BATCH: usize = 50;
/// Apariciones máximas por consulta de `CalendarView` (el límite habitual de Exchange es 1000).
pub const MAX_VIEW: u32 = 1_000;

/// Propiedades que se piden además de `AllProperties` (algunas no vienen en él).
const CALENDAR_FIELDS: &[&str] = &[
    "calendar:UID",
    "calendar:RecurrenceId",
    "calendar:StartTimeZone",
    "calendar:EndTimeZone",
    "calendar:IsResponseRequested",
    "item:LastModifiedTime",
];

/// Una aparición de la vista de calendario (solo lo necesario para saber si cambió).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ViewItem {
    pub id: String,
    pub change_key: Option<String>,
    /// `Single`, `Occurrence`, `Exception` o `RecurringMaster`.
    pub item_type: Option<String>,
}

/// Cómo se avisa a los asistentes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notify {
    /// A nadie (citas sin asistentes, o cambios de quien no organiza).
    None,
    /// A todos (crear, cancelar).
    All,
    /// A los afectados (cambios del organizador).
    Changed,
}

impl Notify {
    fn value(self, changed_ok: bool) -> &'static str {
        match self {
            Notify::None => "SendToNone",
            Notify::All => "SendToAllAndSaveCopy",
            Notify::Changed if changed_ok => "SendToChangedAndSaveCopy",
            Notify::Changed => "SendToAllAndSaveCopy",
        }
    }
}

/// Respuesta a una invitación.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Response {
    Accept,
    Tentative,
    Decline,
}

impl EwsClient {
    /// El calendario principal y sus subcalendarios (clase `IPF.Appointment`).
    pub async fn calendar_folders(&self) -> Result<Vec<FolderInfo>> {
        let props = field_uris(&[
            "folder:ParentFolderId",
            "folder:DisplayName",
            "folder:FolderClass",
            "folder:ChildFolderCount",
        ]);
        let r = self
            .call(&format!(
                "<m:GetFolder><m:FolderShape><t:BaseShape>IdOnly</t:BaseShape><t:AdditionalProperties>{props}</t:AdditionalProperties></m:FolderShape><m:FolderIds>{}</m:FolderIds></m:GetFolder>",
                folder_id("calendar")
            ))
            .await?;
        let mut out: Vec<FolderInfo> =
            single(&r)?.child(M, "Folders").map(|f| f.children.iter().filter_map(folder).collect()).unwrap_or_default();
        let Some(main) = out.first().cloned() else {
            return Err(EwsError::Invalid("el buzón no tiene calendario".into()));
        };
        if main.child_count > 0 {
            let r = self
                .call(&format!(
                    r#"<m:FindFolder Traversal="Deep"><m:FolderShape><t:BaseShape>IdOnly</t:BaseShape><t:AdditionalProperties>{props}</t:AdditionalProperties></m:FolderShape><m:ParentFolderIds>{}</m:ParentFolderIds></m:FindFolder>"#,
                    folder_id(&main.id)
                ))
                .await?;
            let m = single(&r)?;
            if let Some(list) = m.child(M, "RootFolder").and_then(|f| t(f, "Folders")) {
                out.extend(
                    list.children
                        .iter()
                        .take(500)
                        .filter_map(folder)
                        .filter(|f| f.class.as_deref().is_some_and(|c| c.starts_with("IPF.Appointment"))),
                );
            }
        }
        Ok(out)
    }

    /// Apariciones de `folder` entre `start` y `end` (UTC). Devuelve también si vinieron todas
    /// (`false`: había más de [`MAX_VIEW`]).
    pub async fn calendar_view(
        &self,
        folder: &str,
        start: chrono::DateTime<chrono::Utc>,
        end: chrono::DateTime<chrono::Utc>,
    ) -> Result<(Vec<ViewItem>, bool)> {
        let r = self
            .call(&format!(
                r#"<m:FindItem Traversal="Shallow"><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape><t:AdditionalProperties><t:FieldURI FieldURI="calendar:CalendarItemType"/></t:AdditionalProperties></m:ItemShape><m:CalendarView MaxEntriesReturned="{MAX_VIEW}" StartDate="{}" EndDate="{}"/><m:ParentFolderIds>{}</m:ParentFolderIds></m:FindItem>"#,
                start.format("%Y-%m-%dT%H:%M:%SZ"),
                end.format("%Y-%m-%dT%H:%M:%SZ"),
                folder_id(folder)
            ))
            .await?;
        let m = single(&r)?;
        let Some(root) = m.child(M, "RootFolder") else { return Ok((Vec::new(), true)) };
        let complete = root.attr("IncludesLastItemInRange").is_none_or(|v| v.eq_ignore_ascii_case("true"));
        let items = t(root, "Items")
            .map(|i| {
                i.children
                    .iter()
                    .take(MAX_VIEW as usize)
                    .filter_map(|e| {
                        let (id, change_key) = ids(e, "ItemId")?;
                        Some(ViewItem {
                            id,
                            change_key,
                            item_type: t(e, "CalendarItemType").map(|c| c.text_trim().to_string()),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok((items, complete))
    }

    /// Los elementos de calendario completos (`t:CalendarItem`, cuerpo en texto). Los que ya no
    /// existen se omiten.
    pub async fn get_calendar_items(&self, ids: &[String]) -> Result<Vec<Element>> {
        let mut out = Vec::new();
        for chunk in ids.chunks(BATCH) {
            let list: String = chunk.iter().map(|i| item_id(i, None)).collect();
            let r = self
                .call(&format!(
                    "<m:GetItem><m:ItemShape><t:BaseShape>AllProperties</t:BaseShape><t:BodyType>Text</t:BodyType><t:AdditionalProperties>{}</t:AdditionalProperties></m:ItemShape><m:ItemIds>{list}</m:ItemIds></m:GetItem>",
                    field_uris(CALENDAR_FIELDS)
                ))
                .await?;
            for msg in messages(&r) {
                match msg {
                    Ok(m) => out.extend(items_of(m).into_iter().cloned()),
                    Err(e) if e.is_not_found() => {}
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(out)
    }

    /// Un elemento completo, o `None` si ya no existe.
    pub async fn get_calendar_item(&self, id: &str) -> Result<Option<Element>> {
        Ok(self.get_calendar_items(&[id.to_string()]).await?.into_iter().next())
    }

    /// Id del maestro de la serie de una aparición o excepción (`None` si ya no existe).
    pub async fn recurring_master(&self, occurrence: &str) -> Result<Option<String>> {
        let r = self
            .call(&format!(
                r#"<m:GetItem><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape></m:ItemShape><m:ItemIds><t:RecurringMasterItemId OccurrenceId="{}"/></m:ItemIds></m:GetItem>"#,
                esc(occurrence)
            ))
            .await?;
        match single(&r) {
            Ok(m) => Ok(items_of(m).into_iter().find_map(|i| ids(i, "ItemId")).map(|(i, _)| i)),
            Err(e) if e.is_not_found() || e.code() == Some("ErrorCalendarOccurrenceIsDeletedFromRecurrence") => {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    /// Crea un elemento en `folder` con `inner` (los campos de `t:CalendarItem`, en el orden del
    /// esquema). Devuelve su id y change key.
    pub async fn create_calendar_item(
        &self,
        folder: &str,
        inner: &str,
        notify: Notify,
    ) -> Result<(String, Option<String>)> {
        let r = self
            .call(&format!(
                r#"<m:CreateItem SendMeetingInvitations="{}"><m:SavedItemFolderId>{}</m:SavedItemFolderId><m:Items><t:CalendarItem>{inner}</t:CalendarItem></m:Items></m:CreateItem>"#,
                notify.value(false),
                folder_id(folder)
            ))
            .await?;
        items_of(single(&r)?)
            .into_iter()
            .find_map(|i| ids(i, "ItemId"))
            .ok_or_else(|| EwsError::Invalid("CreateItem sin id".into()))
    }

    /// Cambia los campos dados (`updates`: `t:SetItemField`/`t:DeleteItemField`). Con
    /// `change_key`, solo si el elemento no cambió en el servidor (si cambió:
    /// `ErrorIrresolvableConflict`). Devuelve el id y el change key nuevos.
    pub async fn update_calendar_item(
        &self,
        id: &str,
        change_key: Option<&str>,
        updates: &str,
        notify: Notify,
    ) -> Result<(String, Option<String>)> {
        let resolution = if change_key.is_some() { "NeverOverwrite" } else { "AlwaysOverwrite" };
        let r = self
            .call(&format!(
                r#"<m:UpdateItem ConflictResolution="{resolution}" SendMeetingInvitationsOrCancellations="{}"><m:ItemChanges><t:ItemChange>{}<t:Updates>{updates}</t:Updates></t:ItemChange></m:ItemChanges></m:UpdateItem>"#,
                notify.value(true),
                item_id(id, change_key)
            ))
            .await?;
        let m = single(&r)?;
        // Si alguien cambió el elemento, NeverOverwrite deja un conflicto en vez de error.
        if m.child(M, "ConflictResults")
            .and_then(|c| c.child(T, "Count").or_else(|| c.child(M, "Count")))
            .and_then(|c| c.text_trim().parse::<u32>().ok())
            .is_some_and(|n| n > 0)
        {
            return Err(EwsError::Soap { code: "ErrorIrresolvableConflict".into(), message: "conflicto".into() });
        }
        Ok(items_of(m).into_iter().find_map(|i| ids(i, "ItemId")).unwrap_or_else(|| (id.to_string(), None)))
    }

    /// Borra un elemento (a «Elementos eliminados»); `notify` manda la cancelación a los
    /// asistentes. Si ya no existe, da igual.
    pub async fn delete_calendar_item(&self, id: &str, notify: Notify) -> Result<()> {
        let r = self
            .call(&format!(
                r#"<m:DeleteItem DeleteType="MoveToDeletedItems" SendMeetingCancellations="{}"><m:ItemIds>{}</m:ItemIds></m:DeleteItem>"#,
                notify.value(false),
                item_id(id, None)
            ))
            .await?;
        match single(&r) {
            Err(e) if e.is_not_found() => Ok(()),
            other => other.map(|_| ()),
        }
    }

    /// Responde a la invitación `id` (aceptar, provisional o rechazar) con un comentario
    /// opcional; el servidor manda la respuesta al organizador. Si ya no existe, da igual.
    pub async fn respond(&self, id: &str, response: Response, comment: Option<&str>) -> Result<()> {
        let kind = match response {
            Response::Accept => "AcceptItem",
            Response::Tentative => "TentativelyAcceptItem",
            Response::Decline => "DeclineItem",
        };
        let body = comment
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(|c| format!(r#"<t:Body BodyType="Text">{}</t:Body>"#, esc(c)))
            .unwrap_or_default();
        let r = self
            .call(&format!(
                r#"<m:CreateItem MessageDisposition="SendAndSaveCopy"><m:Items><t:{kind}>{body}<t:ReferenceItemId Id="{}"/></t:{kind}></m:Items></m:CreateItem>"#,
                esc(id)
            ))
            .await?;
        match single(&r) {
            Err(e) if e.is_not_found() => Ok(()),
            other => other.map(|_| ()),
        }
    }
}

/// ¿Es un conflicto de concurrencia (alguien cambió el elemento antes)?
pub fn is_conflict(e: &EwsError) -> bool {
    matches!(
        e.code(),
        Some("ErrorIrresolvableConflict" | "ErrorChangeKeyRequiredForWriteOperations" | "ErrorStaleObject")
    )
}

/// ¿El elemento es una reunión que organiza el dueño del buzón? (Para decidir si se avisa a los
/// asistentes al cambiarla o borrarla.)
pub fn organized_by_me(item: &Element) -> bool {
    t_bool(item, "IsMeeting").unwrap_or(false)
        && t(item, "MyResponseType").is_some_and(|r| r.text_trim().eq_ignore_ascii_case("Organizer"))
}
