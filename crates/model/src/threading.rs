//! Enhebrado de mensajes al estilo JWZ (<https://www.jwz.org/doc/threading.html>).
//!
//! Función pura: recibe los sobres de una cuenta y devuelve los hilos. Sigue el algoritmo de
//! Jamie Zawinski (`References`/`In-Reply-To`, contenedores vacíos para mensajes que faltan,
//! poda y agrupación por asunto) con dos ajustes conservadores:
//!
//! - **Agrupación por asunto solo para respuestas:** un mensaje raíz se une a otro hilo por asunto
//!   únicamente si su asunto lleva prefijo de respuesta (`Re:`, `RV:`, `AW:`…). Así dos correos
//!   distintos titulados «Hola» o dos boletines con el mismo título no se mezclan.
//! - **Sin recursión:** todos los recorridos son iterativos y acotados, porque una cadena de
//!   `References` hostil podría ser arbitrariamente profunda.

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use crate::ids::MessageId;
use crate::mail::{MAX_REFERENCES, MessageEnvelope, normalize_subject};

/// Longitud máxima de un `Message-ID` que se toma en cuenta (RFC 5322: línea ≤ 998).
const MAX_MSGID_LEN: usize = 998;

/// Datos mínimos de un mensaje para enhebrarlo.
#[derive(Clone, Debug)]
pub struct ThreadInput<'a> {
    /// Id local.
    pub id: MessageId,
    /// `Message-ID` (con o sin `<>`).
    pub message_id: Option<&'a str>,
    /// `In-Reply-To` (con o sin `<>`).
    pub in_reply_to: Option<&'a str>,
    /// `References`, del más antiguo al más reciente.
    pub references: &'a [String],
    /// Asunto tal cual.
    pub subject: &'a str,
    /// Fecha.
    pub date: DateTime<Utc>,
}

impl<'a> From<&'a MessageEnvelope> for ThreadInput<'a> {
    fn from(e: &'a MessageEnvelope) -> Self {
        Self {
            id: e.id,
            message_id: e.message_id.as_deref(),
            in_reply_to: e.in_reply_to.as_deref(),
            references: &e.references,
            subject: &e.subject,
            date: e.date,
        }
    }
}

/// Un mensaje dentro de un hilo calculado.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadMember {
    /// Mensaje.
    pub id: MessageId,
    /// Mensaje padre más cercano presente (los contenedores vacíos se saltan).
    pub parent: Option<MessageId>,
    /// Profundidad (0 = raíz).
    pub depth: u32,
}

/// Hilo calculado: miembros en orden de lectura (preorden, hermanos por fecha).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadGroup {
    /// Primer mensaje del hilo.
    pub root: MessageId,
    /// Asunto normalizado de la raíz.
    pub subject: String,
    /// Fecha del mensaje más reciente.
    pub last_date: DateTime<Utc>,
    /// Miembros en preorden.
    pub members: Vec<ThreadMember>,
}

#[derive(Default)]
struct Container {
    msg: Option<usize>,
    parent: Option<usize>,
    children: Vec<usize>,
    dead: bool,
}

struct Forest {
    c: Vec<Container>,
}

impl Forest {
    fn new_container(&mut self) -> usize {
        self.c.push(Container::default());
        self.c.len().saturating_sub(1)
    }

    fn parent(&self, i: usize) -> Option<usize> {
        self.c.get(i).and_then(|c| c.parent)
    }

    /// `true` si `a` es `b` o un ancestro de `b`.
    fn is_ancestor_or_self(&self, a: usize, b: usize) -> bool {
        let mut cur = Some(b);
        // Cota: nunca más pasos que contenedores (un ciclo no puede existir, pero por si acaso).
        for _ in 0..=self.c.len() {
            match cur {
                Some(x) if x == a => return true,
                Some(x) => cur = self.parent(x),
                None => return false,
            }
        }
        true
    }

