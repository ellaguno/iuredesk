//! Calendarios compartidos entre cuentas (BUG-DUP).
//!
//! Dos cuentas configuradas pueden ver **la misma colección remota**: la cuenta de Workspace
//! suscrita al calendario principal de la cuenta de Gmail, un calendario compartido entre
//! compañeros, etc. Cada cuenta lo sincroniza como un calendario local propio y, sin más, cada
//! evento se vería dos veces. Aquí se detecta: misma URL de colección (normalizada) en más de un
//! calendario local → uno es el **primario** (el de la cuenta dueña: la ruta contiene su
//! dirección; si no, el de la cuenta configurada primero) y los demás son **duplicados**
//! («También en …»). Las vistas ocultan los duplicados por omisión y las ediciones van por el
//! primario.
//!
//! Sin E/S: el almacén y el motor le pasan los calendarios y las direcciones de las cuentas.

use std::collections::BTreeMap;

use crate::calendar::Calendar;
use crate::ids::{AccountId, CalendarId};

/// Longitud máxima de URL que se normaliza (lo demás no se compara).
const MAX_URL: usize = 4_096;

/// Forma canónica de la URL de una colección, para comparar las que vienen de cuentas distintas:
/// esquema y servidor en minúsculas, sin usuario ni puerto por omisión, `%XX` de caracteres
/// seguros decodificados (`%40` = `@`), escapes restantes en mayúsculas, sin fragmento y sin
/// barras finales. `None` si no es una URL absoluta (ids opacos de Graph, calendarios locales).
pub fn normalize_collection_url(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > MAX_URL {
        return None;
    }
    let (scheme, rest) = raw.split_once("://")?;
    if scheme.is_empty() || !scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
        return None;
    }
    let scheme = scheme.to_ascii_lowercase();
    let rest = rest.split('#').next().unwrap_or_default().trim_end();
    let auth_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(auth_end);
    // Sin credenciales (`usuario@servidor`).
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h).to_ascii_lowercase();
    if host_port.is_empty() {
        return None;
    }
    let host = match (scheme.as_str(), host_port.rsplit_once(':')) {
        ("https", Some((h, "443"))) | ("http", Some((h, "80"))) => h.to_string(),
        _ => host_port,
    };
    if host.is_empty() {
        return None;
    }
    let (path, query) = match tail.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (tail, None),
    };
    let mut path = canonical_escapes(path);
    while path.ends_with(|c: char| c == '/' || c.is_whitespace()) {
        path.pop();
    }
    let mut out = format!("{scheme}://{host}{path}");
    if let Some(q) = query.filter(|q| !q.is_empty()) {
        out.push('?');
        out.push_str(&canonical_escapes(q));
    }
    Some(out)
}

/// Decodifica los `%XX` que no cambian el sentido de la ruta y deja los demás en mayúsculas.
fn canonical_escapes(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        let c = b.get(i).copied().unwrap_or_default();
        if c == b'%'
            && let (Some(h), Some(l)) = (b.get(i.saturating_add(1)), b.get(i.saturating_add(2)))
            && let (Some(h), Some(l)) = (hex(*h), hex(*l))
        {
            let v = h.saturating_mul(16).saturating_add(l);
            if v.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@".contains(&v) {
                out.push(char::from(v));
            } else {
                out.push_str(&format!("%{v:02X}"));
            }
            i = i.saturating_add(3);
            continue;
        }
        // Sigue el texto tal cual (UTF-8 incluido): se copia el carácter entero.
        let ch = s.get(i..).and_then(|t| t.chars().next()).unwrap_or('\u{FFFD}');
        out.push(ch);
        i = i.saturating_add(ch.len_utf8().max(1));
    }
    out
}

fn hex(c: u8) -> Option<u8> {
    char::from(c).to_digit(16).and_then(|d| u8::try_from(d).ok())
}

/// Qué calendarios locales son copias de una misma colección remota y cuál manda.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SharedCalendars {
    /// Duplicado → primario.
    primary: BTreeMap<CalendarId, CalendarId>,
}

impl SharedCalendars {
    /// Detecta los duplicados. `owner_email` da la dirección de cada cuenta (para saber de quién
    /// es la colección).
    pub fn detect(cals: &[Calendar], owner_email: impl Fn(AccountId) -> Option<String>) -> Self {
        let mut groups: BTreeMap<String, Vec<&Calendar>> = BTreeMap::new();
        for c in cals.iter().take(10_000) {
            if let Some(key) = normalize_collection_url(&c.remote_path) {
                groups.entry(key).or_default().push(c);
            }
        }
        let mut primary = BTreeMap::new();
        for (url, members) in groups {
            if members.len() < 2 {
                continue;
            }
            let path = url.split_once("://").map_or(url.as_str(), |(_, r)| r).to_lowercase();
            let owns = |c: &Calendar| {
                owner_email(c.account)
                    .map(|e| e.trim().to_lowercase())
                    .is_some_and(|e| e.contains('@') && path.contains(&e))
            };
            let order = |c: &&&Calendar| (c.account, c.id);
            let main =
                members.iter().filter(|c| owns(c)).min_by_key(order).or_else(|| members.iter().min_by_key(order));
            let Some(main) = main else { continue };
            for c in members.iter().filter(|c| c.id != main.id) {
                primary.insert(c.id, main.id);
            }
        }
        Self { primary }
    }

