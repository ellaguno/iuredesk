//! Direcciones de correo escritas por el usuario: análisis, validación e IDNA.
//!
//! Se aceptan las formas habituales: `ana@acme.mx`, `Ana López <ana@acme.mx>`,
//! `"López, Ana" <ana@acme.mx>`, `mailto:ana@acme.mx`, separadas por `,` o `;`. El dominio puede
//! ir en Unicode (`josé@correo.méxico`); se guarda como lo escribió el usuario (en minúsculas) y se
//! convierte a ASCII (punycode) solo al construir el mensaje ([`ascii_address`]).
//!
//! Las partes locales que no son ASCII (correo internacionalizado, RFC 6531) aún no se admiten: se
//! rechazan con un error claro en vez de enviarse mal.

use iuredesk_model::{MAX_RECIPIENTS, Mailbox};

use crate::lang::Lang;

/// Longitud máxima de una dirección (RFC 5321 §4.5.3.1.3).
pub const MAX_ADDRESS_LEN: usize = 254;
/// Longitud máxima de la parte local.
const MAX_LOCAL_LEN: usize = 64;
/// Longitud máxima de un nombre visible que se conserva.
const MAX_NAME_CHARS: usize = 256;
/// Longitud máxima del texto que se analiza como lista.
const MAX_LIST_INPUT: usize = 64 * 1024;

/// Qué tiene de malo una dirección.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressProblem {
    /// Está vacía.
    Empty,
    /// Falta la `@`.
    MissingAt,
    /// La parte antes de la `@` no es válida.
    BadLocalPart,
    /// La parte local no es ASCII (correo internacionalizado aún no admitido).
    NonAsciiLocalPart,
    /// El dominio no es válido (ni siquiera con IDNA).
    BadDomain,
    /// Es demasiado larga.
    TooLong,
    /// Comillas o `<…>` sin cerrar.
    Unbalanced,
    /// Demasiadas direcciones en la lista.
    TooMany,
}

/// Error de una dirección concreta.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("«{input}»: {problem:?}")]
pub struct AddressError {
    /// Lo que escribió el usuario (recortado).
    pub input: String,
    /// El problema.
    pub problem: AddressProblem,
}

impl AddressError {
    fn new(input: &str, problem: AddressProblem) -> Self {
        let mut input: String = input.trim().chars().take(120).collect();
        input.retain(|c| !c.is_control());
        Self { input, problem }
    }

    /// Mensaje para el usuario.
    pub fn message(&self, lang: Lang) -> String {
        let i = &self.input;
        match (self.problem, lang) {
            (AddressProblem::Empty, _) => lang.pick("The address is empty.", "La dirección está vacía.").to_string(),
            (AddressProblem::MissingAt, Lang::Es) => format!("«{i}» no parece una dirección de correo (falta la @)."),
            (AddressProblem::MissingAt, Lang::En) => format!("“{i}” doesn't look like an email address (missing @)."),
            (AddressProblem::BadLocalPart, Lang::Es) => format!("La parte antes de la @ en «{i}» no es válida."),
            (AddressProblem::BadLocalPart, Lang::En) => format!("The part before the @ in “{i}” is not valid."),
            (AddressProblem::NonAsciiLocalPart, Lang::Es) => {
                format!("«{i}» usa acentos o letras especiales antes de la @; aún no se admite.")
            }
            (AddressProblem::NonAsciiLocalPart, Lang::En) => {
                format!("“{i}” has accents or special letters before the @; that isn't supported yet.")
            }
            (AddressProblem::BadDomain, Lang::Es) => format!("El dominio de «{i}» no es válido."),
            (AddressProblem::BadDomain, Lang::En) => format!("The domain of “{i}” is not valid."),
            (AddressProblem::TooLong, Lang::Es) => format!("«{i}» es demasiado larga."),
            (AddressProblem::TooLong, Lang::En) => format!("“{i}” is too long."),
            (AddressProblem::Unbalanced, Lang::Es) => format!("En «{i}» falta cerrar comillas o «>»."),
            (AddressProblem::Unbalanced, Lang::En) => format!("“{i}” is missing a closing quote or “>”."),
            (AddressProblem::TooMany, _) => lang
                .pick("There are too many addresses in this field.", "Hay demasiadas direcciones en este campo.")
                .to_string(),
        }
    }
}

