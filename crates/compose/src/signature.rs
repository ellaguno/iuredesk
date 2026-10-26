//! Firmas (con formato) por identidad.
//!
//! Cada identidad puede tener una firma para mensajes nuevos y otra para respuestas y reenvíos.
//! La libreta se guarda en el almacén local (clave `compose/signatures`). Si una identidad no
//! tiene entrada, se usa el texto de `Identity::signature` de la cuenta (HTML o texto plano).

use std::collections::BTreeMap;

use iuredesk_model::account::Identity;
use iuredesk_store::Store;
use serde::{Deserialize, Serialize};

use crate::address::address_key;
use crate::error::{ComposeError, Result};
use crate::rich::{Block, RichBody};

/// Clave del almacén.
const STORE_KEY: &str = "compose/signatures";
/// Prefijo del encabezado de un reenvío en línea (lo produce [`crate::reply::forward`]).
pub(crate) const FORWARD_MARK: &str = "----------";

/// Dónde va la firma al responder.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignaturePlacement {
    /// Encima del texto citado (Outlook, Gmail, Apple Mail).
    #[default]
    AboveQuote,
    /// Al final, debajo de la cita (estilo Thunderbird, respuesta abajo).
    BelowQuote,
}

/// Una firma con nombre.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature {
    pub name: String,
    pub body: RichBody,
}

/// Qué firma usa cada identidad.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentitySignatures {
    /// Firma para mensajes nuevos (nombre).
    #[serde(default)]
    pub new_message: Option<String>,
    /// Firma para respuestas y reenvíos (nombre).
    #[serde(default)]
    pub reply: Option<String>,
}

/// Libreta de firmas.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignatureBook {
    #[serde(default)]
    pub signatures: Vec<Signature>,
    /// Por dirección de identidad (clave normalizada).
    #[serde(default)]
    pub by_identity: BTreeMap<String, IdentitySignatures>,
    #[serde(default)]
    pub placement: SignaturePlacement,
}

impl SignatureBook {
    /// Carga la libreta del almacén (vacía si no hay).
    pub fn load(store: &Store) -> Result<Self> {
        match store.sync_state(STORE_KEY)? {
            Some(bytes) => Ok(serde_json::from_slice(&bytes)?),
            None => Ok(Self::default()),
        }
    }

    /// Guarda la libreta.
    pub fn save(&self, store: &Store) -> Result<()> {
        let bytes = serde_json::to_vec(self)?;
        store.put_sync_state(STORE_KEY, &bytes)?;
        Ok(())
    }

    /// Añade o reemplaza una firma por nombre.
    pub fn upsert(&mut self, sig: Signature) {
        match self.signatures.iter_mut().find(|s| s.name == sig.name) {
            Some(s) => *s = sig,
            None => self.signatures.push(sig),
        }
    }

    /// Quita una firma y sus asignaciones.
    pub fn remove(&mut self, name: &str) -> Result<()> {
        let before = self.signatures.len();
        self.signatures.retain(|s| s.name != name);
        if self.signatures.len() == before {
            return Err(ComposeError::NotFound { what: "la firma", name: name.to_string() });
        }
        for v in self.by_identity.values_mut() {
            if v.new_message.as_deref() == Some(name) {
                v.new_message = None;
            }
            if v.reply.as_deref() == Some(name) {
                v.reply = None;
            }
        }
        Ok(())
    }

    /// Asigna firmas a una identidad.
    pub fn assign(&mut self, identity_email: &str, sigs: IdentitySignatures) {
        self.by_identity.insert(address_key(identity_email), sigs);
    }

    /// Firma que corresponde a la identidad (`reply` = respuesta o reenvío).
    pub fn for_identity(&self, identity: &Identity, reply: bool) -> Option<RichBody> {
        if let Some(entry) = self.by_identity.get(&address_key(&identity.email)) {
            let name =
                if reply { entry.reply.as_ref().or(entry.new_message.as_ref()) } else { entry.new_message.as_ref() };
            return name.and_then(|n| self.signatures.iter().find(|s| &s.name == n)).map(|s| s.body.clone());
        }
        identity_signature(identity)
    }
}

/// Firma guardada en la propia identidad de la cuenta (HTML si parece HTML; si no, texto).
pub fn identity_signature(identity: &Identity) -> Option<RichBody> {
    let raw = identity.signature.as_deref()?.trim();
    if raw.is_empty() {
        return None;
    }
    let body =
        if raw.contains('<') && raw.contains('>') { RichBody::from_html(raw) } else { RichBody::from_plain(raw) };
    (!body.is_empty()).then_some(body)
}

