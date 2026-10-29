//! Responder / reenviar: matriz de «responder a todos», línea de atribución localizada,
//! encabezados de hilo y reenvío en línea o como adjunto.

use iuredesk_compose::draft::{AttachmentKind, ForwardMode, OriginalKind};
use iuredesk_compose::reply::{OriginalHeaders, original_headers, reply_recipients};
use iuredesk_compose::{
    Block, FsResolver, Lang, ReplyMode, ReplyOptions, RichBody, SignaturePlacement, SubjectStyle, build_message,
    forward, reply,
};
use iuredesk_model::account::Identity;
use iuredesk_model::{Mailbox, MessageId};

fn id(name: &str, email: &str) -> Identity {
    Identity { name: name.into(), email: email.into(), signature: None, reply_to: None }
}

fn me() -> Vec<Identity> {
    vec![id("Yo", "yo@mia.mx"), id("Yo Ventas", "ventas@mia.mx")]
}

fn mbs(list: &[&str]) -> Vec<Mailbox> {
    list.iter().map(|a| Mailbox::addr(*a)).collect()
}

fn addrs(list: &[Mailbox]) -> Vec<&str> {
    list.iter().map(|m| m.addr.as_str()).collect()
}

const ORIGINAL: &str = "From: =?utf-8?Q?Ana_L=C3=B3pez?= <ana@acme.mx>\r\n\
To: yo@mia.mx, Bob <bob@acme.mx>\r\n\
Cc: carla@otra.mx, YO@MIA.MX, ventas@mia.mx\r\n\
Subject: RE: Propuesta Q4\r\n\
Date: Mon, 26 Oct 2026 10:42:00 -0600\r\n\
Message-ID: <orig-2@acme.mx>\r\n\
References: <orig-0@acme.mx> <orig-1@acme.mx>\r\n\
In-Reply-To: <orig-1@acme.mx>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"X\"\r\n\
\r\n\
--X\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<p>Hola, <b>te mando</b> la propuesta.</p><img src=\"cid:firma@acme\"><div class=\"gmail_signature\">Ana</div>\r\n\
--X\r\n\
Content-Type: image/png\r\n\
Content-ID: <firma@acme>\r\n\
Content-Disposition: inline\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
iVBORw==\r\n\
--X\r\n\
Content-Type: application/pdf; name=\"propuesta.pdf\"\r\n\
Content-Disposition: attachment; filename=\"propuesta.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0=\r\n\
--X--\r\n";

