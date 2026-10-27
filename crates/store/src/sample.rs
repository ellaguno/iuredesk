//! Buzón de demostración (`iuredesk --sample`) y generador de mensajes sintéticos.
//!
//! Determinista: con la misma semilla y la misma fecha de referencia produce exactamente los mismos
//! datos. El generador pseudoaleatorio es SplitMix64 propio (sin dependencias). Los dominios son
//! `example.com/.org/.net` (RFC 2606): nada apunta a personas o empresas reales.

use chrono::{DateTime, Datelike, Duration, FixedOffset, NaiveTime, Offset, TimeZone, Utc};
use iuredesk_model::{
    AccountId, AddressBook, AttachmentMeta, Attendee, AttendeeRole, Calendar, Contact, Event, EventStatus, EventTime,
    Flags, Folder, FolderId, FolderRole, Labeled, Mailbox, MessageEnvelope, Note, PartStat, PostalAddress, Reminder,
    Task, TaskList, TaskStatus, Transparency, make_preview,
};

use crate::Store;
use crate::error::Result;
use crate::mail::{AccountLabel, NewMessage};

/// Semilla por defecto del buzón de ejemplo.
pub const DEFAULT_SEED: u64 = 0x1u64 << 40 | 0x2026;

/// Generador SplitMix64: rápido, determinista y sin dependencias.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    /// Crea el generador con una semilla.
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Siguiente número de 64 bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Entero en `[0, n)` (0 si `n == 0`).
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next_u64() % n }
    }

    /// `true` con probabilidad `pct` %.
    pub fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }

    /// Elemento al azar (o `None` si está vacío).
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        let i = usize::try_from(self.below(items.len() as u64)).unwrap_or(0);
        items.get(i)
    }

    fn pick_str(&mut self, items: &[&'static str]) -> &'static str {
        self.pick(items).copied().unwrap_or("")
    }
}

/// Resumen de lo generado.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SampleSummary {
    /// Cuentas creadas.
    pub accounts: Vec<AccountId>,
    /// Carpetas.
    pub folders: usize,
    /// Mensajes.
    pub messages: usize,
    /// Eventos.
    pub events: usize,
    /// Contactos.
    pub contacts: usize,
    /// Notas.
    pub notes: usize,
    /// Tareas.
    pub tasks: usize,
}

const FIRST: &[&str] = &[
    "Ana", "Luis", "Marta", "Javier", "Sofía", "Diego", "Lucía", "Carlos", "Elena", "Pablo", "Carmen", "Andrés",
    "Valeria", "Miguel", "Paula", "Jorge", "Isabel", "Raúl", "Daniela", "Tomás", "Emily", "James", "Olivia", "Noah",
    "Grace",
];
const LAST: &[&str] = &[
    "García",
    "Martínez",
    "López",
    "Hernández",
    "González",
    "Pérez",
    "Sánchez",
    "Ramírez",
    "Torres",
    "Flores",
    "Rivera",
    "Gómez",
    "Díaz",
    "Morales",
    "Ortiz",
    "Castro",
    "Smith",
    "Johnson",
    "Brown",
    "Taylor",
];
const ORGS: &[&str] = &[
    "Despacho Rivera",
    "Constructora Altamira",
    "Grupo Ferro",
    "Clínica del Valle",
    "Northwind Ltd.",
    "Acme Corp.",
    "Universidad del Norte",
    "Banco Central (demo)",
    "Fundación Aurora",
    "Contoso Legal",
];
const TITLES: &[&str] = &[
    "Abogada",
    "Gerente de proyecto",
    "Director financiero",
    "Contadora",
    "Paralegal",
    "CEO",
    "Account manager",
    "Consultor",
];
const CITIES: &[(&str, &str)] = &[
    ("Ciudad de México", "México"),
    ("Monterrey", "México"),
    ("Madrid", "España"),
    ("Bogotá", "Colombia"),
    ("Buenos Aires", "Argentina"),
    ("Austin", "United States"),
];

const SUBJ_ES: &[&str] = &[
    "Revisión del contrato de {t}",
    "Propuesta para {t}",
    "Reunión de seguimiento: {t}",
    "Factura pendiente — {t}",
    "Documentos para el expediente {t}",
    "Dudas sobre {t}",
    "Agenda de la semana ({t})",
    "Presupuesto actualizado de {t}",
    "Confirmación de cita: {t}",
    "Cambios en el calendario de {t}",
];
const SUBJ_EN: &[&str] = &[
    "Quarterly report: {t}",
    "Quick question about {t}",
    "Invoice for {t}",
    "Kickoff meeting — {t}",
    "Follow-up on {t}",
    "Draft agreement for {t}",
    "Your {t} order has shipped",
    "Weekly digest: {t}",
];
const TOPICS: &[&str] = &[
    "arrendamiento",
    "la fusión Altamira",
    "Proyecto Faro",
    "la auditoría 2026",
    "nómina de octubre",
    "el caso 241/2026",
    "Atlas migration",
    "Q3 budget",
    "the Lisbon offsite",
    "onboarding",
    "licencias de software",
    "la mudanza de oficina",
];
const BODY_ES: &[&str] = &[
    "Hola, te escribo para dar seguimiento a lo que comentamos la semana pasada.",
    "Adjunto la versión revisada con los cambios que pidió el cliente.",
    "¿Podemos vernos el jueves a las 10:00 para revisarlo con calma?",
    "Quedo atento a tus comentarios antes del viernes.",
    "El área de finanzas necesita la información completa para cerrar el mes.",
    "Te confirmo que ya se envió la documentación al juzgado.",
    "Hay un par de cláusulas que conviene renegociar antes de firmar.",
    "Gracias por la rapidez, esto nos ayuda muchísimo.",
    "Si te parece bien, preparo la minuta y la circulo con el equipo.",
];
const BODY_EN: &[&str] = &[
    "Hi, just following up on our conversation from last week.",
    "Please find the updated figures attached.",
    "Could we move the call to Thursday afternoon?",
    "Let me know if anything is unclear before we send it out.",
    "The team has reviewed the draft and has a few minor comments.",
    "Thanks again for your help with this.",
];
const REPLY: &[&str] = &[
    "Perfecto, gracias. Lo reviso hoy mismo.",
    "De acuerdo, nos vemos entonces.",
    "Sounds good, thanks!",
    "Te mando mis comentarios en un rato.",
    "Recibido. ¿Necesitas algo más de mi parte?",
    "Great, I'll loop in the rest of the team.",
];
const FILES: &[&str] = &[
    "contrato_v3.pdf",
    "presupuesto.xlsx",
    "minuta.docx",
    "factura_0042.pdf",
    "report-q3.pdf",
    "photos.zip",
    "plano_oficina.png",
];

