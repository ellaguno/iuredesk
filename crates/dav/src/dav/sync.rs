//! Sincronización de una colección: RFC 6578 (`sync-collection`) y, si el servidor no lo
//! admite, `getctag` + diferencia de ETags.

use std::collections::BTreeMap;

use reqwest::Url;

use super::client::{DavClient, Depth, XML, status_error};
use super::xml::{self, DAV, escape};
use crate::{Error, Result};

/// Un objeto con su ETag.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Item {
    /// URL absoluta.
    pub url: Url,
    /// ETag (puede faltar si el servidor no lo dio).
    pub etag: Option<String>,
}

/// Una página de `sync-collection`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncPage {
    /// Token nuevo.
    pub token: Option<String>,
    /// Objetos nuevos o cambiados.
    pub changed: Vec<Item>,
    /// Objetos borrados.
    pub deleted: Vec<Url>,
    /// El servidor truncó la respuesta (507): hay más cambios.
    pub truncated: bool,
}

/// Estado guardado entre sincronizaciones de una colección.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncState {
    /// Último `sync-token`.
    pub sync_token: Option<String>,
    /// Último `getctag`.
    pub ctag: Option<String>,
    /// El servidor no admite `sync-collection` en esta colección (no volver a intentarlo).
    pub sync_unsupported: bool,
}

/// Cómo se sincronizó.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMethod {
    /// RFC 6578 incremental.
    SyncCollection,
    /// RFC 6578 desde cero (no había token o el servidor lo invalidó).
    SyncCollectionFull,
    /// `getctag` sin cambios: no se pidió nada más.
    CtagUnchanged,
    /// Lista completa de ETags comparada con la local.
    EtagDiff,
}

/// Resultado de [`DavClient::sync`]: qué descargar (con `multiget`) y qué borrar localmente.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncOutcome {
    /// Nuevos o cambiados.
    pub changed: Vec<Item>,
    /// Borrados en el servidor.
    pub deleted: Vec<Url>,
    /// Estado a guardar para la próxima vez.
    pub state: SyncState,
    /// Vía usada.
    pub method: SyncMethod,
}

impl DavClient {
    /// Una vuelta de `sync-collection` (RFC 6578) desde `token` (o desde cero), siguiendo las
    /// páginas truncadas (507) hasta [`super::DavOptions::max_sync_pages`].
    ///
    /// Errores: [`Error::InvalidSyncToken`] si el servidor pide empezar de nuevo;
    /// [`Error::Unsupported`] si no admite el informe.
    pub async fn sync_collection(&self, collection: &Url, token: Option<&str>) -> Result<SyncPage> {
        let mut page = SyncPage { token: token.map(str::to_string), ..SyncPage::default() };
        let mut changed: BTreeMap<Url, Option<String>> = BTreeMap::new();
        let mut deleted: Vec<Url> = Vec::new();
        for round in 0..self.opts.max_sync_pages.max(1) {
            let body = format!(
                r#"<?xml version="1.0" encoding="utf-8"?><d:sync-collection xmlns:d="DAV:"><d:sync-token>{}</d:sync-token><d:sync-level>1</d:sync-level><d:prop><d:getetag/></d:prop></d:sync-collection>"#,
                escape(page.token.as_deref().unwrap_or_default())
            );
            let resp =
                self.request("REPORT", collection, Some(Depth::Zero), &[], Some((XML, body.into_bytes()))).await?;
            if resp.status != 207 {
                let text = String::from_utf8_lossy(&resp.body);
                return Err(match resp.status {
                    403 | 409 if text.contains("valid-sync-token") => Error::InvalidSyncToken,
                    // 400/403/405/415/501: sin soporte (o el informe no se entiende).
                    400 | 403 | 405 | 415 | 501 => Error::Unsupported("sync-collection"),
                    _ => status_error(&resp, "REPORT sync-collection"),
                });
            }
            let ms = xml::parse_multistatus(&resp.body)?;
            let base = resp.url.clone();
            let mut truncated = false;
            for r in &ms.responses {
                let Ok(url) = base.join(&r.href) else { continue };
                if same_path(&url, collection) || same_path(&url, &base) {
                    // El contenedor mismo: 507 = hay más páginas.
                    truncated |= r.status == Some(507);
                    continue;
                }
                match r.status {
                    Some(404) | Some(410) => {
                        changed.remove(&url);
                        deleted.push(url);
                    }
                    _ => {
                        deleted.retain(|d| *d != url);
                        changed.insert(url, r.prop_text(DAV, "getetag"));
                    }
                }
            }
            let new_token = ms.sync_token.clone();
            if new_token.is_none() {
                return Err(Error::Unsupported("sync-collection"));
            }
            let advanced = new_token != page.token;
            page.token = new_token;
            if !truncated {
                page.truncated = false;
                break;
            }
            page.truncated = true;
            // Un servidor que trunca sin avanzar el token se quedaría en bucle.
            if !advanced || round.saturating_add(1) >= self.opts.max_sync_pages {
                break;
            }
        }
        page.changed = changed.into_iter().map(|(url, etag)| Item { url, etag }).collect();
        page.deleted = deleted;
        Ok(page)
    }

