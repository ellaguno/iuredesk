//! S1.34: el caso de Iurefficient de cada tarea que viene de la instancia, para agruparlas «por
//! caso» (y llegar al plan de IureProject de ese caso). La instancia lo da con cada tarea
//! (`case_id`, `case_title`); `Task` no tiene dónde guardarlo y cambiarlo pediría una migración de
//! todo lo que crea tareas, así que va aparte.
//!
//! Tabla aditiva `task_cases`: id remoto de la tarea (`iure:<id>`, estable entre sincronizaciones)
//! → [`TaskCase`]. No cambia la versión de esquema.

use redb::{ReadableDatabase, ReadableTable};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::tables::TASK_CASES;
use crate::{Store, codec};

/// Casos que se leen como mucho (las tareas de Iurefficient se acotan a 500).
pub const MAX_TASK_CASES: usize = 2_000;

/// El caso de una tarea.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskCase {
    /// Id del caso en la instancia.
    pub id: String,
    /// Su nombre visible («1452/2026 · Arrendamiento»).
    pub title: String,
}

impl Store {
    /// El caso de la tarea con ese id remoto, si se sabe.
    pub fn task_case(&self, remote_id: &str) -> Result<Option<TaskCase>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(TASK_CASES)?;
        match t.get(remote_id)? {
            Some(g) => codec::decode(g.value()).map(Some),
            None => Ok(None),
        }
    }

    /// Todos los casos conocidos (id remoto de la tarea → caso), acotado.
    pub fn task_cases(&self) -> Result<Vec<(String, TaskCase)>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(TASK_CASES)?;
        let mut out = Vec::new();
        for row in t.iter()?.take(MAX_TASK_CASES) {
            let (k, v) = row?;
            if let Ok(c) = codec::decode::<TaskCase>(v.value()) {
                out.push((k.value().to_string(), c));
            }
        }
        Ok(out)
    }

    /// Guarda (o quita, con `None`) el caso de una tarea.
    pub fn set_task_case(&self, remote_id: &str, case: Option<&TaskCase>) -> Result<()> {
        let key = codec::clip(remote_id.trim(), 300).to_string();
        if key.is_empty() {
            return Err(crate::StoreError::Invalid("tarea sin id remoto".into()));
        }
        let case = case.map(|c| TaskCase {
            id: codec::clip(&c.id, 200).to_string(),
            title: codec::clip(&c.title, 500).to_string(),
        });
        self.write(|tx, _| {
            let mut t = tx.open_table(TASK_CASES)?;
            match &case {
                Some(c) => {
                    t.insert(key.as_str(), codec::encode(c)?.as_slice())?;
                }
                None => {
                    t.remove(key.as_str())?;
                }
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_cases_are_kept_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let c = TaskCase { id: "c1".into(), title: "1452/2026".into() };
        s.set_task_case("iure:t1", Some(&c)).unwrap();
        assert_eq!(s.task_case("iure:t1").unwrap(), Some(c.clone()));
        assert_eq!(s.task_cases().unwrap(), vec![("iure:t1".to_string(), c)]);
        s.set_task_case("iure:t1", None).unwrap();
        assert_eq!(s.task_case("iure:t1").unwrap(), None);
        assert!(s.set_task_case("  ", None).is_err());
    }
}
