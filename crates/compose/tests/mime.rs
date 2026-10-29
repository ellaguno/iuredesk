//! MIME: salidas «doradas» (normalizando Date, Message-ID y fronteras), ida y vuelta con
//! mail-parser, cabeceras internacionales, Bcc fuera de las cabeceras y saneado del HTML.

use chrono::{FixedOffset, TimeZone};
use iuredesk_compose::draft::{AttachmentSource, Forwarded};
use iuredesk_compose::{
    Attachment, BuildOptions, CalendarMethod, CalendarPart, Draft, FsResolver, Priority, RichBody, SizeAdvice,
    build_message,
};
use iuredesk_model::account::Identity;
use mail_parser::{MessageParser, MimeHeaders};

fn ana() -> Identity {
    Identity {
        name: "Ana López".into(),
        email: "ana@acme.mx".into(),
        signature: None,
        reply_to: Some("Ventas <ventas@acme.mx>".into()),
    }
}

fn opts() -> BuildOptions {
    let date = FixedOffset::west_opt(6 * 3600).unwrap().with_ymd_and_hms(2026, 10, 26, 10, 42, 0).unwrap();
    BuildOptions { date, message_id: Some("<test.1@acme.mx>".into()), max_size: 150 * 1024 * 1024, keep_bcc: false }
}

/// Fronteras → B1, B2… en orden de aparición; quita lo que cambia en cada corrida.
fn normalize(raw: &[u8]) -> String {
    let mut s = String::from_utf8_lossy(raw).into_owned();
    let mut n = 0;
    while let Some(pos) = s.find("boundary=\"") {
        let start = pos + "boundary=\"".len();
        let Some(len) = s[start..].find('"') else { break };
        let b = s[start..start + len].to_string();
        n += 1;
        s = s.replace(&b, &format!("B{n}"));
        s = s.replacen(&format!("boundary=\"B{n}\""), &format!("boundary=\u{1}B{n}\u{1}"), 1);
    }
    s.replace(iuredesk_compose::USER_AGENT, "IureDesk/VERSION").replace('\u{1}', "\"").replace("\r\n", "\n")
}

fn golden(name: &str, actual: &str) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden").join(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() || !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
    }
    let expected = std::fs::read_to_string(&path).unwrap();
    assert_eq!(actual, expected, "difiere de {name} (UPDATE_GOLDEN=1 para regenerar)");
}

fn simple_draft() -> Draft {
    let mut d = Draft::new(ana());
    d.set_to("\"Pérez, Juan\" <juan@cliente.com.mx>, bob@example.com").unwrap();
    d.set_cc("José <jose@correo.méxico>").unwrap();
    d.set_bcc("secreta@acme.mx").unwrap();
    d.subject = "Cotización Q4 — revisión".into();
    d.body =
        RichBody::from_markdown("Hola **Juan**,\n\nAdjunto la *cotización*.\n\n- punto uno\n- punto dos\n\nSaludos");
    d
}

#[test]
fn golden_alternative() {
    let d = simple_draft();
    let built = build_message(&d, &FsResolver, &opts()).unwrap();
    golden("alternative.eml", &normalize(&built.raw));
    assert_eq!(built.message_id, "<test.1@acme.mx>");
    assert_eq!(built.envelope_from, "ana@acme.mx");
    assert_eq!(
        built.recipients,
        vec!["juan@cliente.com.mx", "bob@example.com", "jose@correo.xn--mxico-bsa", "secreta@acme.mx"]
    );
    assert_eq!(built.size_advice, SizeAdvice::Fine);
}

