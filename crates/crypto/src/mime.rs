//! Utilidades MIME a nivel de bytes. Firmar y verificar exige trabajar sobre los bytes exactos de
//! cada parte (RFC 3156 §5, RFC 8551 §3.1.1), así que aquí no se decodifica nada: se separan
//! cabeceras y cuerpo, se leen `Content-Type` y se cortan las partes de un `multipart`.
//!
//! El correo es hostil: todo acotado, sin índices directos y sin pánicos.

use rand::RngCore;

/// Máximo de partes que se miran en un `multipart`.
const MAX_PARTS: usize = 256;
/// Máximo de campos de cabecera que se leen.
const MAX_FIELDS: usize = 1000;

/// Pasa los finales de línea a CRLF (forma canónica de MIME). Los `\r` sueltos se dejan.
pub fn to_crlf(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len().saturating_add(raw.len() / 32));
    let mut prev = 0u8;
    for &b in raw {
        if b == b'\n' && prev != b'\r' {
            out.push(b'\r');
        }
        out.push(b);
        prev = b;
    }
    out
}

/// Posición de `needle` en `hay` a partir de `from`.
pub fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    hay.get(from..)?.windows(needle.len()).position(|w| w == needle).map(|p| p.saturating_add(from))
}

/// Separa cabecera y cuerpo de una entidad en CRLF. Sin línea en blanco, todo es cabecera.
pub fn split_head_body(raw: &[u8]) -> (&[u8], &[u8]) {
    if raw.starts_with(b"\r\n") {
        return (&[], raw.get(2..).unwrap_or(&[]));
    }
    match find(raw, b"\r\n\r\n", 0) {
        Some(i) => (raw.get(..i.saturating_add(2)).unwrap_or(raw), raw.get(i.saturating_add(4)..).unwrap_or(&[])),
        None => (raw, &[]),
    }
}

/// Un campo de cabecera: nombre, valor desplegado y los bytes originales (con el CRLF final).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub value: String,
    pub raw: Vec<u8>,
}

impl Field {
    pub fn is(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
    }
    /// ¿Es una cabecera de contenido (`Content-*` o `MIME-Version`)?
    pub fn is_content(&self) -> bool {
        let n = self.name.to_ascii_lowercase();
        n.starts_with("content-") || n == "mime-version"
    }
}

/// Lee los campos de una cabecera (CRLF), desplegando las continuaciones.
pub fn parse_fields(head: &[u8]) -> Vec<Field> {
    let mut out: Vec<Field> = Vec::new();
    let mut start = 0usize;
    while start < head.len() && out.len() < MAX_FIELDS {
        let end = find(head, b"\r\n", start).map_or(head.len(), |i| i.saturating_add(2));
        let line = head.get(start..end).unwrap_or(&[]);
        let continuation = matches!(line.first(), Some(b' ' | b'\t'));
        if continuation {
            if let Some(last) = out.last_mut() {
                last.raw.extend_from_slice(line);
                let text = String::from_utf8_lossy(line);
                last.value.push(' ');
                last.value.push_str(text.trim());
            }
        } else if let Some(colon) = line.iter().position(|&b| b == b':') {
            let name = String::from_utf8_lossy(line.get(..colon).unwrap_or(&[])).trim().to_string();
            let value = String::from_utf8_lossy(line.get(colon.saturating_add(1)..).unwrap_or(&[])).trim().to_string();
            if !name.is_empty() {
                out.push(Field { name, value, raw: line.to_vec() });
            }
        }
        if end <= start {
            break;
        }
        start = end;
    }
    for f in &mut out {
        if !f.raw.ends_with(b"\r\n") {
            f.raw.extend_from_slice(b"\r\n");
        }
    }
    out
}

/// Valor del primer campo `name`.
pub fn header<'a>(fields: &'a [Field], name: &str) -> Option<&'a str> {
    fields.iter().find(|f| f.is(name)).map(|f| f.value.as_str())
}

/// `Content-Type` interpretado: tipo en minúsculas y parámetros (nombres en minúsculas).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContentType {
    pub mime: String,
    pub params: Vec<(String, String)>,
}

