//! Contactos por EWS (F8.4): la carpeta de contactos (`SyncFolderItems` con los ids y `GetItem`
//! con los datos), crear, cambiar solo los campos dados con `ChangeKey`, borrar, y la libreta
//! global de la organización (`ResolveNames` en Active Directory).
//!
//! Como en [`crate::calendar`], aquí no se conoce el modelo: entrega `t:Contact` y recibe XML.

use crate::ops::{Addr, items_of, t};
use crate::soap::{M, T, clip, esc, folder_id, item_id, messages, single};
use crate::{Element, EwsClient, EwsError, Result};

/// Elementos por lote de `GetItem`.
pub const BATCH: usize = 50;
/// Resultados máximos de la libreta global por búsqueda (`ResolveNames` da hasta 100).
pub const MAX_DIRECTORY: usize = 100;

impl EwsClient {
    /// Contactos completos (`t:Contact`, notas en texto). Las listas de distribución y los que ya
    /// no existen se omiten.
    pub async fn get_contacts(&self, ids: &[String]) -> Result<Vec<Element>> {
        let mut out = Vec::new();
        for chunk in ids.chunks(BATCH) {
            let list: String = chunk.iter().map(|i| item_id(i, None)).collect();
            let r = self
                .call(&format!(
                    "<m:GetItem><m:ItemShape><t:BaseShape>AllProperties</t:BaseShape><t:BodyType>Text</t:BodyType></m:ItemShape><m:ItemIds>{list}</m:ItemIds></m:GetItem>"
                ))
                .await?;
            for msg in messages(&r) {
                match msg {
                    Ok(m) => out.extend(items_of(m).into_iter().filter(|e| e.name == "Contact").cloned()),
                    Err(e) if e.is_not_found() => {}
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(out)
    }

    /// Un contacto completo, o `None` si ya no existe.
    pub async fn get_contact(&self, id: &str) -> Result<Option<Element>> {
        Ok(self.get_contacts(&[id.to_string()]).await?.into_iter().next())
    }

    /// Crea un contacto en `folder` con `inner` (los campos de `t:Contact` en el orden del
    /// esquema). Devuelve su id y change key.
    pub async fn create_contact(&self, folder: &str, inner: &str) -> Result<(String, Option<String>)> {
        let r = self
            .call(&format!(
                "<m:CreateItem><m:SavedItemFolderId>{}</m:SavedItemFolderId><m:Items><t:Contact>{inner}</t:Contact></m:Items></m:CreateItem>",
                folder_id(folder)
            ))
            .await?;
        items_of(single(&r)?)
            .into_iter()
            .find_map(|i| crate::ops::ids(i, "ItemId"))
            .ok_or_else(|| EwsError::Invalid("CreateItem sin id".into()))
    }

    /// Cambia los campos dados de un elemento que no es de calendario (contacto, tarea). Con
    /// `change_key`, solo si no cambió en el servidor (si cambió: `ErrorIrresolvableConflict`).
    pub async fn update_item(
        &self,
        id: &str,
        change_key: Option<&str>,
        updates: &str,
    ) -> Result<(String, Option<String>)> {
        let resolution = if change_key.is_some() { "NeverOverwrite" } else { "AlwaysOverwrite" };
        let r = self
            .call(&format!(
                r#"<m:UpdateItem ConflictResolution="{resolution}"><m:ItemChanges><t:ItemChange>{}<t:Updates>{updates}</t:Updates></t:ItemChange></m:ItemChanges></m:UpdateItem>"#,
                item_id(id, change_key)
            ))
            .await?;
        let m = single(&r)?;
        if m.child(M, "ConflictResults")
            .and_then(|c| c.child(T, "Count").or_else(|| c.child(M, "Count")))
            .and_then(|c| c.text_trim().parse::<u32>().ok())
            .is_some_and(|n| n > 0)
        {
            return Err(EwsError::Soap { code: "ErrorIrresolvableConflict".into(), message: "conflicto".into() });
        }
        Ok(items_of(m).into_iter().find_map(|i| crate::ops::ids(i, "ItemId")).unwrap_or_else(|| (id.to_string(), None)))
    }

    /// Busca `query` en la libreta global de la organización (y en los contactos del buzón):
    /// nombres y direcciones SMTP, hasta [`MAX_DIRECTORY`]. Sin coincidencias: lista vacía.
    pub async fn resolve_names(&self, query: &str) -> Result<Vec<Addr>> {
        let q = clip(query.trim(), 200);
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let r = self
            .call(&format!(
                r#"<m:ResolveNames ReturnFullContactData="false" SearchScope="ActiveDirectoryContacts"><m:UnresolvedEntry>{}</m:UnresolvedEntry></m:ResolveNames>"#,
                esc(&q)
            ))
            .await?;
        // Varias coincidencias llegan como aviso (`ErrorNameResolutionMultipleResults`), que
        // `messages` da por buenas; ninguna es error.
        let m = match single(&r) {
            Ok(m) => m,
            Err(e) if e.code() == Some("ErrorNameResolutionNoResults") => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        Ok(parse_resolutions(m))
    }
}

/// Las direcciones SMTP de un `ResolveNamesResponseMessage` (sin repetir; las que no son SMTP,
/// como las listas privadas sin dirección, se omiten).
pub fn parse_resolutions(m: &Element) -> Vec<Addr> {
    let mut out: Vec<Addr> = Vec::new();
    let Some(set) = m.child(M, "ResolutionSet") else { return out };
    for r in set.children.iter().take(MAX_DIRECTORY) {
        let Some(mb) = t(r, "Mailbox") else { continue };
        let routing = t(mb, "RoutingType").map(|c| c.text_trim().to_ascii_uppercase()).unwrap_or_else(|| "SMTP".into());
        let email = t(mb, "EmailAddress").map(|c| clip(c.text_trim(), 320)).unwrap_or_default();
        if routing != "SMTP" || !email.contains('@') || email.starts_with('/') {
            continue;
        }
        let name = t(mb, "Name").map(|c| clip(c.text_trim(), 300)).unwrap_or_default();
        if !out.iter().any(|a| a.email.eq_ignore_ascii_case(&email)) {
            out.push(Addr { name, email });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolutions_keep_smtp_only() {
        let x = format!(
            r#"<m:ResolveNamesResponseMessage xmlns:m="{M}" xmlns:t="{T}" ResponseClass="Warning"><m:ResponseCode>ErrorNameResolutionMultipleResults</m:ResponseCode><m:ResolutionSet TotalItemsInView="4">
<t:Resolution><t:Mailbox><t:Name>Ana Pérez</t:Name><t:EmailAddress>ana@contoso.com</t:EmailAddress><t:RoutingType>SMTP</t:RoutingType><t:MailboxType>Mailbox</t:MailboxType></t:Mailbox></t:Resolution>
<t:Resolution><t:Mailbox><t:Name>Ventas</t:Name><t:EmailAddress>ventas@contoso.com</t:EmailAddress><t:RoutingType>SMTP</t:RoutingType><t:MailboxType>PublicDL</t:MailboxType></t:Mailbox></t:Resolution>
<t:Resolution><t:Mailbox><t:Name>Lista privada</t:Name><t:RoutingType>MAPIPDL</t:RoutingType></t:Mailbox></t:Resolution>
<t:Resolution><t:Mailbox><t:Name>Ana P. (otra vez)</t:Name><t:EmailAddress>ANA@contoso.com</t:EmailAddress><t:RoutingType>SMTP</t:RoutingType></t:Mailbox></t:Resolution>
</m:ResolutionSet></m:ResolveNamesResponseMessage>"#
        );
        let e = iuredesk_dav::dav::xml::parse(x.as_bytes()).unwrap();
        let r = parse_resolutions(&e);
        assert_eq!(
            r,
            vec![
                Addr { name: "Ana Pérez".into(), email: "ana@contoso.com".into() },
                Addr { name: "Ventas".into(), email: "ventas@contoso.com".into() },
            ]
        );
    }
}