    /// `true` si no hay calendarios compartidos.
    pub fn is_empty(&self) -> bool {
        self.primary.is_empty()
    }

    /// El primario, si `id` es un duplicado.
    pub fn primary_of(&self, id: CalendarId) -> Option<CalendarId> {
        self.primary.get(&id).copied()
    }

    /// `true` si `id` es la copia de otra cuenta.
    pub fn is_duplicate(&self, id: CalendarId) -> bool {
        self.primary.contains_key(&id)
    }

    /// El calendario que representa la colección de `id` (él mismo si no es duplicado).
    pub fn canonical(&self, id: CalendarId) -> CalendarId {
        self.primary_of(id).unwrap_or(id)
    }

    /// Duplicados de `primary`.
    pub fn duplicates_of(&self, primary: CalendarId) -> Vec<CalendarId> {
        self.primary.iter().filter(|(_, p)| **p == primary).map(|(d, _)| *d).collect()
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn same_collection_in_several_spellings() {
        let a = "https://apidata.googleusercontent.com/caldav/v2/eduardo.llaguno%40gmail.com/events/";
        let b = "HTTPS://ApiData.GoogleUserContent.com:443/caldav/v2/eduardo.llaguno@gmail.com/events";
        let c = "https://apidata.googleusercontent.com/caldav/v2/eduardo.llaguno%40gmail.com/events//#x";
        let n = normalize_collection_url(a).unwrap();
        assert_eq!(n, "https://apidata.googleusercontent.com/caldav/v2/eduardo.llaguno@gmail.com/events");
        assert_eq!(normalize_collection_url(b).as_deref(), Some(n.as_str()));
        assert_eq!(normalize_collection_url(c).as_deref(), Some(n.as_str()));
        // Con credenciales y escapes en minúsculas.
        assert_eq!(
            normalize_collection_url("http://user@Example.org:80/a%2fb/%7euser/").as_deref(),
            Some("http://example.org/a%2Fb/~user")
        );
    }

    #[test]
    fn different_collections_stay_different() {
        let n = |s| normalize_collection_url(s).unwrap();
        assert_ne!(n("https://h/cal/a/"), n("https://h/cal/b/"));
        assert_ne!(n("https://h/cal/a"), n("http://h/cal/a"));
        assert_ne!(n("https://h:8443/cal"), n("https://h/cal"));
        assert_ne!(n("https://h/a%2Fb"), n("https://h/a/b"));
    }

    #[test]
    fn opaque_ids_are_not_urls() {
        assert_eq!(normalize_collection_url(""), None);
        assert_eq!(normalize_collection_url("AAMkAGI2TG93AAA="), None);
        assert_eq!(normalize_collection_url("gtasks:L1"), None);
        assert_eq!(normalize_collection_url("https://"), None);
    }

    fn cal(id: u64, account: u64, path: &str) -> Calendar {
        Calendar { id: CalendarId(id), account: AccountId(account), remote_path: path.into(), ..Default::default() }
    }

    #[test]
    fn owner_wins_else_first_account() {
        let shared = "https://apidata.googleusercontent.com/caldav/v2/eduardo.llaguno%40gmail.com/events/";
        // La cuenta 1 (Workspace) se configuró primero, pero la colección es de la 2 (Gmail).
        let cals = vec![
            cal(10, 1, "https://apidata.googleusercontent.com/caldav/v2/eduardo%40llaguno.com/events/"),
            cal(11, 1, shared),
            cal(20, 2, "https://apidata.googleusercontent.com/caldav/v2/eduardo.llaguno@gmail.com/events"),
        ];
        let email = |a: AccountId| {
            Some(if a == AccountId(1) { "eduardo@llaguno.com" } else { "Eduardo.Llaguno@gmail.com" }.to_string())
        };
        let s = SharedCalendars::detect(&cals, email);
        assert_eq!(s.primary_of(CalendarId(11)), Some(CalendarId(20)));
        assert!(!s.is_duplicate(CalendarId(20)) && !s.is_duplicate(CalendarId(10)));
        assert_eq!(s.canonical(CalendarId(11)), CalendarId(20));
        assert_eq!(s.duplicates_of(CalendarId(20)), vec![CalendarId(11)]);
        // Nadie es dueño: gana la cuenta configurada primero.
        let cals = vec![cal(30, 3, "https://dav.example/shared/team/"), cal(21, 2, "https://dav.example/shared/team")];
        let s = SharedCalendars::detect(&cals, |_| None);
        assert_eq!(s.primary_of(CalendarId(30)), Some(CalendarId(21)));
        assert!(SharedCalendars::detect(&cals[..1], |_| None).is_empty());
    }

    proptest! {
        #[test]
        fn normalize_never_panics_and_is_idempotent(s in "\\PC{0,80}") {
            if let Some(n) = normalize_collection_url(&s) {
                prop_assert_eq!(normalize_collection_url(&n), Some(n.clone()));
            }
        }

        #[test]
        fn normalize_url_shapes(host in "[a-zA-Z0-9.-]{1,20}", path in "(/[a-zA-Z0-9%@._~-]{0,12}){0,5}/?") {
            let u = format!("https://{host}{path}");
            if let Some(n) = normalize_collection_url(&u) {
                prop_assert_eq!(normalize_collection_url(&n), Some(n.clone()));
                prop_assert!(!n.ends_with('/'));
            }
        }
    }
}