#[test]
fn round_trip_headers_and_bodies() {
    let mut d = simple_draft();
    d.priority = Priority::High;
    d.request_read_receipt = true;
    d.in_reply_to = Some("<orig@x.mx>".into());
    d.references = vec!["<a@x.mx>".into(), "<orig@x.mx>".into()];
    let built = build_message(&d, &FsResolver, &opts()).unwrap();
    let text = String::from_utf8_lossy(&built.raw);
    // Bcc nunca en cabeceras.
    assert!(!text.to_ascii_lowercase().contains("bcc:"));
    assert!(!text.contains("secreta@acme.mx"));
    // Sin palabras codificadas entre comillas.
    assert!(!text.contains("\"=?"));
    assert!(text.contains("User-Agent: IureDesk/"));
    assert!(text.contains("X-Priority: 1 (Highest)"));
    assert!(text.contains("Date: Mon, 26 Oct 2026 10:42:00 -0600"));

    let msg = MessageParser::default().parse(&built.raw).unwrap();
    assert_eq!(msg.subject(), Some("Cotización Q4 — revisión"));
    let from = msg.from().unwrap().first().unwrap();
    assert_eq!(from.name(), Some("Ana López"));
    assert_eq!(from.address(), Some("ana@acme.mx"));
    let to: Vec<_> =
        msg.to().unwrap().iter().map(|a| (a.name().map(str::to_string), a.address().unwrap().to_string())).collect();
    assert_eq!(to[0], (Some("Pérez, Juan".into()), "juan@cliente.com.mx".into()));
    let cc = msg.cc().unwrap().first().unwrap();
    assert_eq!(cc.name(), Some("José"));
    assert_eq!(cc.address(), Some("jose@correo.xn--mxico-bsa"));
    assert_eq!(msg.reply_to().unwrap().first().unwrap().address(), Some("ventas@acme.mx"));
    assert_eq!(msg.message_id(), Some("test.1@acme.mx"));
    assert_eq!(msg.in_reply_to().as_text(), Some("orig@x.mx"));
    assert!(msg.header_raw("Disposition-Notification-To").unwrap().contains("ana@acme.mx"));

    let plain = msg.body_text(0).unwrap();
    assert!(plain.contains("Hola Juan,"));
    assert!(plain.contains("- punto uno"));
    let html = msg.body_html(0).unwrap();
    assert!(html.contains("<strong>Juan</strong>"));
    assert!(html.contains("<em>cotización</em>"));
    assert!(!html.contains("<table"));
}

#[test]
fn attachments_inline_images_and_rfc2231() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Presupuesto año 2027.pdf");
    std::fs::write(&path, b"%PDF-1.4 falso").unwrap();
    let mut d = simple_draft();
    d.attachments.push(Attachment::file(&path));
    d.attachments.push(Attachment::inline_image("logo.png", "image/png", "logo@acme", vec![0x89, b'P', b'N', b'G']));
    d.body.blocks.push(iuredesk_compose::Block::Paragraph {
        content: vec![iuredesk_compose::Inline::Image(iuredesk_compose::Image {
            src: "cid:logo@acme".into(),
            alt: "Acme".into(),
            ..Default::default()
        })],
    });
    let built = build_message(&d, &FsResolver, &opts()).unwrap();
    golden("mixed_related.eml", &normalize(&built.raw));
    let text = String::from_utf8_lossy(&built.raw);
    assert!(text.contains("filename*=utf-8''Presupuesto%20a%C3%B1o%202027.pdf"));
    assert!(text.contains("Content-ID: <logo@acme>"));
    assert!(text.contains("multipart/related"));

    let msg = MessageParser::default().parse(&built.raw).unwrap();
    let names: Vec<_> = msg.attachments().filter_map(|a| a.attachment_name().map(str::to_string)).collect();
    assert!(names.contains(&"Presupuesto año 2027.pdf".to_string()), "{names:?}");
    let pdf = msg.attachments().find(|a| a.attachment_name() == Some("Presupuesto año 2027.pdf")).unwrap();
    assert_eq!(pdf.contents(), b"%PDF-1.4 falso");
    let logo = msg.attachments().find(|a| a.content_id() == Some("logo@acme")).unwrap();
    assert_eq!(logo.contents(), &[0x89, b'P', b'N', b'G']);
}