/// Pone (o quita, con `None`) la firma del documento. Si ya había una, la nueva ocupa su lugar;
/// si no, va encima de la cita o del reenvío ([`SignaturePlacement::AboveQuote`]) o al final.
pub fn apply(body: &mut RichBody, signature: Option<&RichBody>, placement: SignaturePlacement) {
    let old = body.remove_signature();
    let Some(sig) = signature.filter(|s| !s.is_empty()) else { return };
    // Una firma no lleva firma dentro.
    let blocks: Vec<Block> = sig.blocks.iter().filter(|b| !matches!(b, Block::Signature { .. })).cloned().collect();
    let block = Block::Signature { blocks };
    let at = old.unwrap_or_else(|| match placement {
        SignaturePlacement::AboveQuote => quote_anchor(&body.blocks).unwrap_or(body.blocks.len()),
        SignaturePlacement::BelowQuote => body.blocks.len(),
    });
    let at = at.min(body.blocks.len());
    body.blocks.insert(at, block);
}

/// Posición donde empieza lo citado: la línea de atribución («… escribió:») antes de la primera
/// cita, o el encabezado de un reenvío.
fn quote_anchor(blocks: &[Block]) -> Option<usize> {
    for (i, b) in blocks.iter().enumerate() {
        match b {
            Block::Quote { .. } => {
                let prev = i.checked_sub(1).and_then(|p| blocks.get(p).map(|b| (p, b)));
                if let Some((p, Block::Paragraph { .. })) = prev {
                    let text = RichBody { blocks: vec![blocks.get(p).cloned().unwrap_or(Block::Rule)] }.text_content();
                    if text.trim_end().ends_with(':') {
                        return Some(p);
                    }
                }
                return Some(i);
            }
            Block::Paragraph { .. } => {
                let text = RichBody { blocks: vec![b.clone()] }.text_content();
                if text.trim_start().starts_with(FORWARD_MARK) {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(sig: Option<&str>) -> Identity {
        Identity { name: "Ana".into(), email: "ana@acme.mx".into(), signature: sig.map(String::from), reply_to: None }
    }

    #[test]
    fn apply_above_quote_and_replace() {
        let mut body = RichBody {
            blocks: vec![
                Block::paragraph(""),
                Block::paragraph("El lun, Bob escribió:"),
                Block::Quote { blocks: vec![Block::paragraph("hola")] },
            ],
        };
        let sig = RichBody::from_plain("Ana\nAcme");
        apply(&mut body, Some(&sig), SignaturePlacement::AboveQuote);
        assert!(matches!(body.blocks[1], Block::Signature { .. }));
        let other = RichBody::from_plain("Otra");
        apply(&mut body, Some(&other), SignaturePlacement::BelowQuote);
        assert!(matches!(&body.blocks[1], Block::Signature { blocks } if blocks == &other.blocks));
        apply(&mut body, None, SignaturePlacement::AboveQuote);
        assert!(body.signature().is_none());
        apply(&mut body, Some(&sig), SignaturePlacement::BelowQuote);
        assert!(matches!(body.blocks.last(), Some(Block::Signature { .. })));
    }

    #[test]
    fn book_lookup_and_fallback() {
        let mut book = SignatureBook::default();
        assert!(book.for_identity(&ident(None), false).is_none());
        let html = ident(Some("<b>Ana</b><br>Acme"));
        assert_eq!(book.for_identity(&html, false).unwrap().text_content(), "Ana\nAcme");
        book.upsert(Signature { name: "corta".into(), body: RichBody::from_plain("A.") });
        book.upsert(Signature { name: "larga".into(), body: RichBody::from_plain("Ana López") });
        book.assign(
            "ANA@acme.mx",
            IdentitySignatures { new_message: Some("larga".into()), reply: Some("corta".into()) },
        );
        assert_eq!(book.for_identity(&html, false).unwrap().text_content(), "Ana López");
        assert_eq!(book.for_identity(&html, true).unwrap().text_content(), "A.");
        book.remove("corta").unwrap();
        assert_eq!(book.for_identity(&html, true).unwrap().text_content(), "Ana López");
        assert!(book.remove("nada").is_err());
    }

    #[test]
    fn book_persists() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let mut book = SignatureBook::load(&store).unwrap();
        book.upsert(Signature { name: "x".into(), body: RichBody::from_plain("X") });
        book.save(&store).unwrap();
        assert_eq!(SignatureBook::load(&store).unwrap(), book);
    }
}