#[test]
fn reply_all_matrix() {
    let own = vec!["yo@mia.mx".to_string(), "ventas@mia.mx".to_string()];
    struct Case {
        name: &'static str,
        from: &'static [&'static str],
        reply_to: &'static [&'static str],
        to: &'static [&'static str],
        cc: &'static [&'static str],
        mft: &'static [&'static str],
        mode: ReplyMode,
        want_to: &'static [&'static str],
        want_cc: &'static [&'static str],
    }
    let cases = [
        Case {
            name: "remitente",
            from: &["ana@acme.mx"],
            reply_to: &[],
            to: &["yo@mia.mx", "bob@acme.mx"],
            cc: &["carla@otra.mx"],
            mft: &[],
            mode: ReplyMode::Sender,
            want_to: &["ana@acme.mx"],
            want_cc: &[],
        },
        Case {
            name: "reply-to manda",
            from: &["ana@acme.mx"],
            reply_to: &["lista@acme.mx"],
            to: &["yo@mia.mx"],
            cc: &[],
            mft: &[],
            mode: ReplyMode::Sender,
            want_to: &["lista@acme.mx"],
            want_cc: &[],
        },
        Case {
            name: "todos",
            from: &["ana@acme.mx"],
            reply_to: &[],
            to: &["yo@mia.mx", "bob@acme.mx"],
            cc: &["carla@otra.mx", "Ventas@MIA.mx"],
            mft: &[],
            mode: ReplyMode::All,
            want_to: &["ana@acme.mx", "bob@acme.mx"],
            want_cc: &["carla@otra.mx"],
        },
        Case {
            name: "todos con reply-to",
            from: &["ana@acme.mx"],
            reply_to: &["lista@acme.mx"],
            to: &["yo@mia.mx", "bob@acme.mx"],
            cc: &[],
            mft: &[],
            mode: ReplyMode::All,
            want_to: &["lista@acme.mx", "bob@acme.mx"],
            want_cc: &[],
        },
        Case {
            name: "dedupe y cc ya en para",
            from: &["ana@acme.mx"],
            reply_to: &[],
            to: &["bob@acme.mx", "BOB@acme.mx", "ana@acme.mx"],
            cc: &["bob@acme.mx", "dan@x.mx", "dan@x.mx"],
            mft: &[],
            mode: ReplyMode::All,
            want_to: &["ana@acme.mx", "bob@acme.mx"],
            want_cc: &["dan@x.mx"],
        },
        Case {
            name: "mail-followup-to",
            from: &["ana@acme.mx"],
            reply_to: &[],
            to: &["lista@acme.mx"],
            cc: &["bob@acme.mx"],
            mft: &["lista@acme.mx", "yo@mia.mx"],
            mode: ReplyMode::All,
            want_to: &["lista@acme.mx"],
            want_cc: &[],
        },
        Case {
            name: "propio, a remitente",
            from: &["yo@mia.mx"],
            reply_to: &[],
            to: &["ana@acme.mx"],
            cc: &["bob@acme.mx"],
            mft: &[],
            mode: ReplyMode::Sender,
            want_to: &["ana@acme.mx"],
            want_cc: &[],
        },
        Case {
            name: "propio, a todos",
            from: &["yo@mia.mx"],
            reply_to: &[],
            to: &["ana@acme.mx"],
            cc: &["bob@acme.mx"],
            mft: &[],
            mode: ReplyMode::All,
            want_to: &["ana@acme.mx"],
            want_cc: &["bob@acme.mx"],
        },
        Case {
            name: "solo yo en para",
            from: &["ana@acme.mx"],
            reply_to: &[],
            to: &["yo@mia.mx"],
            cc: &[],
            mft: &[],
            mode: ReplyMode::All,
            want_to: &["ana@acme.mx"],
            want_cc: &[],
        },
        Case {
            name: "nota a mí mismo",
            from: &["yo@mia.mx"],
            reply_to: &[],
            to: &["yo@mia.mx"],
            cc: &[],
            mft: &[],
            mode: ReplyMode::All,
            want_to: &["yo@mia.mx"],
            want_cc: &[],
        },
        Case {
            name: "sin para, cc sube",
            from: &["yo@mia.mx"],
            reply_to: &[],
            to: &["ventas@mia.mx"],
            cc: &["bob@acme.mx", "carla@otra.mx"],
            mft: &[],
            mode: ReplyMode::All,
            want_to: &["bob@acme.mx"],
            want_cc: &["carla@otra.mx"],
        },
    ];
    for c in cases {
        let orig = OriginalHeaders {
            from: mbs(c.from),
            reply_to: mbs(c.reply_to),
            to: mbs(c.to),
            cc: mbs(c.cc),
            mail_followup_to: mbs(c.mft),
            ..Default::default()
        };
        let (to, cc) = reply_recipients(&orig, &own, c.mode);
        assert_eq!(addrs(&to), c.want_to, "{}: para", c.name);
        assert_eq!(addrs(&cc), c.want_cc, "{}: cc", c.name);
    }
}

#[test]
fn headers_from_raw() {
    let raw = "From: ana@acme.mx\r\nTo: yo@mia.mx\r\nMail-Followup-To: lista@acme.mx, =?utf-8?Q?Jos=C3=A9?= <jose@x.mx>\r\nDelivered-To: ventas@mia.mx\r\nSubject: x\r\n\r\nhola\r\n";
    let h = original_headers(raw.as_bytes()).unwrap();
    assert_eq!(addrs(&h.mail_followup_to), vec!["lista@acme.mx", "jose@x.mx"]);
    assert_eq!(h.mail_followup_to[1].name.as_deref(), Some("José"));
    assert_eq!(addrs(&h.delivered_to), vec!["ventas@mia.mx"]);
    assert!(original_headers(b"").is_err() || original_headers(b"").unwrap().from.is_empty());
}