#[test]
fn calendar_invitation_for_outlook() {
    let mut d = simple_draft();
    let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:1@acme.mx\r\nSUMMARY:Revisión\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    d.calendar = Some(CalendarPart::new(CalendarMethod::Request, ics.as_bytes().to_vec()).unwrap());
    let built = build_message(&d, &FsResolver, &opts()).unwrap();
    golden("calendar.eml", &normalize(&built.raw));
    let text = normalize(&built.raw);
    // text/calendar dentro de multipart/alternative, después del HTML.
    let alt = text.find("multipart/alternative").unwrap();
    let html = text.find("Content-Type: text/html").unwrap();
    let cal = text.find("Content-Type: text/calendar; charset=\"utf-8\"; method=\"REQUEST\"").unwrap();
    assert!(alt < html && html < cal);
    assert!(text.contains("application/ics; name=\"invite.ics\""));
    let msg = MessageParser::default().parse(&built.raw).unwrap();
    let found = msg.parts.iter().any(|p| {
        p.is_content_type("text", "calendar") && p.text_contents().is_some_and(|t| t.contains("UID:1@acme.mx"))
    });
    assert!(found);
}

#[test]
fn plain_only_and_bcc_only() {
    let mut d = Draft::new(ana());
    d.set_bcc("oculto@x.mx").unwrap();
    d.subject = "Solo texto".into();
    d.plain_only = true;
    d.body = RichBody::from_plain("Hola.");
    let built = build_message(&d, &FsResolver, &opts()).unwrap();
    let text = String::from_utf8_lossy(&built.raw);
    assert!(text.contains("To: undisclosed-recipients:;"));
    assert!(text.contains("Content-Type: text/plain; charset=\"utf-8\"; format=\"flowed\""));
    assert!(!text.contains("multipart"));
    assert!(!text.contains("oculto"));
    assert_eq!(built.recipients, vec!["oculto@x.mx"]);
}

#[test]
fn forwarded_as_attachment_is_rfc822() {
    let mut d = simple_draft();
    let original = b"From: a@b.mx\nSubject: Hola\n\ncuerpo con \xc3\xb1\n".to_vec();
    d.forwarded = Some(Forwarded { source: AttachmentSource::Bytes(original), filename: "Hola.eml".into() });
    let built = build_message(&d, &FsResolver, &opts()).unwrap();
    let text = String::from_utf8_lossy(&built.raw);
    assert!(text.contains("Content-Type: message/rfc822"));
    assert!(text.contains("Content-Transfer-Encoding: 8bit"));
    let msg = MessageParser::default().parse(&built.raw).unwrap();
    let inner = msg.attachments().find_map(|a| a.message()).unwrap();
    assert_eq!(inner.subject(), Some("Hola"));
}

#[test]
fn header_injection_is_neutralized() {
    let mut d = simple_draft();
    d.subject = "Hola\r\nBcc: victima@x.mx".into();
    let mut id = ana();
    id.name = "Ana\r\nX-Evil: 1".into();
    d.identity = id;
    let built = build_message(&d, &FsResolver, &opts()).unwrap();
    let text = String::from_utf8_lossy(&built.raw);
    assert!(!text.contains("\r\nBcc:"));
    assert!(!text.contains("\r\nX-Evil:"));
}

/// F7.1: lo que se envía solo lleva `Auto-Submitted` (RFC 3834) y nada más lo lleva.
#[test]
fn auto_submitted_header() {
    let mut d = simple_draft();
    let plain = String::from_utf8_lossy(&build_message(&d, &FsResolver, &opts()).unwrap().raw).into_owned();
    assert!(!plain.contains("Auto-Submitted"));
    d.auto_submitted = Some(iuredesk_compose::AutoSubmitted::AutoReplied);
    let text = String::from_utf8_lossy(&build_message(&d, &FsResolver, &opts()).unwrap().raw).into_owned();
    assert!(text.contains("\r\nAuto-Submitted: auto-replied\r\n"), "{text}");
    assert!(text.contains("\r\nX-Auto-Response-Suppress: All\r\n"));
    d.auto_submitted = Some(iuredesk_compose::AutoSubmitted::AutoGenerated);
    let text = String::from_utf8_lossy(&build_message(&d, &FsResolver, &opts()).unwrap().raw).into_owned();
    assert!(text.contains("\r\nAuto-Submitted: auto-generated\r\n"));
    // Se conserva en la cola (serde) para la bandeja de salida.
    let back: Draft = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
    assert_eq!(back.auto_submitted, d.auto_submitted);
}

