//! Mensajes grandes por Graph (R4.5, pendiente de F2.5). `sendMail` y la creación de un borrador
//! desde MIME no admiten peticiones de más de ~4 MB, así que un mensaje mayor se manda en partes:
//!
//! 1. se separan del MIME los adjuntos de primer nivel (`multipart/mixed`), del más grande al más
//!    chico, hasta que lo que queda cabe ([`detach_attachments`]); el cuerpo, las cabeceras
//!    (`In-Reply-To`, `References`…) y los adjuntos chicos se quedan tal cual;
//! 2. ese MIME crea un borrador (`POST /me/messages`);
//! 3. cada adjunto separado se sube: los de menos de 3 MB con `POST …/attachments` y los demás
//!    con una sesión de subida (`createUploadSession` + `PUT` por trozos de 3,125 MiB, múltiplo de
//!    320 KiB, a la URL ya firmada, sin token);
//! 4. `POST …/send`. Si algo falla después de crear el borrador, se borra (lo mejor posible).
//!
//! Graph admite adjuntos de hasta 150 MB; uno mayor es «demasiado grande».

use base64::Engine as _;
use mail_parser::{MessageParser, MimeHeaders};
use reqwest::Method;
use serde_json::{Value, json};

use crate::{Body, GraphClient, GraphError, Result};

/// Por encima de esto un adjunto va por sesión de subida (Graph: 3 MB).
pub const UPLOAD_SESSION_FROM: usize = 3 * 1024 * 1024;
/// Tamaño de cada trozo (múltiplo de 320 KiB, como pide Graph).
pub const CHUNK: usize = 10 * 320 * 1024;
/// Tope de Graph para un adjunto.
pub const MAX_ATTACHMENT: usize = 150 * 1024 * 1024;
/// Tope de partes que se miran (mensajes hostiles).
const MAX_PARTS: usize = 500;

/// Un adjunto separado del MIME para subirlo aparte.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detached {
    pub name: String,
    pub content_type: String,
    pub content_id: Option<String>,
    pub inline: bool,
    pub data: Vec<u8>,
}