#[test]
fn reply_all_from_raw_es() {
    let opts = ReplyOptions {
        lang: Lang::Es,
        subject_style: SubjectStyle::Outlook,
        signature: Some(RichBody::from_plain("Yo\nMía S.A.")),
        original_id: Some(MessageId(42)),
        ..Default::default()
    };
    let d = reply(ORIGINAL.as_bytes(), &me(), ReplyMode::All, &opts).unwrap();
    assert_eq!(d.identity.email, "yo@mia.mx");
    assert_eq!(d.subject, "RE: Propuesta Q4");
    assert_eq!(addrs(&d.to), vec!["ana@acme.mx", "bob@acme.mx"]);
    assert_eq!(addrs(&d.cc), vec!["carla@otra.mx"]);
    assert_eq!(d.in_reply_to.as_deref(), Some("<orig-2@acme.mx>"));
    assert_eq!(d.references, vec!["<orig-0@acme.mx>", "<orig-1@acme.mx>", "<orig-2@acme.mx>"]);
    assert_eq!(d.original.as_ref().map(|o| (o.message, o.kind)), Some((Some(MessageId(42)), OriginalKind::Reply)));
    // Estructura: vacío, firma, atribución, cita.
    assert!(matches!(&d.body.blocks[0], Block::Paragraph { content } if content.is_empty()));
    assert!(matches!(&d.body.blocks[1], Block::Signature { .. }));
    let attribution = RichBody { blocks: vec![d.body.blocks[2].clone()] }.text_content();
    assert_eq!(attribution, "El lun, 26 oct 2026 a las 10:42, Ana López <ana@acme.mx> escribió:");
    let Block::Quote { blocks } = &d.body.blocks[3] else { panic!("{:?}", d.body.blocks) };
    // La firma del original se volvió texto normal dentro de la cita.
    assert!(!blocks.iter().any(|b| matches!(b, Block::Signature { .. })));
    // Solo hay una firma: la nuestra.
    assert_eq!(d.body.signature().map(|s| s.len()), Some(1));

    let plain = d.body.to_plain();
    assert!(plain.contains("\n-- \nYo\nMía S.A.\n"), "{plain}");
    assert!(plain.contains("> Hola, te mando la propuesta."), "{plain}");

    // Y se puede construir y releer.
    let built = build_message(&d, &FsResolver, &iuredesk_compose::BuildOptions::now()).unwrap();
    let msg = mail_parser::MessageParser::default().parse(&built.raw).unwrap();
    assert_eq!(msg.in_reply_to().as_text(), Some("orig-2@acme.mx"));
    assert_eq!(msg.subject(), Some("RE: Propuesta Q4"));
}

#[test]
fn reply_en_below_quote_and_identity_choice() {
    let raw =
        ORIGINAL.replace("To: yo@mia.mx, Bob", "To: ventas@mia.mx, Bob").replace("YO@MIA.MX, ventas@mia.mx", "x@y.mx");
    let opts = ReplyOptions {
        lang: Lang::En,
        signature: Some(RichBody::from_plain("Sales")),
        placement: SignaturePlacement::BelowQuote,
        ..Default::default()
    };
    let d = reply(raw.as_bytes(), &me(), ReplyMode::Sender, &opts).unwrap();
    assert_eq!(d.identity.email, "ventas@mia.mx");
    assert_eq!(d.subject, "Re: Propuesta Q4");
    assert_eq!(addrs(&d.to), vec!["ana@acme.mx"]);
    assert!(d.cc.is_empty());
    let attribution = RichBody { blocks: vec![d.body.blocks[1].clone()] }.text_content();
    assert_eq!(attribution, "On Mon, Oct 26, 2026 at 10:42 AM, Ana López <ana@acme.mx> wrote:");
    assert!(matches!(d.body.blocks.last(), Some(Block::Signature { .. })));
}

#[test]
fn forward_inline_copies_attachments() {
    let opts = ReplyOptions { lang: Lang::Es, original_id: Some(MessageId(7)), ..Default::default() };
    let d = forward(ORIGINAL.as_bytes(), &me(), ForwardMode::Inline, &opts).unwrap();
    assert_eq!(d.subject, "Fwd: Propuesta Q4");
    assert!(d.to.is_empty());
    let header = RichBody { blocks: vec![d.body.blocks[1].clone()] }.text_content();
    assert!(header.starts_with("---------- Mensaje reenviado ---------\nDe: Ana López <ana@acme.mx>\nFecha: lun, 26 oct 2026 a las 10:42\nAsunto: RE: Propuesta Q4\nPara: yo@mia.mx, Bob <bob@acme.mx>"), "{header}");
    assert_eq!(d.attachments.len(), 2);
    let pdf = d.attachments.iter().find(|a| a.filename == "propuesta.pdf").unwrap();
    assert_eq!(pdf.kind, AttachmentKind::Regular);
    let img = d
        .attachments
        .iter()
        .find(|a| matches!(&a.kind, AttachmentKind::Inline { content_id } if content_id == "firma@acme"));
    assert!(img.is_some(), "{:?}", d.attachments);
    assert_eq!(d.original.as_ref().map(|o| o.kind), Some(OriginalKind::Forward));
    // Con destinatario se construye con la imagen en multipart/related.
    let mut d = d;
    d.set_to("zoe@x.mx").unwrap();
    let built = build_message(&d, &FsResolver, &iuredesk_compose::BuildOptions::now()).unwrap();
    let text = String::from_utf8_lossy(&built.raw);
    assert!(text.contains("multipart/related"));
    assert!(text.contains("Content-ID: <firma@acme>"));
}

