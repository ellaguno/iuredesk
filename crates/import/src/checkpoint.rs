//! Estado de reanudación: qué unidades (archivos, carpetas) ya se importaron por completo y hasta
//! qué byte se llegó en cada mbox.
//!
//! Se guarda en `<almacén>/import/checkpoint.json` y se escribe **después** de confirmar cada lote
//! en el almacén: un corte en mitad del lote solo repite ese lote, y la deduplicación evita
//! duplicados. La clave de una unidad incluye tamaño y fecha de modificación, así un archivo que
//! cambió se vuelve a leer entero.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{ImportError, Result};

const VERSION: u32 = 1;
/// Unidades máximas recordadas (más allá se deja de recordar: solo cuesta releer).
const MAX_UNITS: usize = 1_000_000;

#[derive(Debug, Default, Serialize, Deserialize)]
struct Data {
    version: u32,
    done: BTreeSet<String>,
    offsets: BTreeMap<String, u64>,
}

/// Estado de reanudación.
#[derive(Debug)]
pub struct Checkpoint {
    path: Option<PathBuf>,
    data: Data,
    staged_done: Vec<String>,
    staged_offsets: BTreeMap<String, u64>,
    dirty: bool,
}

/// Clave de una unidad: tipo + ruta canónica + tamaño + fecha de modificación.
pub fn unit_key(kind: &str, path: &Path) -> String {
    let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let (size, mtime) = match std::fs::metadata(path) {
        Ok(m) => (
            m.len(),
            m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs()),
        ),
        Err(_) => (0, 0),
    };
    format!("{kind}:{}:{size}:{mtime}", canon.display())
}

impl Checkpoint {
    /// Estado vacío que no se guarda (importación sin reanudación).
    pub fn disabled() -> Self {
        Self {
            path: None,
            data: Data::default(),
            staged_done: Vec::new(),
            staged_offsets: BTreeMap::new(),
            dirty: false,
        }
    }

    /// Carga (o crea vacío) el estado de `dir/checkpoint.json`.
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join("checkpoint.json");
        let data = match std::fs::read(&path) {
            Ok(b) => match serde_json::from_slice::<Data>(&b) {
                Ok(d) if d.version == VERSION => d,
                // Dañado o de otra versión: se empieza de cero (la deduplicación evita duplicados).
                _ => Data::default(),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Data::default(),
            Err(e) => return Err(ImportError::io(&path, e)),
        };
        Ok(Self { path: Some(path), data, staged_done: Vec::new(), staged_offsets: BTreeMap::new(), dirty: false })
    }

    /// `true` si había algo de una importación anterior.
    pub fn has_history(&self) -> bool {
        !self.data.done.is_empty() || !self.data.offsets.is_empty()
    }

    /// ¿La unidad ya se importó entera?
    pub fn is_done(&self, key: &str) -> bool {
        self.data.done.contains(key)
    }

    /// Desplazamiento confirmado de un mbox (0 si no hay).
    pub fn offset(&self, key: &str) -> u64 {
        self.data.offsets.get(key).copied().unwrap_or(0)
    }

    /// Anota (pendiente de confirmar) que la unidad terminó.
    pub fn stage_done(&mut self, key: String) {
        self.staged_done.push(key);
    }

    /// Anota (pendiente de confirmar) hasta dónde se leyó un mbox.
    pub fn stage_offset(&mut self, key: String, offset: u64) {
        self.staged_offsets.insert(key, offset);
    }

    /// Confirma lo anotado (tras guardar el lote en el almacén) y lo escribe a disco.
    pub fn commit(&mut self) -> Result<()> {
        for (k, o) in std::mem::take(&mut self.staged_offsets) {
            if self.data.offsets.len() < MAX_UNITS || self.data.offsets.contains_key(&k) {
                self.data.offsets.insert(k, o);
                self.dirty = true;
            }
        }
        for k in self.staged_done.drain(..) {
            self.data.offsets.remove(&k);
            if self.data.done.len() < MAX_UNITS {
                self.data.done.insert(k);
            }
            self.dirty = true;
        }
        self.save()
    }

    fn save(&mut self) -> Result<()> {
        let Some(path) = &self.path else { return Ok(()) };
        if !self.dirty {
            return Ok(());
        }
        self.data.version = VERSION;
        let bytes = serde_json::to_vec(&self.data).map_err(|e| ImportError::Checkpoint(e.to_string()))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| ImportError::io(dir, e))?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, bytes).map_err(|e| ImportError::io(&tmp, e))?;
        std::fs::rename(&tmp, path).map_err(|e| ImportError::io(path, e))?;
        self.dirty = false;
        Ok(())
    }

    /// Olvida todo (nueva importación desde cero).
    pub fn clear(&mut self) -> Result<()> {
        self.data = Data::default();
        self.staged_done.clear();
        self.staged_offsets.clear();
        self.dirty = true;
        self.save()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staged_until_commit_and_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = Checkpoint::load(dir.path()).unwrap();
        assert!(!c.has_history());
        c.stage_offset("m".into(), 100);
        c.stage_done("d".into());
        assert_eq!(c.offset("m"), 0);
        assert!(!c.is_done("d"));
        c.commit().unwrap();
        let c = Checkpoint::load(dir.path()).unwrap();
        assert_eq!(c.offset("m"), 100);
        assert!(c.is_done("d"));
        assert!(c.has_history());
    }

    #[test]
    fn damaged_file_starts_over() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("checkpoint.json"), b"{no es json").unwrap();
        let c = Checkpoint::load(dir.path()).unwrap();
        assert!(!c.has_history());
    }

    #[test]
    fn unit_key_changes_with_content() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.mbox");
        std::fs::write(&f, b"uno").unwrap();
        let k1 = unit_key("mbox", &f);
        std::fs::write(&f, b"uno dos").unwrap();
        assert_ne!(k1, unit_key("mbox", &f));
    }
}
