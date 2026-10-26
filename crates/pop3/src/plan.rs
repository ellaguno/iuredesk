//! Planificador puro de la descarga POP3 incremental (sin E/S).

use std::collections::{HashMap, HashSet};

/// Qué hacer con los mensajes del servidor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pop3Policy {
    /// Dejar copia en el servidor (si es `false`, se borra al descargar).
    pub leave_on_server: bool,
    /// Con copia en el servidor: borrar los que llevan más de N días descargados.
    pub delete_after_days: Option<u32>,
}

impl Default for Pop3Policy {
    /// Lo prudente: dejar copia y no borrar nunca.
    fn default() -> Self {
        Self { leave_on_server: true, delete_after_days: None }
    }
}

/// Resultado del plan.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DownloadPlan {
    /// `(número, uidl)` a descargar, del más nuevo al más viejo.
    pub download: Vec<(u32, String)>,
    /// `(número, uidl)` ya descargados que hay que borrar del servidor.
    pub delete: Vec<(u32, String)>,
    /// UIDLs conocidos que ya no están en el servidor (se olvidan).
    pub forget: Vec<String>,
}

/// Compara el `UIDL` del servidor con lo ya descargado (`known`: uidl → segundo Unix en que se
/// descargó) y decide. `max_download` acota cuántos se bajan en una pasada.
pub fn plan_download(
    server: &[(u32, String)],
    known: &HashMap<String, i64>,
    policy: Pop3Policy,
    now_unix: i64,
    max_download: usize,
) -> DownloadPlan {
    let mut plan = DownloadPlan::default();
    let mut seen = HashSet::new();
    let mut fresh: Vec<(u32, String)> = Vec::new();
    for (n, uid) in server {
        // Un servidor hostil podría repetir UIDLs: solo cuenta el primero.
        if !seen.insert(uid.as_str()) {
            continue;
        }
        match known.get(uid) {
            None => fresh.push((*n, uid.clone())),
            Some(&when) => {
                let expired = match (policy.leave_on_server, policy.delete_after_days) {
                    (false, _) => true,
                    (true, Some(days)) => now_unix.saturating_sub(when) >= i64::from(days).saturating_mul(86_400),
                    (true, None) => false,
                };
                if expired {
                    plan.delete.push((*n, uid.clone()));
                }
            }
        }
    }
    fresh.sort_by_key(|b| std::cmp::Reverse(b.0));
    fresh.truncate(max_download);
    plan.download = fresh;
    plan.forget = known.keys().filter(|k| !seen.contains(k.as_str())).cloned().collect();
    plan.forget.sort();
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn srv(ids: &[&str]) -> Vec<(u32, String)> {
        ids.iter().enumerate().map(|(i, s)| (u32::try_from(i).unwrap() + 1, s.to_string())).collect()
    }

    #[test]
    fn new_messages_newest_first_and_forget_gone() {
        let known: HashMap<String, i64> = [("a".to_string(), 0), ("z".to_string(), 0)].into();
        let p = plan_download(&srv(&["a", "b", "c"]), &known, Pop3Policy::default(), 100, 10);
        assert_eq!(p.download, vec![(3, "c".into()), (2, "b".into())]);
        assert!(p.delete.is_empty());
        assert_eq!(p.forget, vec!["z".to_string()]);
        let p = plan_download(&srv(&["a", "b", "c"]), &known, Pop3Policy::default(), 100, 1);
        assert_eq!(p.download, vec![(3, "c".into())]);
    }

    #[test]
    fn delete_policies() {
        let day = 86_400;
        let known: HashMap<String, i64> = [("old".to_string(), 0), ("new".to_string(), 9 * day)].into();
        let keep = Pop3Policy { leave_on_server: true, delete_after_days: Some(7) };
        let p = plan_download(&srv(&["old", "new"]), &known, keep, 10 * day, 10);
        assert_eq!(p.delete, vec![(1, "old".into())]);
        let gone = Pop3Policy { leave_on_server: false, delete_after_days: None };
        let p = plan_download(&srv(&["old", "new", "x"]), &known, gone, 10 * day, 10);
        assert_eq!(p.delete.len(), 2);
        assert_eq!(p.download, vec![(3, "x".into())]);
    }

    #[test]
    fn duplicate_uidls_are_ignored() {
        let p = plan_download(&srv(&["a", "a"]), &HashMap::new(), Pop3Policy::default(), 0, 10);
        assert_eq!(p.download, vec![(1, "a".into())]);
    }
}
