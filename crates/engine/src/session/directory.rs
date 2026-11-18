//! Libreta global de la organización al escribir destinatarios (F8.4): además de los contactos
//! locales, los nombres del directorio de Exchange (`ResolveNames`).
//!
//! La búsqueda va por la red, así que no bloquea: [`Session::directory_lookup`] la lanza en
//! segundo plano (una por texto, con caché acotada) y [`Session::suggest_addresses`] suma lo que
//! ya haya llegado, filtrado por lo escrito (los resultados de «ana» sirven mientras llegan los
//! de «ana p»). Cuando llega una respuesta sube [`Session::directory_generation`] y la
//! interfaz vuelve a pedir las sugerencias.

use std::collections::{HashMap, HashSet, VecDeque};

use iuredesk_model::Mailbox;

use super::{EngineJob, JobOutput, Session};

/// Letras mínimas para preguntar al directorio.
const MIN_QUERY: usize = 3;
/// Búsquedas que se recuerdan.
const MAX_CACHED: usize = 64;
/// Tiempo máximo de una búsqueda.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Estado de la libreta global en la sesión.
#[derive(Debug, Default)]
pub(crate) struct Directory {
    cache: HashMap<String, Vec<Mailbox>>,
    order: VecDeque<String>,
    pending: HashSet<String>,
    generation: u64,
}

impl Directory {
    /// Resultados guardados para `needle` o, si no, para el prefijo más largo ya buscado.
    fn lookup(&self, needle: &str) -> Option<&Vec<Mailbox>> {
        let mut end = needle.len();
        loop {
            let prefix = needle.get(..end)?;
            if prefix.chars().count() < MIN_QUERY {
                return None;
            }
            if let Some(v) = self.cache.get(prefix) {
                return Some(v);
            }
            end = prefix.char_indices().last().map(|(i, _)| i)?;
        }
    }

    fn insert(&mut self, query: String, result: Vec<Mailbox>) {
        self.pending.remove(&query);
        if !self.cache.contains_key(&query) {
            self.order.push_back(query.clone());
        }
        self.cache.insert(query, result);
        while self.order.len() > MAX_CACHED {
            if let Some(old) = self.order.pop_front() {
                self.cache.remove(&old);
            }
        }
        self.generation = self.generation.wrapping_add(1);
    }
}

impl Session {
    /// Pide al directorio de la organización lo que coincide con `text` (si hay alguna cuenta de
    /// Exchange y no se buscó ya). La respuesta llega por [`Session::poll`].
    pub fn directory_lookup(&mut self, text: &str) {
        let needle = text.trim().to_lowercase();
        if needle.chars().count() < MIN_QUERY
            || self.directory.cache.contains_key(&needle)
            || self.directory.pending.contains(&needle)
        {
            return;
        }
        let Some(handle) = self.pim.handle.clone().filter(|h| h.has_directory()) else { return };
        self.directory.pending.insert(needle.clone());
        let query = needle.clone();
        self.bg.spawn(async move {
            let result = tokio::time::timeout(TIMEOUT, handle.directory_search(&query, 50)).await.unwrap_or_default();
            JobOutput::Engine(EngineJob::Directory { query, result })
        });
    }

    /// Sube cada vez que llegan resultados del directorio (la interfaz vuelve a pedir las
    /// sugerencias).
    pub fn directory_generation(&self) -> u64 {
        self.directory.generation
    }

    pub(crate) fn on_directory(&mut self, query: String, result: Vec<Mailbox>) {
        self.directory.insert(query, result);
    }

    /// Lo que el directorio ya dio para `needle` (en minúsculas), filtrado por nombre o dirección.
    pub(crate) fn directory_matches(&self, needle: &str) -> Vec<Mailbox> {
        self.directory
            .lookup(needle)
            .map(|v| {
                v.iter()
                    .filter(|m| {
                        m.addr.to_lowercase().contains(needle)
                            || m.name.as_deref().is_some_and(|n| n.to_lowercase().contains(needle))
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_results_serve_longer_queries() {
        let mut d = Directory::default();
        d.insert("ana".into(), vec![Mailbox::new("Ana Pérez", "ana@contoso.com")]);
        assert!(d.lookup("ana p").is_some());
        assert!(d.lookup("an").is_none());
        assert!(d.lookup("bea").is_none());
        assert_eq!(d.generation, 1);
        for i in 0..100 {
            d.insert(format!("q{i:03}"), Vec::new());
        }
        assert!(d.cache.len() <= MAX_CACHED);
        assert!(d.lookup("ana").is_none(), "lo más viejo se olvida");
        // Con acentos (varios bytes por letra) no corta a media letra.
        d.insert("añoñ".into(), Vec::new());
        assert!(d.lookup("añoño").is_some());
    }
}
