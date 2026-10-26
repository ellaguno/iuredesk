//! Registros SRV: RFC 6186 (correo) y RFC 6764 (CalDAV/CardDAV).

use crate::net::SrvRecord;
use crate::xml::valid_host;

/// Elige el destino de una lista SRV: menor prioridad y, a igualdad, mayor peso (sin sorteo: el
/// asistente solo propone). `None` si el dominio declara el servicio como no disponible (`.`) o si
/// ningún destino es válido.
pub fn pick(records: &[SrvRecord]) -> Option<(String, u16)> {
    records
        .iter()
        .filter(|r| r.port != 0 && !r.target.is_empty() && r.target != "." && valid_host(&r.target))
        .min_by(|a, b| a.priority.cmp(&b.priority).then(b.weight.cmp(&a.weight)))
        .map(|r| (r.target.clone(), r.port))
}

/// Ruta de contexto de un TXT de RFC 6764 (`path=/dav/`). Solo rutas absolutas y sanas.
pub fn txt_path(txts: &[String]) -> Option<String> {
    txts.iter().find_map(|t| {
        let p = t.trim().strip_prefix("path=")?.trim();
        (p.starts_with('/') && p.len() <= 256 && !p.contains("..") && p.chars().all(|c| c.is_ascii_graphic()))
            .then(|| p.to_string())
    })
}

/// URL `https` de un servicio DAV encontrado por SRV.
pub fn dav_url(host: &str, port: u16, path: Option<&str>) -> String {
    let path = path.unwrap_or("/");
    if port == 443 { format!("https://{host}{path}") } else { format!("https://{host}:{port}{path}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(priority: u16, weight: u16, port: u16, target: &str) -> SrvRecord {
        SrvRecord { priority, weight, port, target: target.into() }
    }

    #[test]
    fn picks_lowest_priority_then_heaviest() {
        let r = [rec(10, 0, 993, "b.example.com"), rec(0, 1, 993, "a.example.com"), rec(0, 5, 993, "c.example.com")];
        assert_eq!(pick(&r), Some(("c.example.com".into(), 993)));
    }

    #[test]
    fn dot_means_not_offered() {
        assert_eq!(pick(&[rec(0, 0, 0, ".")]), None);
        assert_eq!(pick(&[rec(0, 0, 993, "")]), None);
        assert_eq!(pick(&[rec(0, 0, 993, "bad host")]), None);
        assert_eq!(pick(&[]), None);
    }

    #[test]
    fn txt_paths() {
        assert_eq!(txt_path(&["path=/dav/cal/".into()]), Some("/dav/cal/".into()));
        assert_eq!(txt_path(&["v=spf1".into(), "path=relative".into()]), None);
        assert_eq!(txt_path(&["path=/../etc".into()]), None);
        assert_eq!(dav_url("dav.example.com", 443, Some("/x/")), "https://dav.example.com/x/");
        assert_eq!(dav_url("dav.example.com", 8443, None), "https://dav.example.com:8443/");
    }
}
