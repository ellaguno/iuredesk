//! Fusión a tres bandas tras un 412 (el elemento cambió en el servidor mientras lo editábamos).
//!
//! `base` es la versión de la que partió la edición local (el objeto crudo guardado), `local` la
//! editada y `server` la que hay ahora en el servidor. Campo por campo:
//! - solo cambió en local → gana lo local;
//! - solo cambió en el servidor (o ambos igual) → gana el servidor;
//! - cambió distinto en los dos → **conflicto**: no se fusiona y gana la versión del servidor
//!   (se avisa a la interfaz con `PimEvent::Conflict`).
//!
//! El resultado es la versión del servidor con los cambios locales encima, que luego se *parchea*
//! sobre el objeto del servidor (sin perder lo que puso el otro cliente).

use iuredesk_model::{Contact, Event, Task};

macro_rules! merge_fields {
    ($base:expr, $local:expr, $server:expr, $out:ident, [$($f:ident),* $(,)?]) => {{
        let mut conflict = false;
        $(
            if $local.$f != $base.$f {
                if $server.$f != $base.$f && $server.$f != $local.$f {
                    conflict = true;
                } else {
                    $out.$f = $local.$f.clone();
                }
            }
        )*
        conflict
    }};
}

/// Evento fusionado, o `None` si hay un campo en conflicto.
pub fn merge_event(base: &Event, local: &Event, server: &Event) -> Option<Event> {
    let mut out = server.clone();
    let conflict = merge_fields!(
        base,
        local,
        server,
        out,
        [
            summary,
            location,
            description,
            start,
            end,
            rrule,
            exdates,
            organizer,
            attendees,
            reminders,
            status,
            transparency,
        ]
    );
    if conflict {
        return None;
    }
    out.sequence = server.sequence.max(local.sequence);
    Some(out)
}

/// Contacto fusionado, o `None` si hay conflicto.
pub fn merge_contact(base: &Contact, local: &Contact, server: &Contact) -> Option<Contact> {
    let mut out = server.clone();
    let conflict = merge_fields!(
        base,
        local,
        server,
        out,
        [full_name, given_name, family_name, nickname, emails, phones, org, title, addresses, notes, birthday, photo,]
    );
    (!conflict).then_some(out)
}

/// Tarea fusionada, o `None` si hay conflicto.
pub fn merge_task(base: &Task, local: &Task, server: &Task) -> Option<Task> {
    let mut out = server.clone();
    let conflict = merge_fields!(
        base,
        local,
        server,
        out,
        [summary, description, due, priority, status, percent_complete, completed,]
    );
    (!conflict).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disjoint_changes_merge() {
        let base = Event { summary: "a".into(), location: "x".into(), ..Default::default() };
        let local = Event { summary: "b".into(), ..base.clone() };
        let server = Event { location: "y".into(), sequence: 2, ..base.clone() };
        let m = merge_event(&base, &local, &server).unwrap();
        assert_eq!((m.summary.as_str(), m.location.as_str(), m.sequence), ("b", "y", 2));
    }

    #[test]
    fn same_field_conflicts() {
        let base = Task { summary: "a".into(), ..Default::default() };
        let local = Task { summary: "b".into(), ..base.clone() };
        let server = Task { summary: "c".into(), ..base.clone() };
        assert!(merge_task(&base, &local, &server).is_none());
        // Mismo cambio en los dos: no es conflicto.
        assert!(merge_task(&base, &local, &local).is_some());
        let cb = Contact { org: "a".into(), ..Default::default() };
        assert!(
            merge_contact(&cb, &Contact { org: "b".into(), ..cb.clone() }, &Contact { org: "c".into(), ..cb.clone() })
                .is_none()
        );
    }
}