fn person(rng: &mut Rng) -> Mailbox {
    let f = rng.pick_str(FIRST);
    let l = rng.pick_str(LAST);
    let dom = rng.pick_str(&["example.com", "example.org", "example.net", "mail.example.com"]);
    let addr = format!("{}.{}@{dom}", ascii_lower(f), ascii_lower(l));
    Mailbox::new(format!("{f} {l}"), addr)
}

fn ascii_lower(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'á' | 'Á' => 'a',
            'é' | 'É' => 'e',
            'í' | 'Í' => 'i',
            'ó' | 'Ó' => 'o',
            'ú' | 'Ú' => 'u',
            'ñ' | 'Ñ' => 'n',
            c => c.to_ascii_lowercase(),
        })
        .filter(char::is_ascii_alphanumeric)
        .collect()
}

fn fmt_addrs(v: &[Mailbox]) -> String {
    v.iter()
        .map(|m| match &m.name {
            Some(n) => format!("\"{n}\" <{}>", m.addr),
            None => format!("<{}>", m.addr),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Mensaje RFC 822 sencillo (texto plano UTF-8) a partir de un sobre y un cuerpo.
pub fn render_rfc822(e: &MessageEnvelope, body: &str) -> Vec<u8> {
    let mut s = String::with_capacity(body.len().saturating_add(600));
    s.push_str(&format!("From: {}\r\n", fmt_addrs(&e.from)));
    s.push_str(&format!("To: {}\r\n", fmt_addrs(&e.to)));
    if !e.cc.is_empty() {
        s.push_str(&format!("Cc: {}\r\n", fmt_addrs(&e.cc)));
    }
    s.push_str(&format!("Subject: {}\r\n", e.subject));
    s.push_str(&format!("Date: {}\r\n", e.date.to_rfc2822()));
    if let Some(m) = &e.message_id {
        s.push_str(&format!("Message-ID: <{m}>\r\n"));
    }
    if let Some(m) = &e.in_reply_to {
        s.push_str(&format!("In-Reply-To: <{m}>\r\n"));
    }
    if !e.references.is_empty() {
        let refs: Vec<String> = e.references.iter().map(|r| format!("<{r}>")).collect();
        s.push_str(&format!("References: {}\r\n", refs.join(" ")));
    }
    s.push_str(
        "MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n",
    );
    s.push_str(body);
    s.push_str("\r\n");
    s.into_bytes()
}

/// Mensaje RFC 822 `multipart/alternative` con texto plano y HTML.
pub fn render_rfc822_html(e: &MessageEnvelope, text: &str, html: &str) -> Vec<u8> {
    let plain = render_rfc822(e, "");
    // Se reutilizan los encabezados del texto plano y se cambia el cuerpo.
    let head_end = plain.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(plain.len());
    let head = String::from_utf8_lossy(plain.get(..head_end).unwrap_or(&[])).replace(
        "Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit",
        "Content-Type: multipart/alternative; boundary=\"iuredesk-alt\"",
    );
    let mut s = head;
    s.push_str(
        "\r\n\r\n--iuredesk-alt\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n",
    );
    s.push_str(text);
    s.push_str(
        "\r\n--iuredesk-alt\r\nContent-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n",
    );
    s.push_str(html);
    s.push_str("\r\n--iuredesk-alt--\r\n");
    s.into_bytes()
}

/// HTML de un boletín de ejemplo: tablas, estilos en línea, una imagen remota (se bloquea por
/// omisión) y enlaces. Los dominios son de RFC 2606.
fn newsletter_html(n: usize, to: &str) -> String {
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>Boletín semanal #{n}</title></head>
<body style="margin:0;background:#f3f5f9;font-family:Helvetica,Arial,sans-serif;color:#1f2933">
<table role="presentation" width="100%" cellpadding="0" cellspacing="0" style="background:#f3f5f9"><tr><td align="center" style="padding:24px 12px">
<table role="presentation" width="560" cellpadding="0" cellspacing="0" style="background:#ffffff;border-radius:10px;border:1px solid #e1e6ee">
<tr><td style="padding:20px 28px;background:#11284c;border-radius:10px 10px 0 0;color:#ffffff">
<div style="font-size:12px;letter-spacing:2px;text-transform:uppercase;opacity:.8">Boletín semanal · número {n}</div>
<div style="font-size:24px;font-weight:bold;margin-top:6px">Novedades del sector legal</div></td></tr>
<tr><td style="padding:0"><img src="https://updates.example.org/img/portada-{n}.png" width="560" height="160" alt="Portada del boletín" style="display:block;width:100%;height:auto;background:#dbe4f3"></td></tr>
<tr><td style="padding:24px 28px;font-size:15px;line-height:1.55">
<p style="margin:0 0 12px">Hola {to},</p>
<p style="margin:0 0 12px">Esta semana: la reforma al <b>código de procedimientos</b> entra en vigor, tres criterios nuevos de la Suprema Corte y una guía práctica para firmar contratos en línea.</p>
<table role="presentation" width="100%" cellpadding="0" cellspacing="0" style="margin:16px 0">
<tr><td width="50%" style="padding:12px;background:#eef3fb;border-radius:8px;vertical-align:top"><div style="font-weight:bold;color:#0650ec">Reforma procesal</div><div style="font-size:13px;color:#52606d">Qué cambia en plazos y notificaciones.</div></td>
<td width="12"></td>
<td width="50%" style="padding:12px;background:#eefaf3;border-radius:8px;vertical-align:top"><div style="font-weight:bold;color:#15803d">Firma electrónica</div><div style="font-size:13px;color:#52606d">Checklist para contratos válidos.</div></td></tr></table>
<p style="margin:16px 0"><a href="https://updates.example.org/boletin/{n}" style="display:inline-block;background:#0650ec;color:#ffffff;text-decoration:none;padding:10px 18px;border-radius:6px;font-weight:bold">Leer el boletín completo</a></p>
<p style="margin:0;font-size:12px;color:#7b8794">Recibes este correo porque te suscribiste en updates.example.org. <a href="https://updates.example.org/baja" style="color:#7b8794">Darte de baja</a>.</p>
</td></tr></table></td></tr></table></body></html>"#
    )
}

struct AccountSpec {
    name: &'static str,
    email: &'static str,
    spanish: bool,
    threads: usize,
    custom: &'static [&'static str],
}

const ACCOUNTS: &[AccountSpec] = &[
    AccountSpec {
        name: "Laura Méndez",
        email: "laura@despacho.example.org",
        spanish: true,
        threads: 330,
        custom: &["Clientes", "Juzgados"],
    },
    AccountSpec {
        name: "Laura Méndez",
        email: "laura.mendez@personal.example.com",
        spanish: true,
        threads: 220,
        custom: &["Facturas", "Viajes"],
    },
    AccountSpec {
        name: "Laura Mendez",
        email: "lmendez@example.net",
        spanish: false,
        threads: 180,
        custom: &["Projects", "Receipts"],
    },
];

/// Llena `store` con el buzón de demostración. `now` fija «hoy» (los eventos caen en la semana
/// de `now` y los correos en los 120 días anteriores).
pub fn populate(store: &Store, seed: u64, now: DateTime<Utc>) -> Result<SampleSummary> {
    populate_in(store, seed, now, Utc.fix())
}

/// Como [`populate`], pero las citas caen a horas redondas de la zona `offset` (la del usuario en
/// `iuredesk --sample`; UTC en capturas y pruebas, que deben ser deterministas).
pub fn populate_in(store: &Store, seed: u64, now: DateTime<Utc>, offset: FixedOffset) -> Result<SampleSummary> {
    let mut rng = Rng::new(seed);
    let mut sum = SampleSummary::default();

    // Contactos: 50, compartidos como remitentes del correo.
    let book = store.put_address_book(&AddressBook { name: "Contactos".into(), ..Default::default() })?;
    let mut people = Vec::with_capacity(50);
    while people.len() < 50 {
        let p = person(&mut rng);
        if people.iter().any(|x: &Mailbox| x.addr == p.addr) {
            continue;
        }
        people.push(p);
    }
    for (i, p) in people.iter().enumerate() {
        let name = p.display().to_string();
        let (given, family) = name.split_once(' ').map_or((name.clone(), String::new()), |(a, b)| (a.into(), b.into()));
        let (city, country) = rng.pick(CITIES).copied().unwrap_or(("", ""));
        store.put_contact(&Contact {
            address_book: book,
            uid: format!("sample-contact-{i}"),
            full_name: name,
            given_name: given,
            family_name: family,
            emails: vec![Labeled::new("work", p.addr.clone())],
            phones: vec![Labeled::new("mobile", format!("+52 55 {:04} {:04}", rng.below(10_000), rng.below(10_000)))],
            org: rng.pick_str(ORGS).into(),
            title: rng.pick_str(TITLES).into(),
            addresses: vec![PostalAddress {
                label: "work".into(),
                street: format!("Av. Reforma {}", rng.below(900).saturating_add(1)),
                city: city.into(),
                country: country.into(),
                ..Default::default()
            }],
            notes: if rng.chance(20) { "Prefiere llamadas por la mañana.".into() } else { String::new() },
            ..Default::default()
        })?;
        sum.contacts = sum.contacts.saturating_add(1);
    }

    for (ai, spec) in ACCOUNTS.iter().enumerate() {
        let account = AccountId(u64::try_from(ai).unwrap_or(0).saturating_add(1));
        store.put_account_label(&AccountLabel { id: account, name: spec.name.into(), email: spec.email.into() })?;
        sum.accounts.push(account);
        let me = Mailbox::new(spec.name, spec.email);

        let mk = |name: &str, role: FolderRole, path: &str| Folder {
            account,
            name: name.into(),
            remote_path: path.into(),
            role,
            ..Default::default()
        };
        let (inbox_n, sent_n, drafts_n, arch_n, trash_n, junk_n) = if spec.spanish {
            ("Bandeja de entrada", "Enviados", "Borradores", "Archivo", "Papelera", "No deseado")
        } else {
            ("Inbox", "Sent", "Drafts", "Archive", "Trash", "Junk")
        };
        let inbox = store.put_folder(&mk(inbox_n, FolderRole::Inbox, "INBOX"))?;
        let sent = store.put_folder(&mk(sent_n, FolderRole::Sent, "Sent"))?;
        let drafts = store.put_folder(&mk(drafts_n, FolderRole::Drafts, "Drafts"))?;
        let archive = store.put_folder(&mk(arch_n, FolderRole::Archive, "Archive"))?;
        let trash = store.put_folder(&mk(trash_n, FolderRole::Trash, "Trash"))?;
        let junk = store.put_folder(&mk(junk_n, FolderRole::Junk, "Junk"))?;
        let mut custom = Vec::new();
        for c in spec.custom {
            custom.push(store.put_folder(&mk(c, FolderRole::Custom, &format!("INBOX/{c}")))?);
        }
        sum.folders = sum.folders.saturating_add(6 + custom.len());

        let mut batch: Vec<NewMessage> = Vec::new();
        for t in 0..spec.threads {
            let spanish = spec.spanish || rng.chance(15);
            let topic = rng.pick_str(TOPICS);
            let subject = rng.pick_str(if spanish { SUBJ_ES } else { SUBJ_EN }).replace("{t}", topic);
            let other = rng.pick(&people).cloned().unwrap_or_default();
            let mut cc = Vec::new();
            if rng.chance(25) {
                cc.push(rng.pick(&people).cloned().unwrap_or_default());
            }
            let len = match rng.below(100) {
                0..=44 => 1,
                45..=69 => 2,
                70..=84 => 3,
                85..=94 => 4,
                _ => 5 + rng.below(4),
            };
            let started_by_me = rng.chance(20);
            let mut date = now - Duration::minutes(i64::try_from(rng.below(120 * 24 * 60)).unwrap_or(0));
            // Destino del hilo recibido: bandeja, archivo o carpeta propia (lo viejo se archiva).
            let age_days = (now - date).num_days();
            let recv_folder = if age_days > 45 && rng.chance(50) {
                archive
            } else if !custom.is_empty() && rng.chance(20) {
                rng.pick(&custom).copied().unwrap_or(inbox)
            } else if rng.chance(3) {
                trash
            } else {
                inbox
            };
            let mut refs: Vec<String> = Vec::new();
            let flagged_thread = rng.chance(6);
            let mut msgs: Vec<MessageEnvelope> = Vec::new();
            let mut bodies: Vec<String> = Vec::new();
            for i in 0..len {
                let from_me = (i % 2 == 0) == started_by_me;
                let (from, to) = if from_me { (me.clone(), other.clone()) } else { (other.clone(), me.clone()) };
                let mid = format!("{ai}.{t}.{i}@sample.iuredesk.example");
                let body = if i == 0 {
                    let pool = if spanish { BODY_ES } else { BODY_EN };
                    let n = 2 + rng.below(3);
                    let mut b = String::new();
                    b.push_str(if spanish { "Hola " } else { "Hi " });
                    b.push_str(to.display().split(' ').next().unwrap_or(""));
                    b.push_str(",\n\n");
                    for _ in 0..n {
                        b.push_str(rng.pick_str(pool));
                        b.push(' ');
                    }
                    b.push_str(if spanish { "\n\nSaludos,\n" } else { "\n\nBest,\n" });
                    b.push_str(from.display());
                    b
                } else {
                    // Se cita el mensaje anterior completo (con su propia cita), como hacen los
                    // clientes reales, precedido de la línea de atribución.
                    let prev = bodies.last().map(String::as_str).unwrap_or("");
                    let prev_from = msgs.last().and_then(|m| m.from.first()).map(|m| m.display().to_string());
                    let prev_date = msgs.last().map(|m| m.date.with_timezone(&offset));
                    quote_reply(rng.pick_str(REPLY), prev, prev_from.as_deref().unwrap_or(""), prev_date, spanish)
                };
                let with_att = i == 0 && rng.chance(18);
                let attachments = if with_att {
                    vec![AttachmentMeta {
                        filename: rng.pick_str(FILES).into(),
                        mime_type: "application/octet-stream".into(),
                        size: 20_000 + rng.below(800_000),
                        ..Default::default()
                    }]
                } else {
                    Vec::new()
                };
                let recent = (now - date).num_hours() < 48;
                let seen = from_me || !recent || rng.chance(30);
                let env = MessageEnvelope {
                    folder: if from_me { sent } else { recv_folder },
                    message_id: Some(mid.clone()),
                    in_reply_to: refs.last().cloned(),
                    references: refs.clone(),
                    from: vec![from.clone()],
                    to: vec![to],
                    cc: cc.clone(),
                    subject: if i == 0 {
                        subject.clone()
                    } else {
                        format!("{}{subject}", if spanish { "RE: " } else { "Re: " })
                    },
                    date,
                    flags: Flags { seen, flagged: flagged_thread && i == 0, ..Default::default() },
                    has_attachments: with_att,
                    attachments,
                    preview: make_preview(&body),
                    keywords: if rng.chance(8) { vec!["Importante".into()] } else { Vec::new() },
                    ..Default::default()
                };
                refs.push(mid);
                msgs.push(env);
                bodies.push(body);
                date += Duration::minutes(i64::try_from(10 + rng.below(36 * 60)).unwrap_or(10));
                if date > now {
                    date = now - Duration::minutes(i64::try_from(rng.below(30)).unwrap_or(0));
                }
            }
            // «Respondido» en los recibidos que tienen una respuesta mía después.
            for i in 0..msgs.len() {
                let replied = msgs.get(i.saturating_add(1)).is_some_and(|n| n.from.first() == Some(&me));
                if let Some(m) = msgs.get_mut(i)
                    && replied
                    && m.from.first() != Some(&me)
                {
                    m.flags.answered = true;
                }
            }
            for (env, body) in msgs.into_iter().zip(bodies) {
                let raw = render_rfc822(&env, &body);
                batch.push(NewMessage { envelope: env, raw: Some(raw), body_text: Some(body) });
            }
            if batch.len() >= 500 {
                sum.messages = sum.messages.saturating_add(batch.len());
                store.put_messages(std::mem::take(&mut batch))?;
            }
        }
        // Boletines y spam sueltos.
        for n in 0..25 {
            let junk_msg = n % 3 == 0;
            let sender = if junk_msg {
                Mailbox::new("Ofertas Increíbles", "promo@deals.example.com")
            } else {
                Mailbox::new("Newsletter", "news@updates.example.org")
            };
            let date = now - Duration::hours(i64::try_from(rng.below(90 * 24)).unwrap_or(0));
            let body = if junk_msg {
                "¡Gana un premio! Haz clic aquí para reclamarlo hoy mismo.".to_string()
            } else {
                format!("Resumen semanal #{n}: novedades del sector, eventos y lecturas recomendadas.")
            };
            let env = MessageEnvelope {
                folder: if junk_msg { junk } else { inbox },
                message_id: Some(format!("{ai}.news.{n}@updates.example.org")),
                from: vec![sender],
                to: vec![me.clone()],
                subject: if junk_msg {
                    "¡Felicidades, has sido seleccionado!".into()
                } else {
                    format!("Boletín semanal #{n}")
                },
                date,
                flags: Flags { seen: rng.chance(60), ..Default::default() },
                preview: make_preview(&body),
                ..Default::default()
            };
            // Los boletines llegan en HTML (multipart/alternative), como los reales.
            let raw = if junk_msg {
                render_rfc822(&env, &body)
            } else {
                render_rfc822_html(&env, &body, &newsletter_html(n, me.display()))
            };
            batch.push(NewMessage { envelope: env, raw: Some(raw), body_text: Some(body) });
        }
        // Un par de borradores.
        for n in 0..2 {
            let body = "Borrador: pendiente de revisar antes de enviar.".to_string();
            let env = MessageEnvelope {
                folder: drafts,
                from: vec![me.clone()],
                to: vec![people.get(n).cloned().unwrap_or_default()],
                subject: format!("(Borrador) Propuesta {}", n + 1),
                date: now - Duration::hours(i64::try_from(n).unwrap_or(0).saturating_add(3)),
                flags: Flags { seen: true, draft: true, ..Default::default() },
                preview: make_preview(&body),
                ..Default::default()
            };
            let raw = render_rfc822(&env, &body);
            batch.push(NewMessage { envelope: env, raw: Some(raw), body_text: Some(body) });
        }
        sum.messages = sum.messages.saturating_add(batch.len());
        store.put_messages(batch)?;
        store.rethread_account(account)?;
    }

    // Calendario: la semana de `now` (lunes a domingo).
    let work = store.put_calendar(&Calendar {
        account: AccountId(1),
        name: "Trabajo".into(),
        color: Some(0x3B82F6),
        ..Default::default()
    })?;
    let home = store.put_calendar(&Calendar {
        account: AccountId(2),
        name: "Personal".into(),
        color: Some(0x10B981),
        ..Default::default()
    })?;
    // Las horas son de la zona del usuario: «09:00» es las 9 de su mañana, no las 9 UTC.
    let local_now = now.with_timezone(&offset);
    let monday = (local_now - Duration::days(i64::from(local_now.weekday().num_days_from_monday()))).date_naive();
    let at = |day: i64, h: u32, m: u32| -> DateTime<Utc> {
        let d = monday + Duration::days(day);
        let local = d.and_time(NaiveTime::from_hms_opt(h, m, 0).unwrap_or(NaiveTime::MIN));
        offset.from_local_datetime(&local).single().map_or_else(|| local.and_utc(), |t| t.with_timezone(&Utc))
    };
    let titles_work = [
        ("Audiencia caso 241/2026", "Juzgado 5º Civil"),
        ("Revisión de contrato con Altamira", "Sala de juntas"),
        ("Llamada con Northwind", "Videollamada"),
        ("Comida con cliente", "Restaurante El Faro"),
        ("Preparar alegatos", ""),
        ("Comité de socios", "Sala 2"),
        ("1:1 con Pablo", "Oficina"),
    ];
    let mut ev_count = 0usize;
    for day in 0..5i64 {
        let n = 2 + rng.below(3);
        let mut hour = 9u32;
        for _ in 0..n {
            let (title, loc) = rng.pick(&titles_work).copied().unwrap_or(("Reunión", ""));
            let dur = [60u32, 90, 60].get(usize::try_from(rng.below(3)).unwrap_or(0)).copied().unwrap_or(60);
            let start = at(day, hour, 0);
            let end = start + Duration::minutes(i64::from(dur));
            let attendees: Vec<Attendee> = (0..rng.below(4))
                .map(|_| Attendee {
                    mailbox: rng.pick(&people).cloned().unwrap_or_default(),
                    partstat: [PartStat::Accepted, PartStat::Tentative, PartStat::NeedsAction, PartStat::Declined]
                        .get(usize::try_from(rng.below(4)).unwrap_or(0))
                        .cloned()
                        .unwrap_or_default(),
                    role: AttendeeRole::Required,
                    rsvp: true,
                })
                .collect();
            store.put_event(&Event {
                calendar: work,
                uid: format!("sample-ev-{day}-{hour}@iuredesk.example"),
                summary: title.into(),
                location: loc.into(),
                description: String::new(),
                start: EventTime::DateTime { utc: start, tzid: Some("America/Mexico_City".into()) },
                end: EventTime::DateTime { utc: end, tzid: Some("America/Mexico_City".into()) },
                organizer: Some(Mailbox::new(ACCOUNTS.first().map_or("", |a| a.name), "laura@despacho.example.org")),
                attendees,
                reminders: vec![Reminder { minutes_before: 15 }],
                status: if rng.chance(10) { EventStatus::Tentative } else { EventStatus::Confirmed },
                ..Default::default()
            })?;
            ev_count = ev_count.saturating_add(1);
            hour = hour.saturating_add(1 + u32::try_from(rng.below(3)).unwrap_or(0)).min(19);
        }
    }
    // Recurrente, de día completo y personales.
    store.put_event(&Event {
        calendar: work,
        uid: "sample-standup@iuredesk.example".into(),
        summary: "Junta semanal del despacho".into(),
        start: EventTime::DateTime { utc: at(0, 8, 30), tzid: Some("America/Mexico_City".into()) },
        end: EventTime::DateTime { utc: at(0, 9, 0), tzid: Some("America/Mexico_City".into()) },
        rrule: Some("FREQ=WEEKLY;BYDAY=MO".into()),
        ..Default::default()
    })?;
    store.put_event(&Event {
        calendar: home,
        uid: "sample-bday@iuredesk.example".into(),
        summary: "Cumpleaños de mamá".into(),
        start: EventTime::Date(monday + Duration::days(3)),
        end: EventTime::Date(monday + Duration::days(4)),
        transparency: Transparency::Transparent,
        ..Default::default()
    })?;
    for (day, h, title) in [(1i64, 19u32, "Clase de yoga"), (5, 11, "Mercado"), (6, 14, "Comida familiar")] {
        store.put_event(&Event {
            calendar: home,
            uid: format!("sample-home-{day}@iuredesk.example"),
            summary: title.into(),
            start: EventTime::DateTime { utc: at(day, h, 0), tzid: Some("America/Mexico_City".into()) },
            end: EventTime::DateTime {
                utc: at(day, h.saturating_add(1), 30),
                tzid: Some("America/Mexico_City".into()),
            },
            ..Default::default()
        })?;
    }
    sum.events = ev_count.saturating_add(5);

    // Notas.
    let notes = [
        (
            "Ideas para el despacho",
            "# Ideas\n\n- Plantillas de contratos\n- Firma electrónica\n- **Agenda compartida**",
            vec!["trabajo"],
        ),
        (
            "Lista de lectura",
            "1. *Clean Code*\n2. *El nombre de la rosa*\n3. *Thinking, Fast and Slow*",
            vec!["personal"],
        ),
        (
            "Minuta 12/10",
            "## Acuerdos\n\n- Enviar propuesta a Altamira\n- Revisar honorarios",
            vec!["trabajo", "minutas"],
        ),
        ("Packing list — Lisbon", "- Passport\n- Charger\n- Adapter (type F)", vec!["viajes"]),
        ("Recetas", "### Salsa verde\n\nTomate verde, chile serrano, cebolla, cilantro.", vec![]),
    ];
    for (i, (title, body, tags)) in notes.into_iter().enumerate() {
        let updated = now - Duration::hours(i64::try_from(i).unwrap_or(0).saturating_mul(29));
        store.put_note(&Note {
            title: title.into(),
            body: body.into(),
            tags: tags.into_iter().map(String::from).collect(),
            pinned: i == 0,
            created: updated - Duration::days(7),
            updated,
            ..Default::default()
        })?;
        sum.notes = sum.notes.saturating_add(1);
    }

    // Tareas.
    let lists = [
        store.put_task_list(&TaskList {
            account: Some(AccountId(1)),
            name: "Despacho".into(),
            ..Default::default()
        })?,
        store.put_task_list(&TaskList { account: Some(AccountId(2)), name: "Casa".into(), ..Default::default() })?,
    ];
    let tasks: [(usize, &str, i64, u8, bool); 12] = [
        (0, "Enviar propuesta a Altamira", 1, 1, false),
        (0, "Revisar honorarios de octubre", 2, 5, false),
        (0, "Presentar escrito caso 241/2026", 0, 1, false),
        (0, "Actualizar plantilla de contrato", 6, 9, false),
        (0, "Llamar al notario", -1, 5, true),
        (0, "Archivar expedientes 2024", 10, 0, false),
        (0, "Preparar informe trimestral", 4, 5, false),
        (1, "Pagar tarjeta de crédito", 3, 1, false),
        (1, "Renovar pasaporte", 20, 5, false),
        (1, "Comprar regalo de cumpleaños", 2, 5, false),
        (1, "Cita con el dentista", -3, 0, true),
        (1, "Reservar vuelo a Lisboa", 8, 9, false),
    ];
    for (i, (list, summary, due_days, priority, done)) in tasks.into_iter().enumerate() {
        let list = lists.get(list).copied().unwrap_or_default();
        store.put_task(&Task {
            list,
            uid: format!("sample-task-{i}@iuredesk.example"),
            summary: summary.into(),
            due: Some(EventTime::Date((now + Duration::days(due_days)).date_naive())),
            priority,
            status: if done { TaskStatus::Completed } else { TaskStatus::NeedsAction },
            completed: if done { Some(now - Duration::days(1)) } else { None },
            ..Default::default()
        })?;
        sum.tasks = sum.tasks.saturating_add(1);
    }
    add_invitation(store, now, offset)?;
    sum.messages = sum.messages.saturating_add(1);
    Ok(sum)
}

/// Correo con una invitación iMIP (`METHOD:REQUEST`) en la bandeja de la primera cuenta: la
/// tarjeta de invitación del panel de lectura (F2.4). Es para pasado mañana a las 16:00 locales.
pub fn add_invitation(store: &Store, now: DateTime<Utc>, offset: FixedOffset) -> Result<()> {
    let account = AccountId(1);
    let Some(inbox) = store.folders(account)?.into_iter().find(|f| f.role == FolderRole::Inbox) else { return Ok(()) };
    let me = ACCOUNTS.first().map_or("laura@despacho.example.org", |a| a.email);
    let day = (now.with_timezone(&offset) + Duration::days(1)).date_naive();
    let local = day.and_time(NaiveTime::from_hms_opt(16, 0, 0).unwrap_or(NaiveTime::MIN));
    let start = offset.from_local_datetime(&local).single().map_or_else(|| local.and_utc(), |t| t.with_timezone(&Utc));
    let end = start + Duration::hours(1);
    let f = |t: DateTime<Utc>| t.format("%Y%m%dT%H%M%SZ").to_string();
    let ics = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//IureDesk//Ejemplo//ES\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\n\
UID:sample-invite-revision@northwind.example.com\r\nSEQUENCE:0\r\nDTSTAMP:{}\r\nDTSTART:{}\r\nDTEND:{}\r\n\
SUMMARY:Revisión trimestral con Northwind\r\nLOCATION:Sala de juntas, piso 4\r\n\
DESCRIPTION:Repasamos el avance del trimestre y los siguientes pasos.\r\n\
ORGANIZER;CN=Carmen Brown:mailto:carmen.brown@northwind.example.com\r\n\
ATTENDEE;CN=Carmen Brown;ROLE=CHAIR;PARTSTAT=ACCEPTED:mailto:carmen.brown@northwind.example.com\r\n\
ATTENDEE;CN=Laura Méndez;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:{me}\r\n\
ATTENDEE;CN=Pablo Ruiz;ROLE=OPT-PARTICIPANT;PARTSTAT=TENTATIVE:mailto:pablo@despacho.example.org\r\n\
END:VEVENT\r\nEND:VCALENDAR\r\n",
        f(now - Duration::minutes(40)),
        f(start),
        f(end)
    );
    let body = "Hola Laura,\r\n\r\nTe envío la invitación para la revisión trimestral.\r\n\r\nCarmen";
    let env = MessageEnvelope {
        account,
        folder: inbox.id,
        message_id: Some("sample-invite@northwind.example.com".into()),
        from: vec![Mailbox::new("Carmen Brown", "carmen.brown@northwind.example.com")],
        to: vec![Mailbox::new("Laura Méndez", me)],
        subject: "Invitación: Revisión trimestral con Northwind".into(),
        date: now - Duration::minutes(40),
        preview: make_preview(body),
        ..Default::default()
    };
    let mut raw = String::new();
    raw.push_str(&format!("From: {}\r\nTo: {}\r\n", fmt_addrs(&env.from), fmt_addrs(&env.to)));
    raw.push_str(&format!("Subject: {}\r\nDate: {}\r\n", env.subject, env.date.to_rfc2822()));
    raw.push_str("Message-ID: <sample-invite@northwind.example.com>\r\nMIME-Version: 1.0\r\n");
    raw.push_str("Content-Type: multipart/alternative; boundary=\"inv\"\r\n\r\n--inv\r\n");
    raw.push_str("Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n");
    raw.push_str(body);
    raw.push_str(
        "\r\n--inv\r\nContent-Type: text/calendar; charset=utf-8; method=REQUEST; name=\"invitacion.ics\"\r\n",
    );
    raw.push_str("Content-Transfer-Encoding: 8bit\r\n\r\n");
    raw.push_str(&ics);
    raw.push_str("--inv--\r\n");
    store.put_messages(vec![NewMessage {
        envelope: env,
        raw: Some(raw.into_bytes()),
        body_text: Some(body.into()),
    }])?;
    store.rethread_account(account)?;
    Ok(())
}