/// F7.1: los borradores guardados conservan `Bcc:` para reabrirse; lo enviado, nunca.
#[test]
fn drafts_keep_bcc_only_when_asked() {
    let d = simple_draft();
    let o = BuildOptions { keep_bcc: true, ..opts() };
    let text = String::from_utf8_lossy(&build_message(&d, &FsResolver, &o).unwrap().raw).into_owned();
    assert!(text.contains("\r\nBcc: secreta@acme.mx\r\n"));
    let sent = String::from_utf8_lossy(&build_message(&d, &FsResolver, &opts()).unwrap().raw).into_owned();
    assert!(!sent.contains("secreta@acme.mx"));
}

#[test]
fn composed_html_is_sanitized() {
    let mut d = simple_draft();
    d.body = RichBody::from_html(
        "<p onclick=\"x()\">Hola<script>alert(1)</script><img src=x onerror=alert(2)>\
         <a href=\"javascript:alert(3)\">clic</a><iframe src=\"https://evil\"></iframe>\
         <style>body{}</style><a href=\"https://ok.mx\" onmouseover=\"y()\">ok</a></p>",
    );
    let built = build_message(&d, &FsResolver, &opts()).unwrap();
    let msg = MessageParser::default().parse(&built.raw).unwrap();
    let html = msg.body_html(0).unwrap().to_ascii_lowercase();
    for bad in ["<script", "javascript:", "onerror", "onclick", "onmouseover", "<iframe", "<style", "alert("] {
        assert!(!html.contains(bad), "{bad} en {html}");
    }
    assert!(html.contains("href=\"https://ok.mx\""));
}

#[test]
fn size_limits() {
    let mut d = simple_draft();
    d.attachments.push(Attachment::bytes("grande.bin", "application/octet-stream", vec![7u8; 26 * 1024 * 1024]));
    let built = build_message(&d, &FsResolver, &opts()).unwrap();
    assert!(matches!(built.size_advice, SizeAdvice::SuggestCloudLink { .. }));
    let mut small = opts();
    small.max_size = 1024;
    assert!(matches!(
        build_message(&d, &FsResolver, &small),
        Err(iuredesk_compose::ComposeError::Attachment { .. } | iuredesk_compose::ComposeError::TooLarge { .. })
    ));
}

#[test]
fn missing_attachment_is_a_friendly_error() {
    let mut d = simple_draft();
    d.attachments.push(Attachment::file("/no/existe/informe.pdf"));
    let err = build_message(&d, &FsResolver, &opts()).unwrap_err();
    assert!(err.message(iuredesk_compose::Lang::Es).contains("informe.pdf"));
}

/// F5.4: el sellador recibe los destinatarios del sobre y la petición; sin sellador, pedir
/// firma/cifrado es un error claro (nunca se envía en claro sin avisar).
#[test]
fn sealer_hook() {
    use iuredesk_compose::secure::{MessageSealer, SecurityRequest, build_sealed};
    use std::sync::Arc;
    struct Tag;
    impl MessageSealer for Tag {
        fn seal(&self, raw: &[u8], rcpt: &[String], req: SecurityRequest) -> Result<Vec<u8>, (String, String)> {
            if req.encrypt {
                return Err(("falta clave".into(), "missing key".into()));
            }
            let mut v = format!("X-Sealed: {}\r\n", rcpt.join(",")).into_bytes();
            v.extend_from_slice(raw);
            Ok(v)
        }
    }
    let mut d = simple_draft();
    let s: Arc<dyn MessageSealer> = Arc::new(Tag);
    let b = build_sealed(&d, &FsResolver, &opts(), Some(&s)).unwrap();
    assert!(b.raw.starts_with(b"X-Sealed: "));
    d.security = SecurityRequest { sign: true, encrypt: true };
    let e = build_sealed(&d, &FsResolver, &opts(), Some(&s)).unwrap_err();
    assert_eq!(e.message(iuredesk_compose::Lang::Es), "falta clave");
    assert!(build_sealed(&d, &FsResolver, &opts(), None).is_err());
}