/// Separa adjuntos de primer nivel de un `multipart/mixed` hasta que el MIME restante mida como
/// mucho `max` bytes. `None` si el mensaje no tiene esa forma o ni aun así cabe.
pub fn detach_attachments(raw: &[u8], max: usize) -> Option<(Vec<u8>, Vec<Detached>)> {
    let (head_end, body_start) = header_end(raw)?;
    let msg = MessageParser::default().parse(raw)?;
    let ct = msg.root_part().content_type()?;
    if !ct.ctype().eq_ignore_ascii_case("multipart") || !ct.subtype().is_some_and(|s| s.eq_ignore_ascii_case("mixed")) {
        return None;
    }
    let boundary = ct.attribute("boundary")?.to_string();
    if boundary.is_empty() || boundary.len() > 200 {
        return None;
    }
    let body = raw.get(body_start..)?;
    let (preamble, parts, epilogue) = split_parts(body, boundary.as_bytes())?;
    // Candidatos: todas las partes menos la primera (el cuerpo), con su adjunto ya decodificado.
    let mut candidates: Vec<(usize, Detached)> = Vec::new();
    for (i, part) in parts.iter().enumerate().skip(1) {
        let Some(p) = MessageParser::default().parse(part) else { continue };
        let root = p.root_part();
        let name = root.attachment_name().map(str::to_string);
        let is_attachment =
            root.content_disposition().is_some_and(|d| d.is_attachment()) || (name.is_some() && !root.is_text());
        if !is_attachment {
            continue;
        }
        let Some(ct) = root.content_type() else { continue };
        let content_type = match ct.subtype() {
            Some(sub) => format!("{}/{}", ct.ctype(), sub),
            None => ct.ctype().to_string(),
        };
        let inline = root.content_disposition().is_some_and(|d| d.is_inline());
        candidates.push((
            i,
            Detached {
                name: name.unwrap_or_else(|| "adjunto".to_string()).chars().take(255).collect(),
                content_type: content_type.chars().take(127).collect(),
                content_id: root.content_id().map(|c| c.trim_matches(['<', '>']).chars().take(255).collect()),
                inline,
                data: root.contents().to_vec(),
            },
        ));
    }
    // Del más grande (en el MIME) al más chico, hasta que quepa.
    candidates.sort_by_key(|(i, _)| std::cmp::Reverse(parts.get(*i).map_or(0, |p| p.len())));
    let delim_len = boundary.len().saturating_add(6);
    let mut size = raw.len();
    let mut out = Vec::new();
    let mut removed = Vec::new();
    for (i, d) in candidates {
        if size <= max {
            break;
        }
        size = size.saturating_sub(parts.get(i).map_or(0, |p| p.len()).saturating_add(delim_len));
        removed.push(i);
        out.push(d);
    }
    if size > max || out.is_empty() {
        return None;
    }
    // Rearmar: cabecera, preámbulo, las partes que quedan y el cierre.
    let mut mime = Vec::with_capacity(size.saturating_add(64));
    mime.extend_from_slice(raw.get(..head_end)?);
    mime.extend_from_slice(b"\r\n\r\n");
    mime.extend_from_slice(preamble);
    for (i, part) in parts.iter().enumerate() {
        if removed.contains(&i) {
            continue;
        }
        mime.extend_from_slice(b"\r\n--");
        mime.extend_from_slice(boundary.as_bytes());
        mime.extend_from_slice(b"\r\n");
        mime.extend_from_slice(part);
    }
    mime.extend_from_slice(b"\r\n--");
    mime.extend_from_slice(boundary.as_bytes());
    mime.extend_from_slice(b"--");
    mime.extend_from_slice(epilogue);
    (mime.len() <= max).then_some((mime, out))
}

/// Fin de la cabecera y comienzo del cuerpo (`\r\n\r\n` o `\n\n`).
fn header_end(raw: &[u8]) -> Option<(usize, usize)> {
    if let Some(p) = find(raw, b"\r\n\r\n", 0) {
        return Some((p, p.saturating_add(4)));
    }
    find(raw, b"\n\n", 0).map(|p| (p, p.saturating_add(2)))
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    let rest = hay.get(from..)?;
    rest.windows(needle.len()).position(|w| w == needle).map(|p| p.saturating_add(from))
}

/// Parte un cuerpo `multipart` en (preámbulo, partes, epílogo). Cada delimitador va al inicio de
/// una línea: `--límite` (o `--límite--` al cierre). Las partes no incluyen el salto de línea que
/// precede al siguiente delimitador.
fn split_parts<'a>(body: &'a [u8], boundary: &[u8]) -> Option<(&'a [u8], Vec<&'a [u8]>, &'a [u8])> {
    let mut delim = b"--".to_vec();
    delim.extend_from_slice(boundary);
    // Posiciones de los delimitadores al inicio de línea.
    let mut marks = Vec::new();
    let mut from = 0usize;
    while let Some(p) = find(body, &delim, from) {
        let at_line_start = p == 0 || body.get(p.saturating_sub(1)) == Some(&b'\n');
        if at_line_start {
            marks.push(p);
            if marks.len() > MAX_PARTS {
                return None;
            }
        }
        from = p.saturating_add(delim.len());
    }
    let first = *marks.first()?;
    let preamble = trim_eol(body.get(..first)?);
    let mut parts = Vec::new();
    let mut epilogue: &[u8] = &[];
    for (k, &m) in marks.iter().enumerate() {
        let after = m.saturating_add(delim.len());
        if body.get(after..after.saturating_add(2)) == Some(b"--") {
            epilogue = body.get(after.saturating_add(2)..)?;
            break;
        }
        // Saltar el resto de la línea del delimitador (espacios y el salto).
        let start = find(body, b"\n", after)?.saturating_add(1);
        let end = marks.get(k.saturating_add(1)).copied()?;
        parts.push(trim_eol(body.get(start..end)?));
    }
    if parts.is_empty() {
        return None;
    }
    Some((preamble, parts, epilogue))
}

