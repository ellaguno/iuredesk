//! Vínculos locales de correos con Iurefficient (F3.1): «este correo se registró en el lead X»,
//! «se subió al caso Y», «se comentó en el ticket Z». La instancia aún no tiene un recurso de
//! vínculos (`docs/iurefficient-api.md` §3.2), así que IureDesk recuerda lo que hizo para que el
//! panel contextual muestre «Vinculado a …» al instante y sin conexión.
//!
//! Tabla aditiva `email_links`: `Message-ID` normalizado → lista de [`EmailLink`]. No cambia la
//! versión de esquema.

use chrono::{DateTime, Utc};
use redb::ReadableDatabase;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::tables::EMAIL_LINKS;
use crate::{Store, codec};

/// Vínculos que se guardan por mensaje como máximo (los más viejos se descartan).
pub const MAX_LINKS_PER_MESSAGE: usize = 50;
/// Longitud máxima de la clave (`Message-ID`) y de los textos guardados.
const MAX_KEY: usize = 998;
const MAX_TEXT: usize = 500;

/// Un vínculo de un correo con una entidad de Iurefficient.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailLink {
    /// Tipo de la entidad: `case`, `client`, `lead`, `opportunity`, `ticket`, `task`.
    pub kind: String,
    /// Id de la entidad en la instancia.
    pub id: String,
    /// Nombre visible («241/2026 · Arrendamiento», «Acme Corp.»).
    pub label: String,
    /// Qué se hizo: `activity`, `document`, `comment`, `created`.
    pub how: String,
    /// Id de lo creado en la instancia (actividad, documento, comentario), si lo hay.
    pub remote_ref: Option<String>,
    /// Cuándo.
    pub created: DateTime<Utc>,
}

/// Clave normalizada de un `Message-ID`: sin `<>` ni espacios, en minúsculas, acotada.
pub fn link_key(message_id: &str) -> String {
    let k = message_id.trim().trim_start_matches('<').trim_end_matches('>').trim().to_lowercase();
    codec::clip(&k, MAX_KEY).to_string()
}

fn clip_link(l: &EmailLink) -> EmailLink {
    EmailLink {
        kind: codec::clip(&l.kind, 32).to_string(),
        id: codec::clip(&l.id, 200).to_string(),
        label: codec::clip(&l.label, MAX_TEXT).to_string(),
        how: codec::clip(&l.how, 32).to_string(),
        remote_ref: l.remote_ref.as_deref().map(|r| codec::clip(r, 200).to_string()),
        created: l.created,
    }
}

impl Store {
    /// Vínculos de un mensaje (por `Message-ID`), del más viejo al más nuevo.
    pub fn email_links(&self, message_id: &str) -> Result<Vec<EmailLink>> {
        let key = link_key(message_id);
        if key.is_empty() {
            return Ok(Vec::new());
        }
        let tx = self.db.begin_read()?;
        let t = tx.open_table(EMAIL_LINKS)?;
        match t.get(key.as_str())? {
            Some(g) => codec::decode(g.value()),
            None => Ok(Vec::new()),
        }
    }

    /// Añade un vínculo (si ya había uno con el mismo tipo, id y acción, lo reemplaza).
    pub fn add_email_link(&self, message_id: &str, link: &EmailLink) -> Result<()> {
        let key = link_key(message_id);
        if key.is_empty() {
            return Err(crate::StoreError::Invalid("vínculo sin Message-ID".into()));
        }
        let link = clip_link(link);
        self.write(|tx, _| {
            let mut t = tx.open_table(EMAIL_LINKS)?;
            let mut list: Vec<EmailLink> = match t.get(key.as_str())? {
                Some(g) => codec::decode(g.value()).unwrap_or_default(),
                None => Vec::new(),
            };
            list.retain(|l| !(l.kind == link.kind && l.id == link.id && l.how == link.how));
            list.push(link);
            let extra = list.len().saturating_sub(MAX_LINKS_PER_MESSAGE);
            list.drain(..extra);
            let bytes = codec::encode(&list)?;
            t.insert(key.as_str(), bytes.as_slice())?;
            Ok(())
        })
    }

    /// Quita los vínculos de un mensaje con una entidad.
    pub fn remove_email_link(&self, message_id: &str, kind: &str, id: &str) -> Result<()> {
        let key = link_key(message_id);
        self.write(|tx, _| {
            let mut t = tx.open_table(EMAIL_LINKS)?;
            let list: Vec<EmailLink> = match t.get(key.as_str())? {
                Some(g) => codec::decode(g.value()).unwrap_or_default(),
                None => return Ok(()),
            };
            let list: Vec<EmailLink> = list.into_iter().filter(|l| !(l.kind == kind && l.id == id)).collect();
            if list.is_empty() {
                t.remove(key.as_str())?;
            } else {
                let bytes = codec::encode(&list)?;
                t.insert(key.as_str(), bytes.as_slice())?;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn link(kind: &str, id: &str, how: &str) -> EmailLink {
        EmailLink {
            kind: kind.into(),
            id: id.into(),
            label: format!("{kind} {id}"),
            how: how.into(),
            remote_ref: None,
            created: Utc.with_ymd_and_hms(2026, 10, 26, 10, 0, 0).single().unwrap(),
        }
    }

    #[test]
    fn links_round_trip_dedupe_and_survive_reopen() {
        let dir = tempfile::tempdir().unwrap();
        {
            let s = Store::open(dir.path()).unwrap();
            assert!(s.email_links("<a@b>").unwrap().is_empty());
            s.add_email_link("<A@B>", &link("lead", "7", "activity")).unwrap();
            s.add_email_link(" a@b ", &link("lead", "7", "activity")).unwrap();
            s.add_email_link("a@b", &link("case", "c1", "document")).unwrap();
            assert_eq!(s.email_links("<a@b>").unwrap().len(), 2);
        }
        let s = Store::open(dir.path()).unwrap();
        let l = s.email_links("a@b").unwrap();
        assert_eq!(l.iter().map(|l| l.kind.as_str()).collect::<Vec<_>>(), ["lead", "case"]);
        s.remove_email_link("a@b", "lead", "7").unwrap();
        assert_eq!(s.email_links("a@b").unwrap().len(), 1);
        s.remove_email_link("a@b", "case", "c1").unwrap();
        assert!(s.email_links("a@b").unwrap().is_empty());
        assert!(s.add_email_link("  <> ", &link("case", "1", "document")).is_err());
    }

    #[test]
    fn links_are_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        for i in 0..(MAX_LINKS_PER_MESSAGE + 10) {
            s.add_email_link("x@y", &link("task", &i.to_string(), "created")).unwrap();
        }
        let l = s.email_links("x@y").unwrap();
        assert_eq!(l.len(), MAX_LINKS_PER_MESSAGE);
        assert_eq!(l.last().map(|l| l.id.as_str()), Some("59"));
        let long = "z".repeat(5_000);
        s.add_email_link(&long, &link("case", &long, "document")).unwrap();
        assert_eq!(s.email_links(&long).unwrap().first().map(|l| l.id.len()), Some(200));
    }
}