    fn unlink(&mut self, child: usize) {
        if let Some(p) = self.parent(child) {
            if let Some(pc) = self.c.get_mut(p) {
                pc.children.retain(|&x| x != child);
            }
            if let Some(cc) = self.c.get_mut(child) {
                cc.parent = None;
            }
        }
    }

    fn link(&mut self, parent: usize, child: usize) {
        self.unlink(child);
        if let Some(cc) = self.c.get_mut(child) {
            cc.parent = Some(parent);
        }
        if let Some(pc) = self.c.get_mut(parent) {
            pc.children.push(child);
        }
    }

    /// Postorden iterativo de todo el bosque vivo (hijos antes que padres).
    fn postorder(&self) -> Vec<usize> {
        let mut out = Vec::with_capacity(self.c.len());
        let mut stack: Vec<(usize, bool)> = Vec::new();
        for (i, c) in self.c.iter().enumerate() {
            if c.dead || c.parent.is_some() {
                continue;
            }
            stack.push((i, false));
            while let Some((n, expanded)) = stack.pop() {
                if expanded {
                    out.push(n);
                    continue;
                }
                stack.push((n, true));
                if let Some(c) = self.c.get(n) {
                    for &ch in c.children.iter().rev() {
                        stack.push((ch, false));
                    }
                }
            }
        }
        out
    }
}

fn clean_id(s: &str) -> Option<&str> {
    let t = s.trim().trim_start_matches('<').trim_end_matches('>').trim();
    if t.is_empty() || t.len() > MAX_MSGID_LEN { None } else { Some(t) }
}

