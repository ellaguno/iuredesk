//! Particularidades conocidas de cada servidor.
//!
//! | Servidor | Qué hacemos |
//! |---|---|
//! | iCloud | Se entra por `caldav.icloud.com` / `contacts.icloud.com`; el principal redirige a `pNN-caldav.icloud.com`. Las redirecciones se siguen a mano conservando método y cuerpo (`PROPFIND`), y la credencial solo viaja dentro de `icloud.com`. Exige contraseña de app. Lista `inbox`/`outbox` de agenda como colecciones: se filtran por `resourcetype`. |
//! | Google | Solo OAuth (Bearer). CalDAV en `https://apidata.googleusercontent.com/caldav/v2/{correo}/user` y CardDAV en `https://www.googleapis.com/carddav/v1/principals/{correo}/lists/default/`; no hay `.well-known` útil en el dominio del usuario. |
//! | Fastmail | `caldav.fastmail.com/dav/` y `carddav.fastmail.com/dav/`; soporta `sync-collection`. |
//! | Nextcloud | `/remote.php/dav/`; el `.well-known` del dominio a veces apunta mal (proxy): se prueba la ruta directa. |
//! | Radicale | Sin `getctag` en versiones viejas: se usa `sync-token` o el diff de ETags. |
//! | Stalwart | `/.well-known/caldav` → `/dav/cal/`; RFC 6578 completo. |
//! | Iurefficient | Aún sin publicar (PLAN.md §13.3): se trata como genérico, probando `/dav/` y `/remote.php/dav/`. |

use reqwest::Url;

/// Servicio a descubrir.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Service {
    /// Calendarios (RFC 4791).
    CalDav,
    /// Libretas de direcciones (RFC 6352).
    CardDav,
}

impl Service {
    /// Ruta `.well-known` (RFC 6764).
    pub fn well_known(self) -> &'static str {
        match self {
            Self::CalDav => "/.well-known/caldav",
            Self::CardDav => "/.well-known/carddav",
        }
    }
}

/// Proveedor reconocido por la URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    /// Apple iCloud.
    ICloud,
    /// Google Calendar / Contacts.
    Google,
    /// Fastmail.
    Fastmail,
    /// Nextcloud / ownCloud.
    Nextcloud,
    /// Radicale.
    Radicale,
    /// Stalwart.
    Stalwart,
    /// Instancia de Iurefficient.
    Iurefficient,
    /// Cualquier otro.
    Generic,
}

/// Reconoce el proveedor por el nombre del servidor y la ruta.
pub fn detect(url: &Url) -> Provider {
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let path = url.path().to_ascii_lowercase();
    if host == "icloud.com" || host.ends_with(".icloud.com") {
        Provider::ICloud
    } else if host.ends_with("googleusercontent.com")
        || host.ends_with("googleapis.com")
        || host.ends_with("google.com")
    {
        Provider::Google
    } else if host.ends_with("fastmail.com") || host.ends_with("messagingengine.com") {
        Provider::Fastmail
    } else if path.contains("/remote.php/") {
        Provider::Nextcloud
    } else if url.port() == Some(5232) {
        Provider::Radicale
    } else if path.starts_with("/dav/cal") || path.starts_with("/dav/card") {
        Provider::Stalwart
    } else if host.contains("iurefficient") {
        Provider::Iurefficient
    } else {
        Provider::Generic
    }
}

/// URLs a probar, en orden, para encontrar el principal a partir de lo que escribió el usuario.
pub fn candidates(start: &Url, service: Service) -> Vec<Url> {
    let mut out: Vec<Url> = Vec::new();
    let mut add = |u: Option<Url>| {
        if let Some(u) = u
            && !out.contains(&u)
        {
            out.push(u);
        }
    };
    let provider = detect(start);
    match (provider, service) {
        (Provider::ICloud, Service::CalDav) => add(Url::parse("https://caldav.icloud.com/").ok()),
        (Provider::ICloud, Service::CardDav) => add(Url::parse("https://contacts.icloud.com/").ok()),
        (Provider::Fastmail, Service::CalDav) => add(Url::parse("https://caldav.fastmail.com/dav/").ok()),
        (Provider::Fastmail, Service::CardDav) => add(Url::parse("https://carddav.fastmail.com/dav/").ok()),
        _ => {}
    }
    if start.path() != "/" && !start.path().is_empty() {
        add(Some(start.clone()));
    }
    add(start.join(service.well_known()).ok());
    match provider {
        Provider::Nextcloud | Provider::Iurefficient | Provider::Generic => {
            add(start.join("/remote.php/dav/").ok());
            if provider == Provider::Iurefficient {
                add(start.join("/dav/").ok());
            }
        }
        _ => {}
    }
    add(start.join("/").ok());
    out
}