/// Abre (o crea) un almacén de demostración en `dir`. Si está vacío lo llena con
/// [`populate`] usando [`DEFAULT_SEED`] y la fecha actual.
pub fn open_sample(dir: impl AsRef<std::path::Path>) -> Result<Store> {
    let dir = dir.as_ref();
    // Un buzón de ejemplo de una versión anterior (citas en UTC, citas truncadas) se rehace.
    let marker = dir.join(SAMPLE_MARKER);
    if dir.exists() && !marker.exists() {
        let _ = std::fs::remove_dir_all(dir);
    }
    let store = Store::open(dir)?;
    if store.message_count()? == 0 {
        let offset = chrono::Local::now().offset().fix();
        populate_in(&store, DEFAULT_SEED, Utc::now(), offset)?;
        let _ = std::fs::write(&marker, b"2");
    }
    Ok(store)
}

/// Archivo que marca la versión del buzón de ejemplo.
const SAMPLE_MARKER: &str = ".sample-v2";

/// Respuesta con la cita completa del mensaje anterior (acotada a 80 líneas) y su atribución
/// («El lun 13 oct 2026 a las 10:13, Ana escribió:»).
fn quote_reply(
    reply: &str,
    prev: &str,
    prev_from: &str,
    prev_date: Option<DateTime<FixedOffset>>,
    spanish: bool,
) -> String {
    let when = prev_date.map(|d| d.format("%d/%m/%Y %H:%M").to_string()).unwrap_or_default();
    let attribution = match (spanish, when.is_empty()) {
        (true, false) => format!("El {when}, {prev_from} escribió:"),
        (true, true) => format!("{prev_from} escribió:"),
        (false, false) => format!("On {when}, {prev_from} wrote:"),
        (false, true) => format!("{prev_from} wrote:"),
    };
    let mut out = format!("{reply}\n\n{attribution}\n");
    for l in prev.lines().take(80) {
        if l.is_empty() {
            out.push_str(">\n");
        } else if l.starts_with('>') {
            out.push('>');
            out.push_str(l);
            out.push('\n');
        } else {
            out.push_str("> ");
            out.push_str(l);
            out.push('\n');
        }
    }
    out
}