/// Quita un salto de línea final (`\r\n` o `\n`): pertenece al delimitador que sigue.
fn trim_eol(s: &[u8]) -> &[u8] {
    s.strip_suffix(b"\r\n").or_else(|| s.strip_suffix(b"\n")).unwrap_or(s)
}

/// Manda un mensaje grande en partes (ver el módulo). `mime` ya lleva `Bcc:` si hace falta.
pub async fn send_in_parts(client: &GraphClient, mime: &[u8], attachments: &[Detached]) -> Result<()> {
    if attachments.iter().any(|a| a.data.len() > MAX_ATTACHMENT) {
        return Err(GraphError::TooLarge);
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(mime);
    let r = client
        .request(
            Method::POST,
            &client.url("/me/messages"),
            Some(Body::Raw { content_type: "text/plain", data: b64.as_bytes() }),
            &[],
        )
        .await?;
    if !r.is_success() {
        return Err(r.error());
    }
    let id = r.body.get("id").and_then(Value::as_str).filter(|s| !s.is_empty() && s.len() < 1024);
    let Some(id) = id.map(str::to_string) else {
        return Err(GraphError::Invalid("borrador sin id".into()));
    };
    let base = client.url(&format!("/me/messages/{}", crate::enc(&id)));
    let result = async {
        for a in attachments {
            upload(client, &base, a).await?;
        }
        let r = client.request(Method::POST, &format!("{base}/send"), None, &[]).await?;
        if !r.is_success() {
            return Err(r.error());
        }
        Ok(())
    }
    .await;
    if result.is_err() {
        // No dejar un borrador a medias en «Borradores».
        let _ = client.request(Method::DELETE, &base, None, &[]).await;
    }
    result
}

async fn upload(client: &GraphClient, message_url: &str, a: &Detached) -> Result<()> {
    if a.data.len() < UPLOAD_SESSION_FROM {
        let mut v = json!({
            "@odata.type": "#microsoft.graph.fileAttachment",
            "name": a.name,
            "contentType": a.content_type,
            "isInline": a.inline,
            "contentBytes": base64::engine::general_purpose::STANDARD.encode(&a.data),
        });
        if let Some(cid) = &a.content_id {
            v["contentId"] = cid.clone().into();
        }
        let r = client.request(Method::POST, &format!("{message_url}/attachments"), Some(Body::Json(&v)), &[]).await?;
        return if r.is_success() { Ok(()) } else { Err(r.error()) };
    }
    let mut item = json!({
        "attachmentType": "file",
        "name": a.name,
        "size": a.data.len(),
        "contentType": a.content_type,
        "isInline": a.inline,
    });
    if let Some(cid) = &a.content_id {
        item["contentId"] = cid.clone().into();
    }
    let req = json!({ "AttachmentItem": item });
    let r = client
        .request(Method::POST, &format!("{message_url}/attachments/createUploadSession"), Some(Body::Json(&req)), &[])
        .await?;
    if !r.is_success() {
        return Err(r.error());
    }
    let Some(upload_url) = r.body.get("uploadUrl").and_then(Value::as_str).map(str::to_string) else {
        return Err(GraphError::Invalid("sesión de subida sin uploadUrl".into()));
    };
    // Cada trozo debe salir bien (Graph: 200 con lo que falta, 201 al terminar).
    let total = a.data.len();
    let mut start = 0usize;
    for chunk in a.data.chunks(CHUNK) {
        client.upload_chunk(&upload_url, start, total, chunk).await?;
        start = start.saturating_add(chunk.len());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn big_mime(sizes: &[usize]) -> Vec<u8> {
        let mut m = b"From: ana@contoso.com\r\nTo: bob@x.com\r\nSubject: Fotos\r\nIn-Reply-To: <a@b>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"XyZ\"\r\n\r\nThis is a multi-part message.\r\n--XyZ\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHola, van las fotos.\r\n".to_vec();
        for (i, n) in sizes.iter().enumerate() {
            let data = vec![b'a' + (i as u8 % 26); *n];
            let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
            m.extend_from_slice(
                format!(
                    "--XyZ\r\nContent-Type: image/jpeg; name=\"f{i}.jpg\"\r\nContent-Disposition: attachment; filename=\"f{i}.jpg\"\r\nContent-Transfer-Encoding: base64\r\n\r\n"
                )
                .as_bytes(),
            );
            for line in b64.as_bytes().chunks(76) {
                m.extend_from_slice(line);
                m.extend_from_slice(b"\r\n");
            }
        }
        m.extend_from_slice(b"--XyZ--\r\n");
        m
    }

    #[test]
    fn detaches_largest_first_until_it_fits() {
        let raw = big_mime(&[10_000, 2_000_000, 500]);
        let (mime, out) = detach_attachments(&raw, 100_000).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "f1.jpg");
        assert_eq!(out[0].content_type, "image/jpeg");
        assert_eq!(out[0].data, vec![b'b'; 2_000_000]);
        assert!(mime.len() <= 100_000, "{}", mime.len());
        // Lo que queda se lee igual: cuerpo, cabeceras y los adjuntos chicos.
        let p = MessageParser::default().parse(&mime).unwrap();
        assert_eq!(p.subject(), Some("Fotos"));
        assert!(p.header_raw("In-Reply-To").is_some());
        assert_eq!(p.body_text(0).as_deref().map(str::trim), Some("Hola, van las fotos."));
        let names: Vec<_> = p.attachments().filter_map(|a| a.attachment_name().map(str::to_string)).collect();
        assert_eq!(names, vec!["f0.jpg".to_string(), "f2.jpg".to_string()]);
        // Si todo cabe con quitar todos, se quitan todos; si ni así, `None`.
        let (_, all) = detach_attachments(&raw, 1_000).unwrap();
        assert_eq!(all.len(), 3);
        assert!(detach_attachments(&raw, 10).is_none());
    }

    #[test]
    fn only_multipart_mixed() {
        assert!(detach_attachments(b"From: a@b\r\nSubject: x\r\n\r\nhola", 1).is_none());
        assert!(detach_attachments(b"", 1).is_none());
        let alt = b"Content-Type: multipart/alternative; boundary=b\r\n\r\n--b\r\n\r\nx\r\n--b--\r\n";
        assert!(detach_attachments(alt, 1).is_none());
    }

    #[test]
    fn hostile_input_never_panics() {
        for raw in [
            &b"Content-Type: multipart/mixed; boundary=b\r\n\r\n--b"[..],
            b"Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\n",
            b"Content-Type: multipart/mixed; boundary=\"\"\r\n\r\n----\r\n",
            b"Content-Type: multipart/mixed; boundary=b\n\n--b\nContent-Disposition: attachment; filename=x\n\nzz\n--b--",
        ] {
            let _ = detach_attachments(raw, 0);
        }
        // LF solos: también se entiende.
        let lf = b"Content-Type: multipart/mixed; boundary=b\n\n--b\nContent-Type: text/plain\n\nhola\n--b\nContent-Type: application/pdf; name=x.pdf\nContent-Disposition: attachment; filename=x.pdf\n\nPDFDATA-PDFDATA-PDFDATA-PDFDATA\n--b--\n";
        let (mime, out) = detach_attachments(lf, 90).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].data, b"PDFDATA-PDFDATA-PDFDATA-PDFDATA");
        assert!(String::from_utf8_lossy(&mime).contains("hola"));
    }
}