/// Caracteres permitidos en una parte local sin comillas (RFC 5322 `atext` más el punto).
fn is_atext(c: char) -> bool {
    c.is_ascii_alphanumeric() || "!#$%&'*+-/=?^_`{|}~.".contains(c)
}

/// Valida una dirección pelada (`local@dominio`) y la devuelve normalizada: parte local tal cual,
/// dominio en minúsculas y en Unicode.
pub fn validate_address(addr: &str) -> Result<String, AddressError> {
    let a = addr.trim();
    let a = a.strip_prefix("mailto:").or_else(|| a.strip_prefix("MAILTO:")).unwrap_or(a);
    if a.is_empty() {
        return Err(AddressError::new(addr, AddressProblem::Empty));
    }
    if a.len() > MAX_ADDRESS_LEN.saturating_mul(4) {
        return Err(AddressError::new(addr, AddressProblem::TooLong));
    }
    let Some((local, domain)) = a.rsplit_once('@') else {
        return Err(AddressError::new(addr, AddressProblem::MissingAt));
    };
    check_local(local).map_err(|p| AddressError::new(addr, p))?;
    let ascii = domain_ascii(domain).ok_or_else(|| AddressError::new(addr, AddressProblem::BadDomain))?;
    if local.len().saturating_add(1).saturating_add(ascii.len()) > MAX_ADDRESS_LEN {
        return Err(AddressError::new(addr, AddressProblem::TooLong));
    }
    let (unicode, res) = idna::domain_to_unicode(&ascii);
    let shown = if res.is_ok() { unicode } else { ascii };
    Ok(format!("{local}@{shown}"))
}

fn check_local(local: &str) -> Result<(), AddressProblem> {
    if local.is_empty() {
        return Err(AddressProblem::BadLocalPart);
    }
    if !local.is_ascii() {
        return Err(AddressProblem::NonAsciiLocalPart);
    }
    if local.len() > MAX_LOCAL_LEN {
        return Err(AddressProblem::TooLong);
    }
    if !local.chars().all(is_atext) || local.starts_with('.') || local.ends_with('.') || local.contains("..") {
        return Err(AddressProblem::BadLocalPart);
    }
    Ok(())
}

/// Dominio en ASCII (IDNA/punycode, minúsculas) si es un nombre de host válido con al menos dos
/// etiquetas.
pub fn domain_ascii(domain: &str) -> Option<String> {
    let d = domain.trim().trim_end_matches('.');
    if d.is_empty() || d.len() > 1_000 {
        return None;
    }
    let ascii = idna::domain_to_ascii(d).ok()?.to_ascii_lowercase();
    if ascii.is_empty() || ascii.len() > 253 {
        return None;
    }
    let mut labels = 0usize;
    for label in ascii.split('.') {
        labels = labels.saturating_add(1);
        let ok = !label.is_empty()
            && label.len() <= 63
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !label.starts_with('-')
            && !label.ends_with('-');
        if !ok {
            return None;
        }
    }
    // Un TLD totalmente numérico delata una IP o basura.
    let tld_numeric = ascii.rsplit('.').next().is_some_and(|t| t.chars().all(|c| c.is_ascii_digit()));
    (labels >= 2 && !tld_numeric).then_some(ascii)
}

/// Dirección lista para el sobre SMTP y las cabeceras: dominio en ASCII.
pub fn ascii_address(addr: &str) -> Result<String, AddressError> {
    let norm = validate_address(addr)?;
    let (local, domain) = norm.rsplit_once('@').ok_or_else(|| AddressError::new(addr, AddressProblem::MissingAt))?;
    let ascii = domain_ascii(domain).ok_or_else(|| AddressError::new(addr, AddressProblem::BadDomain))?;
    Ok(format!("{local}@{ascii}"))
}