const WORDS: &[&str] = &[
    "contrato",
    "reunión",
    "factura",
    "proyecto",
    "cliente",
    "audiencia",
    "propuesta",
    "informe",
    "presupuesto",
    "calendario",
    "budget",
    "meeting",
    "invoice",
    "report",
    "deadline",
    "review",
    "draft",
    "update",
    "schedule",
    "Altamira",
    "Northwind",
    "Faro",
    "Atlas",
    "Lisboa",
    "octubre",
    "trimestre",
    "firma",
    "abogado",
    "nómina",
    "auditoría",
    "pago",
    "entrega",
    "agenda",
    "minuta",
    "expediente",
    "juzgado",
    "demanda",
    "acuerdo",
    "honorarios",
];

/// Mensajes sintéticos sueltos para pruebas de carga: `count` mensajes repartidos entre
/// `folders`, con fechas en el último año y textos de ~1–3 KB.
pub fn synthetic_messages(rng: &mut Rng, folders: &[FolderId], count: usize, now: DateTime<Utc>) -> Vec<NewMessage> {
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let folder = rng.pick(folders).copied().unwrap_or_default();
        let from = person(rng);
        let to = person(rng);
        let n_subj = 3 + rng.below(5);
        let subject: Vec<&str> = (0..n_subj).map(|_| rng.pick_str(WORDS)).collect();
        let n_body = 120 + rng.below(300);
        let mut body = String::with_capacity(usize::try_from(n_body).unwrap_or(0).saturating_mul(9));
        for i in 0..n_body {
            body.push_str(rng.pick_str(WORDS));
            body.push(if i % 14 == 13 { '\n' } else { ' ' });
        }
        let id = rng.next_u64();
        let with_att = rng.chance(15);
        let env = MessageEnvelope {
            folder,
            remote_id: Some(format!("{id:x}")),
            message_id: Some(format!("{id:x}@synthetic.example")),
            from: vec![from],
            to: vec![to],
            subject: subject.join(" "),
            date: now - Duration::seconds(i64::try_from(rng.below(365 * 86_400)).unwrap_or(0)),
            flags: Flags { seen: rng.chance(70), flagged: rng.chance(5), ..Default::default() },
            has_attachments: with_att,
            attachments: if with_att {
                vec![AttachmentMeta { filename: rng.pick_str(FILES).into(), ..Default::default() }]
            } else {
                Vec::new()
            },
            preview: make_preview(&body),
            ..Default::default()
        };
        let raw = render_rfc822(&env, &body);
        out.push(NewMessage { envelope: env, raw: Some(raw), body_text: Some(body) });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 21, 15, 0, 0).unwrap()
    }

    #[test]
    fn rng_is_deterministic() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        assert_eq!(Rng::new(1).below(0), 0);
    }

    #[test]
    fn sample_mailbox() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let sum = populate(&s, DEFAULT_SEED, now()).unwrap();
        assert_eq!(sum.accounts.len(), 3);
        assert!((1_500..=2_600).contains(&sum.messages), "{}", sum.messages);
        assert_eq!(s.message_count().unwrap() as usize, sum.messages);
        assert_eq!(sum.contacts, 50);
        assert!(sum.events >= 15);
        let stats = s.stats().unwrap();
        assert!(stats.threads > 500 && stats.threads < sum.messages as u64);

        // Bandeja de entrada no vacía, por fecha descendente, con hilos de varios mensajes.
        let inbox = s.folders(AccountId(1)).unwrap().into_iter().find(|f| f.role == FolderRole::Inbox).unwrap();
        let page = s.list_envelopes(inbox.id, 0, 50).unwrap();
        assert_eq!(page.len(), 50);
        assert!(page.windows(2).all(|w| w[0].date >= w[1].date));
        let t = page.iter().find_map(|e| e.thread).unwrap();
        assert!(s.get_thread(t).unwrap().is_some());
        let multi = page.iter().filter_map(|e| e.thread).any(|t| s.thread_envelopes(t).unwrap().len() > 1);
        assert!(multi);
        assert!(!s.get_raw(page[0].id).unwrap().is_empty());

        // Búsqueda.
        assert!(!s.search("contrato", 20).unwrap().is_empty());
        assert!(!s.search("has:attachment", 20).unwrap().is_empty());
        assert!(!s.search("in:inbox is:unread", 20).unwrap().is_empty());

        // Calendario de la semana.
        let monday = Utc.with_ymd_and_hms(2026, 10, 19, 0, 0, 0).unwrap();
        let week = s.events_between(&[], monday, monday + Duration::days(7)).unwrap();
        assert!(week.len() >= 15, "{}", week.len());
        assert_eq!(s.notes().unwrap().len(), 5);

        // Determinismo: otra generación con la misma semilla da los mismos asuntos.
        let dir2 = tempfile::tempdir().unwrap();
        let s2 = Store::open(dir2.path()).unwrap();
        populate(&s2, DEFAULT_SEED, now()).unwrap();
        let a: Vec<String> = s.list_envelopes(inbox.id, 0, 100).unwrap().into_iter().map(|e| e.subject).collect();
        let b: Vec<String> = s2.list_envelopes(inbox.id, 0, 100).unwrap().into_iter().map(|e| e.subject).collect();
        assert_eq!(a, b);
    }

    #[test]
    fn replies_quote_the_real_previous_text() {
        let r = quote_reply("Va.", "Hola Ana,\n\nTe envío el contrato.\n\nSaludos", "Luis", None, true);
        assert!(r.starts_with("Va.\n\nLuis escribió:\n"));
        assert!(r.contains("> Te envío el contrato.\n>\n> Saludos"));
        // La cita de la cita se anida sin espacios de más.
        let r2 = quote_reply("Ok", &r, "Ana", None, false);
        assert!(r2.contains(">> Te envío el contrato."));
        assert!(r2.contains("> Luis escribió:"));
        // En el buzón, ninguna respuesta termina en una atribución vacía.
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        populate(&s, DEFAULT_SEED, now()).unwrap();
        let hits = s.search("Re:", 200).unwrap();
        let mut checked = 0;
        for e in hits.iter().filter(|e| e.in_reply_to.is_some()).take(40) {
            let raw = String::from_utf8_lossy(&s.get_raw(e.id).unwrap()).into_owned();
            let last = raw.trim_end().lines().last().unwrap_or("").to_string();
            assert!(!last.ends_with("escribió:") && !last.ends_with("wrote:"), "{last}");
            checked += 1;
        }
        assert!(checked > 5);
    }

    #[test]
    fn events_fall_on_local_hours() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let mx = FixedOffset::west_opt(6 * 3600).unwrap();
        populate_in(&s, DEFAULT_SEED, now(), mx).unwrap();
        let monday = Utc.with_ymd_and_hms(2026, 10, 19, 0, 0, 0).unwrap();
        let week = s.events_between(&[], monday, monday + Duration::days(8)).unwrap();
        let standup = week.iter().find(|e| e.summary.starts_with("Junta semanal")).unwrap();
        let start = standup.start.to_utc().with_timezone(&mx);
        assert_eq!(start.format("%H:%M").to_string(), "08:30");
    }
}