    /// Lista los objetos de una colección con su ETag (`PROPFIND Depth: 1`).
    pub async fn list_etags(&self, collection: &Url) -> Result<Vec<Item>> {
        let body = r#"<?xml version="1.0" encoding="utf-8"?><d:propfind xmlns:d="DAV:"><d:prop><d:getetag/><d:resourcetype/></d:prop></d:propfind>"#;
        let (base, ms) = self.propfind(collection, Depth::One, body).await?;
        Ok(ms
            .responses
            .iter()
            .filter(|r| r.ok())
            .filter(|r| r.prop(DAV, "resourcetype").is_none_or(|rt| rt.child(DAV, "collection").is_none()))
            .filter_map(|r| {
                let url = base.join(&r.href).ok()?;
                (!same_path(&url, collection) && !same_path(&url, &base))
                    .then(|| Item { url, etag: r.prop_text(DAV, "getetag") })
            })
            .collect())
    }

    /// Sincroniza una colección contra lo que hay localmente (`known`: URL → ETag).
    ///
    /// 1. `sync-collection` incremental si hay token; si el token caducó, desde cero (y lo que
    ///    no vuelva se da por borrado).
    /// 2. Si el servidor no admite RFC 6578: `getctag`; si no cambió, nada que hacer; si cambió
    ///    (o no hay), lista de ETags y diferencia con `known`.
    pub async fn sync(
        &self,
        collection: &Url,
        state: &SyncState,
        known: &BTreeMap<Url, String>,
    ) -> Result<SyncOutcome> {
        if !state.sync_unsupported {
            let attempt = match self.sync_collection(collection, state.sync_token.as_deref()).await {
                Err(Error::InvalidSyncToken) if state.sync_token.is_some() => {
                    self.sync_collection(collection, None).await.map(|p| (p, true))
                }
                other => other.map(|p| (p, state.sync_token.is_none())),
            };
            match attempt {
                Ok((page, full)) => {
                    let mut deleted = page.deleted;
                    let changed = if full {
                        // Desde cero: lo local que no volvió está borrado; solo se baja lo distinto.
                        let seen: BTreeMap<&Url, &Option<String>> =
                            page.changed.iter().map(|i| (&i.url, &i.etag)).collect();
                        deleted.extend(known.keys().filter(|k| !seen.contains_key(k)).cloned());
                        page.changed
                            .into_iter()
                            .filter(|i| i.etag.is_none() || known.get(&i.url) != i.etag.as_ref())
                            .collect()
                    } else {
                        page.changed
                            .into_iter()
                            .filter(|i| i.etag.is_none() || known.get(&i.url) != i.etag.as_ref())
                            .collect()
                    };
                    deleted.sort();
                    deleted.dedup();
                    return Ok(SyncOutcome {
                        changed,
                        deleted,
                        state: SyncState { sync_token: page.token, ctag: state.ctag.clone(), sync_unsupported: false },
                        method: if full { SyncMethod::SyncCollectionFull } else { SyncMethod::SyncCollection },
                    });
                }
                Err(Error::Unsupported(_)) => {}
                Err(e) => return Err(e),
            }
        }

        // Caída: getctag + ETags.
        let props = self.collection_props(collection).await?;
        let ctag = props.and_then(|c| c.ctag);
        let base_state = SyncState { sync_token: None, ctag: ctag.clone(), sync_unsupported: true };
        if ctag.is_some() && ctag == state.ctag {
            return Ok(SyncOutcome {
                changed: Vec::new(),
                deleted: Vec::new(),
                state: base_state,
                method: SyncMethod::CtagUnchanged,
            });
        }
        let remote = self.list_etags(collection).await?;
        let remote_urls: BTreeMap<&Url, ()> = remote.iter().map(|i| (&i.url, ())).collect();
        let deleted: Vec<Url> = known.keys().filter(|k| !remote_urls.contains_key(k)).cloned().collect();
        let changed: Vec<Item> =
            remote.into_iter().filter(|i| i.etag.is_none() || known.get(&i.url) != i.etag.as_ref()).collect();
        Ok(SyncOutcome { changed, deleted, state: base_state, method: SyncMethod::EtagDiff })
    }
}

/// Compara rutas sin la barra final (los servidores no son consistentes con ella).
fn same_path(a: &Url, b: &Url) -> bool {
    a.host_str() == b.host_str() && a.path().trim_end_matches('/') == b.path().trim_end_matches('/')
}
