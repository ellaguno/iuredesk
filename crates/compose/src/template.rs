//! Plantillas: borradores guardados con nombre en el almacén local.
//!
//! Se guardan sin lo que ata a un hilo concreto (`In-Reply-To`, `References`, original, envío
//! programado). Los adjuntos en memoria se pasan a blobs para no inflar el registro.

use iuredesk_store::Store;

use crate::draft::{AttachmentSource, Draft};
use crate::error::{ComposeError, Result};

const INDEX_KEY: &str = "compose/templates";
const PREFIX: &str = "compose/template/";
/// Longitud máxima del nombre de una plantilla.
const MAX_NAME: usize = 200;
/// Número máximo de plantillas.
const MAX_TEMPLATES: usize = 1_000;

/// Plantillas sobre un [`Store`].
#[derive(Debug, Clone, Copy)]
pub struct TemplateStore<'a> {
    store: &'a Store,
}

impl<'a> TemplateStore<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }

    /// Nombres de las plantillas, en orden alfabético.
    pub fn list(&self) -> Result<Vec<String>> {
        let mut names: Vec<String> = match self.store.sync_state(INDEX_KEY)? {
            Some(b) => serde_json::from_slice(&b)?,
            None => Vec::new(),
        };
        names.sort_by_key(|n| n.to_lowercase());
        Ok(names)
    }

    fn put_index(&self, names: &[String]) -> Result<()> {
        self.store.put_sync_state(INDEX_KEY, &serde_json::to_vec(names)?)?;
        Ok(())
    }

    /// Guarda (o reemplaza) una plantilla.
    pub fn save(&self, name: &str, draft: &Draft) -> Result<()> {
        let name = clean_name(name)?;
        let mut t = draft.clone();
        t.in_reply_to = None;
        t.references.clear();
        t.original = None;
        t.forwarded = None;
        t.send_at = None;
        for a in &mut t.attachments {
            if let AttachmentSource::Bytes(data) = &a.source {
                let hash = self.store.blobs().put(data)?;
                a.source = AttachmentSource::Blob(hash);
            }
        }
        let mut names = self.list()?;
        if !names.contains(&name) {
            if names.len() >= MAX_TEMPLATES {
                return Err(ComposeError::Invalid("demasiadas plantillas".to_string()));
            }
            names.push(name.clone());
        }
        self.store.put_sync_state(&format!("{PREFIX}{name}"), &serde_json::to_vec(&t)?)?;
        self.put_index(&names)
    }

    /// Carga una plantilla como borrador nuevo.
    pub fn load(&self, name: &str) -> Result<Draft> {
        let name = clean_name(name)?;
        match self.store.sync_state(&format!("{PREFIX}{name}"))? {
            Some(b) => Ok(serde_json::from_slice(&b)?),
            None => Err(ComposeError::NotFound { what: "la plantilla", name }),
        }
    }

    /// Borra una plantilla.
    pub fn delete(&self, name: &str) -> Result<()> {
        let name = clean_name(name)?;
        let mut names = self.list()?;
        let before = names.len();
        names.retain(|n| n != &name);
        if names.len() == before {
            return Err(ComposeError::NotFound { what: "la plantilla", name });
        }
        self.store.delete_sync_state(&format!("{PREFIX}{name}"))?;
        self.put_index(&names)
    }
}

fn clean_name(name: &str) -> Result<String> {
    let n: String = name.trim().chars().filter(|c| !c.is_control()).take(MAX_NAME).collect();
    if n.is_empty() {
        return Err(ComposeError::Invalid("nombre de plantilla vacío".to_string()));
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draft::Attachment;
    use crate::rich::RichBody;
    use iuredesk_model::account::Identity;

    #[test]
    fn save_load_list_delete() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let ts = TemplateStore::new(&store);
        let mut d =
            Draft::new(Identity { name: "Ana".into(), email: "ana@acme.mx".into(), signature: None, reply_to: None });
        d.subject = "Cotización".into();
        d.body = RichBody::from_plain("Hola {nombre}");
        d.in_reply_to = Some("<x@y>".into());
        d.attachments.push(Attachment::bytes("a.txt", "text/plain", vec![b'x'; 2 * 1024 * 1024]));
        ts.save("  Cotización  ", &d).unwrap();
        ts.save("bienvenida", &d).unwrap();
        assert_eq!(ts.list().unwrap(), vec!["bienvenida".to_string(), "Cotización".to_string()]);
        let t = ts.load("Cotización").unwrap();
        assert_eq!(t.subject, "Cotización");
        assert_eq!(t.in_reply_to, None);
        assert!(matches!(t.attachments[0].source, AttachmentSource::Blob(_)));
        ts.delete("Cotización").unwrap();
        assert!(matches!(ts.load("Cotización"), Err(ComposeError::NotFound { .. })));
        assert!(ts.delete("Cotización").is_err());
        assert!(ts.save(" ", &d).is_err());
    }
}