/// Clave para comparar direcciones (deduplicar, excluir las propias): ASCII y en minúsculas.
pub fn address_key(addr: &str) -> String {
    ascii_address(addr).unwrap_or_else(|_| addr.trim().to_string()).to_ascii_lowercase()
}

/// Analiza una sola dirección (con nombre visible opcional).
pub fn parse_address(input: &str) -> Result<Mailbox, AddressError> {
    let s = input.trim();
    if s.is_empty() {
        return Err(AddressError::new(input, AddressProblem::Empty));
    }
    let (name, addr) = split_name_addr(s).ok_or_else(|| AddressError::new(input, AddressProblem::Unbalanced))?;
    let addr = validate_address(&addr)?;
    let mut name = name.unwrap_or_default();
    name.retain(|c| !c.is_control());
    let name: String = name.trim().chars().take(MAX_NAME_CHARS).collect();
    // «ana@acme.mx <ana@acme.mx>»: el nombre repetido no aporta.
    if name.eq_ignore_ascii_case(&addr) { Ok(Mailbox::addr(addr)) } else { Ok(Mailbox::new(name, addr)) }
}

/// Separa `Nombre <dir>` en (nombre, dirección). `None` si hay comillas o `<` sin cerrar.
fn split_name_addr(s: &str) -> Option<(Option<String>, String)> {
    let mut in_quote = false;
    let mut escaped = false;
    let mut lt = None;
    for (i, c) in s.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if in_quote => escaped = true,
            '"' => in_quote = !in_quote,
            '<' if !in_quote => {
                lt = Some(i);
                break;
            }
            _ => {}
        }
    }
    if in_quote {
        return None;
    }
    let Some(lt) = lt else {
        return Some((None, s.to_string()));
    };
    let head = s.get(..lt)?;
    let rest = s.get(lt.saturating_add(1)..)?;
    let gt = rest.find('>')?;
    let addr = rest.get(..gt)?.trim().to_string();
    Some((Some(unquote(head.trim())), addr))
}

/// Quita comillas y escapes de un nombre visible.
fn unquote(s: &str) -> String {
    let inner = s.strip_prefix('"').and_then(|x| x.strip_suffix('"')).unwrap_or(s);
    let mut out = String::with_capacity(inner.len());
    let mut escaped = false;
    for c in inner.chars() {
        if escaped {
            out.push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else {
            out.push(c);
        }
    }
    out
}

/// Analiza una lista separada por `,` o `;` (fuera de comillas y de `<…>`). Las entradas vacías
/// se ignoran; la primera dirección mala corta con su error.
pub fn parse_address_list(input: &str) -> Result<Vec<Mailbox>, AddressError> {
    if input.len() > MAX_LIST_INPUT {
        return Err(AddressError::new(input, AddressProblem::TooLong));
    }
    let mut out = Vec::new();
    for item in split_list(input) {
        if item.trim().is_empty() {
            continue;
        }
        if out.len() >= MAX_RECIPIENTS {
            return Err(AddressError::new(input, AddressProblem::TooMany));
        }
        out.push(parse_address(&item)?);
    }
    Ok(out)
}

fn split_list(input: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    let mut in_angle = false;
    let mut escaped = false;
    for c in input.chars() {
        if escaped {
            cur.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' if in_quote => {
                escaped = true;
                cur.push(c);
            }
            '"' if !in_angle => {
                in_quote = !in_quote;
                cur.push(c);
            }
            '<' if !in_quote => {
                in_angle = true;
                cur.push(c);
            }
            '>' if !in_quote => {
                in_angle = false;
                cur.push(c);
            }
            ',' | ';' | '\n' | '\r' if !in_quote && !in_angle => items.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    items.push(cur);
    items
}

/// Texto de un buzón para mostrar o editar: `Nombre <dir>` con comillas si hacen falta.
pub fn format_mailbox(m: &Mailbox) -> String {
    match m.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) if n.chars().any(|c| "\",;<>@()[]:\\.".contains(c)) => {
            format!("\"{}\" <{}>", n.replace('\\', "\\\\").replace('"', "\\\""), m.addr)
        }
        Some(n) => format!("{n} <{}>", m.addr),
        None => m.addr.clone(),
    }
}