impl ContentType {
    pub fn parse(value: &str) -> Self {
        let mut parts = split_params(value).into_iter();
        let mime = parts.next().unwrap_or_default().trim().to_ascii_lowercase();
        let params = parts
            .filter_map(|p| {
                let (k, v) = p.split_once('=')?;
                let v = v.trim();
                let v = v.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(v);
                Some((k.trim().to_ascii_lowercase(), v.replace("\\\"", "\"")))
            })
            .collect();
        ContentType { mime, params }
    }

    pub fn param(&self, name: &str) -> Option<&str> {
        self.params.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    /// `Content-Type` de una entidad; `text/plain` si no lo declara (RFC 2045 §5.2).
    pub fn of(fields: &[Field]) -> Self {
        header(fields, "content-type").map(ContentType::parse).unwrap_or_else(|| ContentType {
            mime: "text/plain".into(),
            params: vec![("charset".into(), "us-ascii".into())],
        })
    }
}

/// Parte por `;` respetando comillas.
fn split_params(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for c in value.chars() {
        if escaped {
            cur.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' if quoted => {
                cur.push(c);
                escaped = true;
            }
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            ';' if !quoted => {
                out.push(std::mem::take(&mut cur));
                if out.len() > 64 {
                    return out;
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// Cuerpos de las partes de un `multipart` (bytes exactos, sin el CRLF que precede a cada
/// delimitador). `None` si no hay delimitador de cierre ni partes.
pub fn split_multipart<'a>(body: &'a [u8], boundary: &str) -> Option<Vec<&'a [u8]>> {
    if boundary.is_empty() || boundary.len() > 200 {
        return None;
    }
    let delim = format!("--{boundary}");
    let delim = delim.as_bytes();
    // Primer delimitador: al principio del cuerpo o tras un CRLF.
    let mut pos = if body.starts_with(delim) {
        0
    } else {
        let crlf_delim = [b"\r\n".as_slice(), delim].concat();
        find(body, &crlf_delim, 0)?.saturating_add(2)
    };
    let crlf_delim = [b"\r\n".as_slice(), delim].concat();
    let mut parts = Vec::new();
    loop {
        // `pos` apunta al delimitador. ¿Cierre?
        let after = pos.saturating_add(delim.len());
        if body.get(after..after.saturating_add(2)) == Some(b"--".as_slice()) {
            break;
        }
        // El contenido empieza tras el fin de línea del delimitador.
        let line_end = find(body, b"\r\n", after)?;
        let start = line_end.saturating_add(2);
        let Some(next) = find(body, &crlf_delim, start.saturating_sub(2)) else {
            // Sin cierre: la última parte llega hasta el final (mensaje truncado).
            parts.push(body.get(start..).unwrap_or(&[]));
            break;
        };
        let next = next.max(start.saturating_sub(2));
        parts.push(body.get(start..next.max(start)).unwrap_or(&[]));
        pos = next.saturating_add(2);
        if parts.len() >= MAX_PARTS {
            break;
        }
    }
    if parts.is_empty() { None } else { Some(parts) }
}

/// Delimitador nuevo y aleatorio.
pub fn new_boundary(tag: &str) -> String {
    let mut b = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut b);
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("=_iuredesk_{tag}_{hex}")
}

/// Un mensaje separado en cabeceras externas (`From`, `To`, `Subject`…) y la entidad de contenido
/// (cabeceras `Content-*` + línea en blanco + cuerpo), que es lo que se firma o se cifra.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Split {
    pub outer: Vec<Field>,
    pub entity: Vec<u8>,
}

/// Separa un mensaje (en CRLF).
pub fn split_message(raw: &[u8]) -> Split {
    let (head, body) = split_head_body(raw);
    let fields = parse_fields(head);
    let mut entity = Vec::with_capacity(body.len().saturating_add(256));
    let mut outer = Vec::new();
    let mut has_ct = false;
    for f in fields {
        if f.is("mime-version") {
            continue;
        }
        if f.is_content() {
            has_ct |= f.is("content-type");
            entity.extend_from_slice(&f.raw);
        } else {
            outer.push(f);
        }
    }
    if !has_ct {
        entity.extend_from_slice(b"Content-Type: text/plain; charset=us-ascii\r\n");
    }
    entity.extend_from_slice(b"\r\n");
    entity.extend_from_slice(body);
    Split { outer, entity }
}

/// Reconstruye un mensaje: cabeceras externas + `MIME-Version` + la entidad dada.
pub fn join_message(outer: &[Field], entity: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(entity.len().saturating_add(2048));
    for f in outer {
        out.extend_from_slice(&f.raw);
    }
    out.extend_from_slice(b"MIME-Version: 1.0\r\n");
    out.extend_from_slice(entity);
    out
}

/// Agrega un campo de cabecera externo (antes de reconstruir).
pub fn field(name: &str, value: &str) -> Field {
    Field { name: name.to_string(), value: value.to_string(), raw: format!("{name}: {value}\r\n").into_bytes() }
}

/// Parte en líneas de 76 columnas (base64 para MIME).
pub fn wrap76(s: &str) -> String {
    let mut out = String::with_capacity(s.len().saturating_add(s.len() / 76 * 2 + 2));
    let bytes = s.as_bytes();
    for chunk in bytes.chunks(76) {
        out.push_str(&String::from_utf8_lossy(chunk));
        out.push_str("\r\n");
    }
    out
}

/// Direcciones (`addr-spec`, en minúsculas) de una cabecera de direcciones.
pub fn addresses(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    // Lo que va entre <> manda; si no hay, cada elemento separado por comas.
    let mut rest = value;
    let mut found = false;
    while let Some(open) = rest.find('<') {
        let Some(close) = rest.get(open..).and_then(|r| r.find('>')) else { break };
        if let Some(a) = rest.get(open.saturating_add(1)..open.saturating_add(close)) {
            let a = a.trim().to_ascii_lowercase();
            if a.contains('@') {
                out.push(a);
                found = true;
            }
        }
        rest = rest.get(open.saturating_add(close).saturating_add(1)..).unwrap_or("");
        if out.len() > 500 {
            break;
        }
    }
    if !found {
        for p in value.split(',').take(500) {
            let a = p.trim().trim_matches('"').to_ascii_lowercase();
            if a.contains('@') && !a.contains(' ') {
                out.push(a);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_and_split() {
        assert_eq!(to_crlf(b"a\nb\r\nc"), b"a\r\nb\r\nc");
        let (h, b) = split_head_body(b"A: 1\r\nB: 2\r\n\r\nbody");
        assert_eq!(h, b"A: 1\r\nB: 2\r\n");
        assert_eq!(b, b"body");
        let f = parse_fields(b"Subject: hola\r\n mundo\r\nX: y\r\n");
        assert_eq!(f.len(), 2);
        assert_eq!(f.first().map(|f| f.value.as_str()), Some("hola mundo"));
    }

    #[test]
    fn content_type_params() {
        let ct = ContentType::parse(
            "multipart/signed; micalg=pgp-sha256;\r\n protocol=\"application/pgp-signature\"; boundary=\"a;b\"",
        );
        assert_eq!(ct.mime, "multipart/signed");
        assert_eq!(ct.param("protocol"), Some("application/pgp-signature"));
        assert_eq!(ct.param("boundary"), Some("a;b"));
    }

    #[test]
    fn multipart_parts_are_exact() {
        let body = b"preamble\r\n--B\r\nContent-Type: text/plain\r\n\r\nuno\r\n--B\r\n\r\ndos\r\n--B--\r\n";
        let parts = split_multipart(body, "B").unwrap();
        assert_eq!(parts, vec![b"Content-Type: text/plain\r\n\r\nuno".as_slice(), b"\r\ndos".as_slice()]);
        assert!(split_multipart(b"nada", "B").is_none());
    }

    #[test]
    fn address_lists() {
        assert_eq!(addresses("Ana <ANA@x.com>, b@y.org"), vec!["ana@x.com"]);
        assert_eq!(addresses("b@y.org, c@z.org"), vec!["b@y.org", "c@z.org"]);
    }

    proptest::proptest! {
        #[test]
        fn never_panics(data in proptest::collection::vec(proptest::num::u8::ANY, 0..2000), b in "[a-zA-Z0-9]{0,10}") {
            let crlf = to_crlf(&data);
            let (h, body) = split_head_body(&crlf);
            let _ = parse_fields(h);
            let _ = split_multipart(body, &b);
            let _ = split_message(&crlf);
            let _ = ContentType::parse(&String::from_utf8_lossy(&data));
            let _ = addresses(&String::from_utf8_lossy(&data));
        }
    }
}
