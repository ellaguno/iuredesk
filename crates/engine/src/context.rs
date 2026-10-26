//! Panel contextual (a la derecha del mensaje): quién es la persona y qué de Iurefficient está
//! vinculado. Se oculta si no aporta nada.
//!
//! F0.6 no habla todavía con Iurefficient (eso es F3): los vínculos salen de una detección local
//! de referencias de expediente en el asunto («caso 241/2026», «expediente 1452/2026») y de la
//! organización del contacto. Cuando exista la API, `links` vendrá del servidor.

use iuredesk_model::{Contact, MessageEnvelope};

use crate::lang::Text;

/// Tipo de vínculo con Iurefficient.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind {
    Case,
    Lead,
}

/// Un elemento de Iurefficient vinculado al correo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IureLink {
    pub kind: LinkKind,
    pub title: String,
    pub detail: Text,
}

/// Lo que muestra el panel contextual.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContextCard {
    pub contact: Option<Contact>,
    pub links: Vec<IureLink>,
    /// Mensajes de la misma persona en el almacén (aprox., hasta 50).
    pub history: usize,
}

impl ContextCard {
    /// `true` si no hay nada que valga la pena mostrar.
    pub fn is_empty(&self) -> bool {
        self.contact.is_none() && self.links.is_empty()
    }
}

/// Busca «caso 241/2026», «expediente 1452/2026» o «case 12/2026» en un texto.
pub fn case_refs(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let mut out: Vec<String> = Vec::new();
    for key in ["caso ", "expediente ", "case ", "exp. "] {
        let mut from = 0usize;
        for _ in 0..8 {
            let Some(pos) = lower.get(from..).and_then(|s| s.find(key)) else { break };
            let start = from.saturating_add(pos).saturating_add(key.len());
            let tail = lower.get(start..).unwrap_or("");
            let num: String = tail.chars().take_while(|c| c.is_ascii_digit() || *c == '/').take(16).collect();
            if num.contains('/') && num.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                let num = num.trim_end_matches('/').to_string();
                if !out.contains(&num) {
                    out.push(num);
                }
            }
            from = start;
        }
    }
    out
}

/// Organizaciones que el buzón de ejemplo trata como «lead» abierto en el CRM.
const SAMPLE_LEADS: &[&str] = &["Acme Corp.", "Northwind Ltd.", "Constructora Altamira"];

/// Vínculos detectados para un mensaje y su contacto.
pub fn detect_links(env: &MessageEnvelope, contact: Option<&Contact>) -> Vec<IureLink> {
    let mut links: Vec<IureLink> = case_refs(&format!("{} {}", env.subject, env.preview))
        .into_iter()
        .take(3)
        .map(|n| IureLink {
            kind: LinkKind::Case,
            title: n,
            detail: Text::new("Case file · linked by reference", "Expediente · vinculado por referencia"),
        })
        .collect();
    if let Some(c) = contact
        && SAMPLE_LEADS.contains(&c.org.as_str())
    {
        links.push(IureLink {
            kind: LinkKind::Lead,
            title: c.org.clone(),
            detail: Text::new("Open opportunity in the CRM", "Oportunidad abierta en el CRM"),
        });
    }
    links
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_case_numbers() {
        assert_eq!(case_refs("Audiencia caso 241/2026 mañana"), vec!["241/2026"]);
        assert_eq!(case_refs("Fwd: Expediente 1452/2026 — acuerdo"), vec!["1452/2026"]);
        assert!(case_refs("caso perdido 12 de 2026").is_empty());
        assert!(case_refs("").is_empty());
        assert_eq!(case_refs("caso 1/2 y caso 1/2").len(), 1);
    }
}