#[test]
fn forward_as_attachment() {
    let opts = ReplyOptions { lang: Lang::En, subject_style: SubjectStyle::Outlook, ..Default::default() };
    let mut d = forward(ORIGINAL.as_bytes(), &me(), ForwardMode::AsAttachment, &opts).unwrap();
    assert_eq!(d.subject, "FW: Propuesta Q4");
    assert_eq!(d.forwarded.as_ref().map(|f| f.filename.as_str()), Some("Propuesta Q4.eml"));
    d.set_to("zoe@x.mx").unwrap();
    let built = build_message(&d, &FsResolver, &iuredesk_compose::BuildOptions::now()).unwrap();
    let msg = mail_parser::MessageParser::default().parse(&built.raw).unwrap();
    use mail_parser::MimeHeaders;
    let inner = msg.attachments().find_map(|a| a.message()).unwrap();
    assert_eq!(inner.message_id(), Some("orig-2@acme.mx"));
    assert!(msg.attachments().any(|a| a.attachment_name() == Some("Propuesta Q4.eml")));
}

#[test]
fn garbage_original_does_not_panic() {
    for raw in [&b""[..], b"\xff\xfe\x00", b"From:\r\n\r\n", b"Subject: =?utf-8?B?????\r\n\r\n"] {
        let _ = reply(raw, &me(), ReplyMode::All, &ReplyOptions::default());
        let _ = forward(raw, &me(), ForwardMode::Inline, &ReplyOptions::default());
    }
    assert!(reply(ORIGINAL.as_bytes(), &[], ReplyMode::All, &ReplyOptions::default()).is_err());
}

/// F7.1: un borrador guardado se reabre con todo: identidad del «De:», Para/CC/CCO, asunto,
/// cuerpo enriquecido, adjuntos, `In-Reply-To`/`References`, prioridad y acuse.
#[test]
fn saved_draft_reopens_with_everything() {
    let mut d = iuredesk_compose::Draft::new(id("Yo Ventas", "ventas@mia.mx"));
    d.set_to("Ana <ana@acme.mx>").unwrap();
    d.set_cc("carla@otra.mx").unwrap();
    d.set_bcc("oculto@mia.mx").unwrap();
    d.subject = "Re: Propuesta Q4 — ajustes".into();
    d.body = RichBody::from_markdown("Hola **Ana**,\n\n- uno\n- dos\n\nSaludos");
    d.in_reply_to = Some("<orig-2@acme.mx>".into());
    d.references = vec!["<orig-1@acme.mx>".into(), "<orig-2@acme.mx>".into()];
    d.priority = iuredesk_compose::Priority::High;
    d.request_read_receipt = true;
    d.attachments.push(iuredesk_compose::Attachment::bytes(
        "cotizacion.pdf",
        "application/pdf",
        b"%PDF-1.4 x".to_vec(),
    ));
    let opts = iuredesk_compose::BuildOptions { keep_bcc: true, ..iuredesk_compose::BuildOptions::now() };
    let built = build_message(&d, &FsResolver, &opts).unwrap();

    let back = iuredesk_compose::reopen_draft(&built.raw, &me()).unwrap();
    assert_eq!(back.identity.email, "ventas@mia.mx");
    assert_eq!(addrs(&back.to), vec!["ana@acme.mx"]);
    assert_eq!(addrs(&back.cc), vec!["carla@otra.mx"]);
    assert_eq!(addrs(&back.bcc), vec!["oculto@mia.mx"]);
    assert_eq!(back.subject, "Re: Propuesta Q4 — ajustes");
    assert_eq!(back.in_reply_to.as_deref(), Some("<orig-2@acme.mx>"));
    assert_eq!(back.references, vec!["<orig-1@acme.mx>".to_string(), "<orig-2@acme.mx>".to_string()]);
    assert_eq!(back.priority, iuredesk_compose::Priority::High);
    assert!(back.request_read_receipt);
    assert_eq!(back.attachments.len(), 1);
    assert_eq!(back.attachments[0].filename, "cotizacion.pdf");
    let md = back.body.to_markdown();
    assert!(md.contains("**Ana**"), "{md}");
    assert!(md.contains("- uno"), "{md}");

    // Identidad desconocida: la primera; basura: error, no pánico.
    let other = iuredesk_compose::reopen_draft(&built.raw, &[id("Otra", "otra@x.mx")]).unwrap();
    assert_eq!(other.identity.email, "otra@x.mx");
    let _ = iuredesk_compose::reopen_draft(b"\x00\xff garbage", &me());
    assert!(iuredesk_compose::reopen_draft(&built.raw, &[]).is_err());
}