/// Principal CalDAV de Google para una cuenta (requiere OAuth con el alcance de Calendar).
pub fn google_caldav_principal(email: &str) -> String {
    format!("https://apidata.googleusercontent.com/caldav/v2/{}/user", email.trim())
}

/// Libreta CardDAV de Google para una cuenta (requiere OAuth con el alcance de Contacts).
pub fn google_carddav_principal(email: &str) -> String {
    format!("https://www.googleapis.com/carddav/v1/principals/{}/lists/default/", email.trim())
}

/// ¿Se puede mandar la credencial de `from` a `to` al seguir una redirección? Solo si no baja de
/// HTTPS a HTTP y el destino es el mismo servidor o comparte el dominio registrable (las dos
/// últimas etiquetas: `p42-caldav.icloud.com` ← `caldav.icloud.com`).
pub fn same_site(from: &Url, to: &Url) -> bool {
    if from.scheme() == "https" && to.scheme() != "https" {
        return false;
    }
    let (Some(a), Some(b)) = (from.host_str(), to.host_str()) else { return false };
    if a.eq_ignore_ascii_case(b) {
        return true;
    }
    let tail = |h: &str| {
        let labels: Vec<&str> = h.rsplit('.').take(2).collect();
        if labels.len() < 2 { None } else { Some(labels.join(".").to_ascii_lowercase()) }
    };
    // IPs: solo el mismo servidor.
    if a.parse::<std::net::IpAddr>().is_ok() || b.parse::<std::net::IpAddr>().is_ok() {
        return false;
    }
    matches!((tail(a), tail(b)), (Some(x), Some(y)) if x == y)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn detects() {
        assert_eq!(detect(&u("https://p42-caldav.icloud.com/123/")), Provider::ICloud);
        assert_eq!(detect(&u("https://apidata.googleusercontent.com/caldav/v2/x/user")), Provider::Google);
        assert_eq!(detect(&u("https://cloud.x.mx/remote.php/dav/")), Provider::Nextcloud);
        assert_eq!(detect(&u("http://localhost:5232/")), Provider::Radicale);
        assert_eq!(detect(&u("https://mail.x.mx/dav/cal/ana/")), Provider::Stalwart);
        assert_eq!(detect(&u("https://acme.iurefficient.com/")), Provider::Iurefficient);
    }

    #[test]
    fn candidate_order() {
        let c = candidates(&u("https://example.com"), Service::CalDav);
        assert_eq!(c[0].as_str(), "https://example.com/.well-known/caldav");
        assert!(c.iter().any(|x| x.path() == "/remote.php/dav/"));
        let c = candidates(&u("https://www.icloud.com/x"), Service::CardDav);
        assert_eq!(c[0].as_str(), "https://contacts.icloud.com/");
    }

    #[test]
    fn redirect_credentials() {
        assert!(same_site(&u("https://caldav.icloud.com/"), &u("https://p42-caldav.icloud.com/1/")));
        assert!(!same_site(&u("https://caldav.icloud.com/"), &u("http://p42-caldav.icloud.com/")));
        assert!(!same_site(&u("https://a.example.com/"), &u("https://evil.com/")));
        assert!(same_site(&u("http://127.0.0.1:8080/"), &u("http://127.0.0.1:8080/x")));
        assert!(!same_site(&u("http://127.0.0.1/"), &u("http://127.0.0.2/")));
    }
}