/// Calcula los hilos de un conjunto de mensajes (normalmente, todos los de una cuenta).
///
/// Los hilos salen ordenados por su mensaje más reciente, del más nuevo al más viejo.
pub fn thread_messages(inputs: &[ThreadInput<'_>]) -> Vec<ThreadGroup> {
    let mut f = Forest { c: Vec::with_capacity(inputs.len().saturating_mul(2)) };
    let mut table: HashMap<&str, usize> = HashMap::with_capacity(inputs.len());

    // 1. Contenedores y enlaces según References / In-Reply-To.
    for (i, m) in inputs.iter().enumerate() {
        let own = m.message_id.and_then(clean_id);
        let this = match own.and_then(|k| table.get(k).copied()) {
            Some(c) if f.c.get(c).is_some_and(|c| c.msg.is_none()) => c,
            _ => {
                let c = f.new_container();
                if let Some(k) = own {
                    // Un Message-ID duplicado queda como contenedor aparte, sin entrada en la tabla.
                    table.entry(k).or_insert(c);
                }
                c
            }
        };
        if let Some(c) = f.c.get_mut(this) {
            c.msg = Some(i);
        }

        let start = m.references.len().saturating_sub(MAX_REFERENCES);
        let mut refs: Vec<&str> = m.references.iter().skip(start).filter_map(|r| clean_id(r)).collect();
        if let Some(irt) = m.in_reply_to.and_then(clean_id)
            && refs.last() != Some(&irt)
        {
            refs.push(irt);
        }

        let mut prev: Option<usize> = None;
        for r in refs {
            let rc = match table.get(r) {
                Some(&c) => c,
                None => {
                    let c = f.new_container();
                    table.insert(r, c);
                    c
                }
            };
            if rc == this {
                // Un mensaje que se cita a sí mismo: se ignora esa referencia.
                continue;
            }
            if let Some(p) = prev
                && f.parent(rc).is_none()
                && !f.is_ancestor_or_self(rc, p)
            {
                f.link(p, rc);
            }
            prev = Some(rc);
        }
        match prev {
            Some(p) if !f.is_ancestor_or_self(this, p) => f.link(p, this),
            Some(_) => {}
            None => f.unlink(this),
        }
    }

    // 2–3. Poda de contenedores vacíos (de abajo arriba).
    for n in f.postorder() {
        let (empty, children, parent) = match f.c.get(n) {
            Some(c) => (c.msg.is_none(), c.children.clone(), c.parent),
            None => continue,
        };
        if !empty {
            continue;
        }
        match parent {
            Some(p) => {
                // Sube los hijos al padre y elimina el vacío.
                f.unlink(n);
                for ch in children {
                    f.link(p, ch);
                }
                if let Some(c) = f.c.get_mut(n) {
                    c.dead = true;
                }
            }
            None if children.len() <= 1 => {
                for ch in children {
                    f.unlink(ch);
                }
                if let Some(c) = f.c.get_mut(n) {
                    c.dead = true;
                }
            }
            None => {}
        }
    }

    // Asunto y fecha representativos de cada contenedor raíz.
    let msg_of = |f: &Forest, n: usize| -> Option<usize> {
        let c = f.c.get(n)?;
        c.msg.or_else(|| c.children.iter().find_map(|&ch| f.c.get(ch).and_then(|x| x.msg)))
    };

    // 4. Agrupación por asunto (solo respuestas; ver documentación del módulo).
    let roots: Vec<usize> =
        f.c.iter().enumerate().filter(|(_, c)| !c.dead && c.parent.is_none()).map(|(i, _)| i).collect();
    let mut by_subject: HashMap<String, (usize, bool, DateTime<Utc>)> = HashMap::new();
    let mut root_info: Vec<(usize, String, bool)> = Vec::with_capacity(roots.len());
    for &r in &roots {
        let Some(mi) = msg_of(&f, r) else { continue };
        let Some(m) = inputs.get(mi) else { continue };
        let (subj, is_reply) = normalize_subject(m.subject);
        if subj.is_empty() {
            continue;
        }
        let better = match by_subject.get(&subj) {
            None => true,
            // Preferencia: no-respuesta antes que respuesta; luego la más antigua.
            Some(&(_, old_reply, old_date)) => (old_reply && !is_reply) || (old_reply == is_reply && m.date < old_date),
        };
        if better {
            by_subject.insert(subj.clone(), (r, is_reply, m.date));
        }
        root_info.push((r, subj, is_reply));
    }
    for (r, subj, is_reply) in root_info {
        if !is_reply {
            continue;
        }
        let Some(&(t, _, _)) = by_subject.get(&subj) else { continue };
        if t == r || f.is_ancestor_or_self(r, t) || f.is_ancestor_or_self(t, r) {
            continue;
        }
        let r_empty = f.c.get(r).is_some_and(|c| c.msg.is_none());
        if r_empty {
            let children = f.c.get(r).map(|c| c.children.clone()).unwrap_or_default();
            for ch in children {
                f.link(t, ch);
            }
            if let Some(c) = f.c.get_mut(r) {
                c.dead = true;
            }
        } else {
            f.link(t, r);
        }
    }

    // Fecha de cada contenedor = la más antigua de su subárbol (para ordenar hermanos).
    let order = f.postorder();
    let mut first_date: Vec<Option<DateTime<Utc>>> = vec![None; f.c.len()];
    let mut last_date: Vec<Option<DateTime<Utc>>> = vec![None; f.c.len()];
    for &n in &order {
        let Some(c) = f.c.get(n) else { continue };
        let own = c.msg.and_then(|i| inputs.get(i)).map(|m| m.date);
        let mut lo = own;
        let mut hi = own;
        for &ch in &c.children {
            let cl = first_date.get(ch).copied().flatten();
            let chh = last_date.get(ch).copied().flatten();
            lo = match (lo, cl) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            hi = match (hi, chh) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            };
        }
        if let Some(x) = first_date.get_mut(n) {
            *x = lo;
        }
        if let Some(x) = last_date.get_mut(n) {
            *x = hi;
        }
    }
    let key = |n: usize| first_date.get(n).copied().flatten();
    for c in &mut f.c {
        c.children.sort_by_key(|&ch| key(ch));
    }

    // 5. Emisión de grupos en preorden.
    let mut groups = Vec::new();
    for (r, c) in f.c.iter().enumerate() {
        if c.dead || c.parent.is_some() {
            continue;
        }
        let mut members = Vec::new();
        // (contenedor, padre-mensaje más cercano, profundidad)
        let mut stack: Vec<(usize, Option<MessageId>, u32)> = vec![(r, None, 0)];
        while let Some((n, pmsg, depth)) = stack.pop() {
            let Some(cn) = f.c.get(n) else { continue };
            let (next_parent, next_depth) = match cn.msg.and_then(|i| inputs.get(i)) {
                Some(m) => {
                    members.push(ThreadMember { id: m.id, parent: pmsg, depth });
                    (Some(m.id), depth.saturating_add(1))
                }
                None => (pmsg, depth),
            };
            for &ch in cn.children.iter().rev() {
                stack.push((ch, next_parent, next_depth));
            }
        }
        let Some(first) = members.first() else { continue };
        let root = first.id;
        let subject = inputs.iter().find(|m| m.id == root).map(|m| normalize_subject(m.subject).0).unwrap_or_default();
        let last = last_date.get(r).copied().flatten().unwrap_or_default();
        groups.push(ThreadGroup { root, subject, last_date: last, members });
    }
    groups.sort_by(|a, b| b.last_date.cmp(&a.last_date).then(a.root.cmp(&b.root)));
    groups
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    struct M {
        id: u64,
        mid: &'static str,
        irt: Option<&'static str>,
        refs: Vec<String>,
        subject: &'static str,
        day: u32,
    }

    fn m(id: u64, mid: &'static str, irt: Option<&'static str>, refs: &[&str], subject: &'static str, day: u32) -> M {
        M { id, mid, irt, refs: refs.iter().map(|s| s.to_string()).collect(), subject, day }
    }

    fn run(ms: &[M]) -> Vec<ThreadGroup> {
        let inputs: Vec<ThreadInput<'_>> = ms
            .iter()
            .map(|x| ThreadInput {
                id: MessageId(x.id),
                message_id: if x.mid.is_empty() { None } else { Some(x.mid) },
                in_reply_to: x.irt,
                references: &x.refs,
                subject: x.subject,
                date: Utc.with_ymd_and_hms(2026, 1, x.day, 0, 0, 0).unwrap(),
            })
            .collect();
        thread_messages(&inputs)
    }

    fn ids(g: &ThreadGroup) -> Vec<u64> {
        g.members.iter().map(|m| m.id.0).collect()
    }

    #[test]
    fn simple_reply_chain() {
        let g = run(&[
            m(3, "<c@x>", Some("<b@x>"), &["<a@x>", "<b@x>"], "Re: Plan", 3),
            m(1, "<a@x>", None, &[], "Plan", 1),
            m(2, "<b@x>", Some("<a@x>"), &["<a@x>"], "Re: Plan", 2),
        ]);
        assert_eq!(g.len(), 1);
        assert_eq!(ids(&g[0]), vec![1, 2, 3]);
        assert_eq!(g[0].root, MessageId(1));
        assert_eq!(g[0].members[2].parent, Some(MessageId(2)));
        assert_eq!(g[0].members[2].depth, 2);
        assert_eq!(g[0].subject, "Plan");
    }

    #[test]
    fn missing_parent_keeps_siblings_together() {
        // El original (a@x) nunca llegó: sus dos respuestas comparten hilo.
        let g = run(&[
            m(1, "<b@x>", Some("<a@x>"), &["<a@x>"], "Re: Viaje", 1),
            m(2, "<c@x>", Some("<a@x>"), &["<a@x>"], "Re: Viaje", 2),
        ]);
        assert_eq!(g.len(), 1);
        assert_eq!(ids(&g[0]), vec![1, 2]);
        assert!(g[0].members.iter().all(|m| m.parent.is_none()));
    }

    #[test]
    fn subject_fallback_only_for_replies() {
        let g = run(&[
            m(1, "<a@x>", None, &[], "Hola", 1),
            m(2, "<b@x>", None, &[], "Re: Hola", 2),
            m(3, "<c@x>", None, &[], "Hola", 3),
        ]);
        // La respuesta se une al «Hola» más antiguo; el otro «Hola» queda aparte.
        assert_eq!(g.len(), 2);
        let joined = g.iter().find(|x| x.members.len() == 2).unwrap();
        assert_eq!(ids(joined), vec![1, 2]);
    }

    #[test]
    fn unrelated_messages_stay_apart() {
        let g =
            run(&[m(1, "<a@x>", None, &[], "Uno", 1), m(2, "<b@x>", None, &[], "Dos", 2), m(3, "", None, &[], "", 3)]);
        assert_eq!(g.len(), 3);
        // Orden: más reciente primero.
        assert_eq!(g[0].root, MessageId(3));
    }

    #[test]
    fn duplicates_and_self_references_do_not_loop() {
        let g = run(&[
            m(1, "<a@x>", Some("<a@x>"), &["<a@x>"], "Bucle", 1),
            m(2, "<a@x>", None, &[], "Bucle", 2),
            m(3, "<b@x>", Some("<c@x>"), &["<c@x>"], "x", 3),
            m(4, "<c@x>", Some("<b@x>"), &["<b@x>"], "x", 4),
        ]);
        let total: usize = g.iter().map(|x| x.members.len()).sum();
        assert_eq!(total, 4);
    }

    #[test]
    fn deep_chain_is_iterative() {
        let n = 5_000u64;
        let mids: Vec<String> = (0..n).map(|i| format!("m{i}@x")).collect();
        let refs: Vec<Vec<String>> =
            (0..n).map(|i| if i == 0 { vec![] } else { vec![mids[(i - 1) as usize].clone()] }).collect();
        let inputs: Vec<ThreadInput<'_>> = (0..n)
            .map(|i| ThreadInput {
                id: MessageId(i + 1),
                message_id: Some(&mids[i as usize]),
                in_reply_to: None,
                references: &refs[i as usize],
                subject: "Re: cadena",
                date: Utc.timestamp_opt(i as i64, 0).unwrap(),
            })
            .collect();
        let g = thread_messages(&inputs);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].members.len(), n as usize);
        assert_eq!(g[0].members.last().unwrap().depth, (n - 1) as u32);
    }

    proptest::proptest! {
        #[test]
        fn every_message_appears_once(
            spec in proptest::collection::vec((0u8..20, proptest::option::of(0u8..20), proptest::collection::vec(0u8..20, 0..4), proptest::bool::ANY), 0..60)
        ) {
            let mids: Vec<String> = (0..20).map(|i| format!("<{i}@p>")).collect();
            let refs: Vec<Vec<String>> = spec.iter().map(|(_, _, r, _)| r.iter().map(|&x| mids[x as usize].clone()).collect()).collect();
            let inputs: Vec<ThreadInput<'_>> = spec.iter().enumerate().map(|(i, (mid, irt, _, reply))| ThreadInput {
                id: MessageId(i as u64 + 1),
                message_id: Some(mids[*mid as usize].as_str()),
                in_reply_to: irt.map(|x| mids[x as usize].as_str()),
                references: &refs[i],
                subject: if *reply { "Re: tema" } else { "tema" },
                date: Utc.timestamp_opt(i as i64 * 60, 0).unwrap(),
            }).collect();
            let g = thread_messages(&inputs);
            let mut seen: Vec<u64> = g.iter().flat_map(|x| x.members.iter().map(|m| m.id.0)).collect();
            seen.sort_unstable();
            let expected: Vec<u64> = (1..=spec.len() as u64).collect();
            proptest::prop_assert_eq!(seen, expected);
        }
    }
}
