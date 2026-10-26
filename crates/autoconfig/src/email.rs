//! Dirección de correo validada (lo mínimo para descubrir servidores).

/// Error de una dirección que no sirve.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EmailError {
    #[error("la dirección está vacía")]
    Empty,
    #[error("falta la arroba (@)")]
    MissingAt,
    #[error("la parte local no es válida")]
    BadLocalPart,
    #[error("el dominio no es válido")]
    BadDomain,
    #[error("los dominios internacionales (con acentos) aún no se admiten; usa su forma xn--")]
    InternationalDomain,
}

/// Correo separado en parte local y dominio (en minúsculas).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailAddress {
    /// Dirección completa: parte local tal cual + `@` + dominio en minúsculas.
    pub address: String,
    pub local: String,
    pub domain: String,
}

impl EmailAddress {
    pub fn parse(input: &str) -> Result<Self, EmailError> {
        let s = input.trim();
        if s.is_empty() {
            return Err(EmailError::Empty);
        }
        let (local, domain) = s.rsplit_once('@').ok_or(EmailError::MissingAt)?;
        if local.is_empty()
            || local.len() > 64
            || local.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | '%' | '/' | '\\'))
        {
            return Err(EmailError::BadLocalPart);
        }
        let domain = domain.trim_end_matches('.').to_lowercase();
        if !domain.is_ascii() {
            return Err(EmailError::InternationalDomain);
        }
        let labels_ok = domain.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        });
        if domain.len() > 253 || !domain.contains('.') || !labels_ok {
            return Err(EmailError::BadDomain);
        }
        Ok(Self { address: format!("{local}@{domain}"), local: local.to_string(), domain })
    }

    /// Sustituye los marcadores de Thunderbird en un texto de configuración.
    pub fn substitute(&self, template: &str) -> String {
        template
            .replace("%EMAILADDRESS%", &self.address)
            .replace("%EMAILLOCALPART%", &self.local)
            .replace("%EMAILDOMAIN%", &self.domain)
    }

    /// La dirección codificada para una query de URL.
    pub fn url_encoded(&self) -> String {
        url_encode(&self.address)
    }
}

/// Codificación porcentual estricta (todo salvo `A-Za-z0-9-._~`).
pub(crate) fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len().saturating_mul(3));
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_normalizes() {
        let e = EmailAddress::parse("  Ana.Perez+x@Example.COM. ").unwrap();
        assert_eq!(e.local, "Ana.Perez+x");
        assert_eq!(e.domain, "example.com");
        assert_eq!(e.address, "Ana.Perez+x@example.com");
        assert_eq!(e.url_encoded(), "Ana.Perez%2Bx%40example.com");
        assert_eq!(e.substitute("%EMAILLOCALPART%/%EMAILDOMAIN%"), "Ana.Perez+x/example.com");
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(EmailAddress::parse(""), Err(EmailError::Empty));
        assert_eq!(EmailAddress::parse("ana"), Err(EmailError::MissingAt));
        assert_eq!(EmailAddress::parse("@x.com"), Err(EmailError::BadLocalPart));
        assert_eq!(EmailAddress::parse("a b@x.com"), Err(EmailError::BadLocalPart));
        assert_eq!(EmailAddress::parse("a@localhost"), Err(EmailError::BadDomain));
        assert_eq!(EmailAddress::parse("a@x..com"), Err(EmailError::BadDomain));
        assert_eq!(EmailAddress::parse("a@-x.com"), Err(EmailError::BadDomain));
        assert_eq!(EmailAddress::parse("a@x.com/../y"), Err(EmailError::BadDomain));
        assert_eq!(EmailAddress::parse("a@años.mx"), Err(EmailError::InternationalDomain));
    }
}