/// Quita duplicados (por [`address_key`]) conservando el primero y su orden.
pub fn dedupe(list: &mut Vec<Mailbox>) {
    let mut seen = std::collections::HashSet::new();
    list.retain(|m| seen.insert(address_key(&m.addr)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_and_named() {
        let m = parse_address("ana@acme.mx").unwrap();
        assert_eq!(m, Mailbox::addr("ana@acme.mx"));
        let m = parse_address("Ana López <ana@ACME.mx>").unwrap();
        assert_eq!(m.name.as_deref(), Some("Ana López"));
        assert_eq!(m.addr, "ana@acme.mx");
        let m = parse_address("\"López, Ana\" <ana@acme.mx>").unwrap();
        assert_eq!(m.name.as_deref(), Some("López, Ana"));
        let m = parse_address("mailto:bob@example.com").unwrap();
        assert_eq!(m.addr, "bob@example.com");
    }

    #[test]
    fn idna_domains() {
        let m = parse_address("josé <jose@correo.méxico>").unwrap();
        assert_eq!(m.addr, "jose@correo.méxico");
        assert_eq!(ascii_address(&m.addr).unwrap(), "jose@correo.xn--mxico-bsa");
        assert_eq!(ascii_address("x@correo.xn--mxico-bsa").unwrap(), "x@correo.xn--mxico-bsa");
        assert_eq!(address_key("Ana@Correo.MÉXICO"), "ana@correo.xn--mxico-bsa");
    }

    #[test]
    fn rejects_bad() {
        let p = |s: &str| parse_address(s).unwrap_err().problem;
        assert_eq!(p(""), AddressProblem::Empty);
        assert_eq!(p("ana"), AddressProblem::MissingAt);
        assert_eq!(p("@acme.mx"), AddressProblem::BadLocalPart);
        assert_eq!(p("a..b@acme.mx"), AddressProblem::BadLocalPart);
        assert_eq!(p("a b@acme.mx"), AddressProblem::BadLocalPart);
        assert_eq!(p("josé@acme.mx"), AddressProblem::NonAsciiLocalPart);
        assert_eq!(p("ana@acme"), AddressProblem::BadDomain);
        assert_eq!(p("ana@-acme.mx"), AddressProblem::BadDomain);
        assert_eq!(p("ana@1.2.3.4"), AddressProblem::BadDomain);
        assert_eq!(p("\"Ana <ana@acme.mx>"), AddressProblem::Unbalanced);
        assert_eq!(p("Ana <ana@acme.mx"), AddressProblem::Unbalanced);
        assert!(!AddressError::new("x", AddressProblem::BadDomain).message(Lang::Es).is_empty());
    }

    #[test]
    fn lists() {
        let l = parse_address_list("Ana <ana@acme.mx>, \"Pérez, Juan\" <juan@x.mx>; bob@example.com,, ").unwrap();
        assert_eq!(l.len(), 3);
        assert_eq!(l[1].name.as_deref(), Some("Pérez, Juan"));
        assert!(parse_address_list("ana@acme.mx, malo").is_err());
        let many = vec!["a@b.co"; MAX_RECIPIENTS + 1].join(",");
        assert_eq!(parse_address_list(&many).unwrap_err().problem, AddressProblem::TooMany);
    }

    #[test]
    fn format_and_dedupe() {
        assert_eq!(format_mailbox(&Mailbox::new("López, Ana", "ana@acme.mx")), "\"López, Ana\" <ana@acme.mx>");
        assert_eq!(format_mailbox(&Mailbox::new("Ana", "ana@acme.mx")), "Ana <ana@acme.mx>");
        let mut l = vec![Mailbox::addr("A@acme.mx"), Mailbox::new("Ana", "a@ACME.mx"), Mailbox::addr("b@acme.mx")];
        dedupe(&mut l);
        assert_eq!(l.len(), 2);
    }
}
