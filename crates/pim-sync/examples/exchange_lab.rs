//! F8.5: IureDesk contra el Exchange del laboratorio (`tools/exchange-lab`). Recorre lo que hace la
//! app con dos buzones reales (Ana y Bea) y dice OK o FALLA en cada paso, sin detenerse:
//! descubrimiento (Autodiscover sin y con contraseña), correo de ida y vuelta, calendario con una
//! invitación, disponibilidad, contactos, tareas y libreta global.
//!
//! ```text
//! cargo run -p iuredesk-pim-sync --example exchange_lab
//! ```
//!
//! Lee las contraseñas de `~/VirtualBox VMs/iuredesk-ex1/lab/lab.env` (o `IUREDESK_LAB_ENV`) y no
//! las imprime. Para probar cada forma de entrar, cambia el modo en la VM (`Z:\6-modo.ps1`) y
//! vuelve a correrlo.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use iuredesk_accounts::{MemoryStore, ProbeOptions, SecretPurpose, SecretStore};
use iuredesk_ews::{BasicAuth, EwsClient, MessageDisposition};
use iuredesk_model::account::{Account, AccountKind, AuthMethod, Protocol, Security, ServerConfig};
use iuredesk_model::{Attendee, AttendeeRole, Contact, Event, EventTime, Labeled, Mailbox, PartStat, Task};
use iuredesk_pim_sync::{PimAccount, PimSyncEngine, PimSyncOptions};
use iuredesk_store::Store;

/// Inicio de sesión (UPN). El correo es otro (paso 5b del laboratorio): Exchange no entrega a
/// dominios reservados por la RFC 2606 como `.test`.
const ANA: &str = "ana@iuredesk.test";
const BEA: &str = "bea@iuredesk.test";
const ANA_MAIL: &str = "ana@iuredesk.internal";
const BEA_MAIL: &str = "bea@iuredesk.internal";

struct Report {
    ok: usize,
    failed: Vec<String>,
}

impl Report {
    fn check(&mut self, what: &str, r: Result<String, String>) {
        match r {
            Ok(detail) => {
                self.ok += 1;
                println!("  OK     {what}{}", if detail.is_empty() { String::new() } else { format!(" — {detail}") });
            }
            Err(e) => {
                self.failed.push(what.to_string());
                println!("  FALLA  {what} — {e}");
            }
        }
    }
}

fn lab_env() -> Result<HashMap<String, String>, String> {
    let path = std::env::var("IUREDESK_LAB_ENV").unwrap_or_else(|_| {
        format!("{}/VirtualBox VMs/iuredesk-ex1/lab/lab.env", std::env::var("HOME").unwrap_or_default())
    });
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
    Ok(text
        .lines()
        .filter_map(|l| l.split_once('='))
        .filter(|(k, _)| k.chars().all(|c| c.is_ascii_uppercase() || c == '_'))
        .map(|(k, v)| (k.to_string(), v.trim().to_string()))
        .collect())
}

fn account(id: &str, email: &str, login: &str, url: &str) -> Account {
    let srv = ServerConfig {
        protocol: Protocol::Ews,
        host: url.to_string(),
        port: 443,
        security: Security::Tls,
        auth: AuthMethod::Password,
        username: login.to_string(),
    };
    Account {
        id: id.into(),
        display_name: email.split('@').next().unwrap_or_default().into(),
        email: email.into(),
        kind: AccountKind::Exchange,
        incoming: srv.clone(),
        outgoing: srv,
        caldav_url: None,
        carddav_url: None,
        identities: vec![],
        shared_of: None,
    }
}

/// Una petición a EWS con solo el token de Kerberos (sin caer a NTLM): el certificado del
/// servidor (de una primera petición) va en el channel binding, como exige la Extended Protection.
async fn kerberos_only(url: &str, t: &iuredesk_ews::kerberos::Ticket) -> Result<String, String> {
    let http = reqwest::Client::builder().tls_info(true).http1_only().build().map_err(|e| e.to_string())?;
    let first = http.get(url).send().await.map_err(|e| e.to_string())?;
    let cert = first.extensions().get::<reqwest::tls::TlsInfo>().and_then(|i| i.peer_certificate()).map(<[u8]>::to_vec);
    let header = iuredesk_ews::kerberos::authorization(t, cert.as_deref()).map_err(|e| e.to_string())?;
    let body = iuredesk_ews::soap::envelope(
        r#"<m:GetFolder><m:FolderShape><t:BaseShape>IdOnly</t:BaseShape></m:FolderShape><m:FolderIds><t:DistinguishedFolderId Id="inbox"/></m:FolderIds></m:GetFolder>"#,
    );
    let resp = http
        .post(url)
        .header("Content-Type", "text/xml; charset=utf-8")
        .header("Authorization", header)
        .body(body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if status == 200 && text.contains("NoError") {
        Ok(format!("200, channel binding {}", if cert.is_some() { "incluido" } else { "sin certificado" }))
    } else {
        Err(format!("HTTP {status}"))
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let env = match lab_env() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("no se pudo leer lab.env: {e}");
            std::process::exit(2);
        }
    };
    let (ana_pw, bea_pw) =
        (env.get("ANA_PASSWORD").cloned().unwrap_or_default(), env.get("BEA_PASSWORD").cloned().unwrap_or_default());
    let mut r = Report { ok: 0, failed: Vec::new() };
    let stamp = Utc::now().format("%H%M%S").to_string();

    println!("1. Descubrimiento");
    let discovery = iuredesk_accounts::autoconfig::discover(ANA).await;
    let hint = discovery.as_ref().ok().and_then(|d| d.exchange.clone());
    r.check(
        "Autodiscover sin contraseña reconoce un Exchange",
        match &discovery {
            Ok(d) if d.kind == AccountKind::Exchange => Ok(format!(
                "{} servidor(es) POX, EWS por v2: {}",
                hint.as_ref().map_or(0, |h| h.autodiscover.len()),
                hint.as_ref().and_then(|h| h.ews_url.clone()).unwrap_or_else(|| "no".into())
            )),
            Ok(d) => {
                Err(format!("tipo {:?}, notas: {:?}", d.kind, d.notes.iter().map(|n| n.es.clone()).collect::<Vec<_>>()))
            }
            Err(e) => Err(e.to_string()),
        },
    );
    let endpoints = hint.as_ref().map(|h| h.autodiscover.clone()).unwrap_or_default();
    let found = iuredesk_accounts::discover_exchange(
        ANA,
        ANA,
        &ana_pw,
        &endpoints,
        hint.as_ref().and_then(|h| h.ews_url.as_deref()),
        ProbeOptions::default(),
    )
    .await;
    r.check(
        "Autodiscover con contraseña da una URL de EWS que funciona",
        found.as_ref().map(|s| s.host.clone()).map_err(|e| format!("{e:?}")),
    );
    let url = found.map(|s| s.host).unwrap_or_else(|_| "https://ex1.iuredesk.test/EWS/Exchange.asmx".into());
    // Sin la pista de v2 (Exchange antiguo): Autodiscover POX con la autenticación que haya
    // (Basic, NTLM o Kerberos según el modo del laboratorio).
    let pox = iuredesk_accounts::discover_exchange(ANA, ANA, &ana_pw, &endpoints, None, ProbeOptions::default()).await;
    r.check("Autodiscover POX (sin v2) con contraseña", pox.map(|s| s.host).map_err(|e| format!("{e:?}")));

    println!("2. Kerberos (sin NTLM de respaldo)");
    let host = reqwest::Url::parse(&url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
    let ticket =
        iuredesk_ews::kerberos::http_ticket(&iuredesk_ews::kerberos::TcpKdc::default(), ANA, &ana_pw, &host).await;
    r.check(
        "ticket de HTTP/servidor del KDC del dominio (AS-REQ y TGS-REQ)",
        ticket.as_ref().map(|t| format!("reino {}, cifrado {}", t.crealm, t.key.etype)).map_err(|e| e.to_string()),
    );
    if let Ok(t) = &ticket {
        r.check("EWS acepta solo el ticket de Kerberos (con channel binding)", kerberos_only(&url, t).await);
    }

    println!("3. Correo");
    let client =
        |email: &str, pw: &str| EwsClient::new(&url, Arc::new(BasicAuth::new(email, pw)), Duration::from_secs(60));
    let (ana, bea) = match (client(ANA, &ana_pw), client(BEA, &bea_pw)) {
        (Ok(a), Ok(b)) => (a, b),
        (a, b) => {
            eprintln!("no se pudo crear el cliente: {:?} {:?}", a.err(), b.err());
            std::process::exit(1);
        }
    };
    r.check(
        "Ana entra y ve sus carpetas",
        ana.sync_folder_hierarchy(None)
            .await
            .map(|h| format!("{} carpetas de correo", h.changed.iter().filter(|f| f.is_mail()).count()))
            .map_err(|e| e.to_string()),
    );
    let wrong = EwsClient::new(&url, Arc::new(BasicAuth::new(ANA, "no-es-la-contraseña")), Duration::from_secs(60));
    r.check(
        "una contraseña equivocada es «credencial rechazada»",
        match wrong {
            Ok(c) => match c.sync_folder_hierarchy(None).await {
                Err(iuredesk_ews::EwsError::Unauthorized) => Ok(String::new()),
                other => Err(format!("{other:?}")),
            },
            Err(e) => Err(e.to_string()),
        },
    );
    let subject = format!("Prueba F8.5 {stamp}");
    let mime = format!(
        "From: Ana <{ANA_MAIL}>\r\nTo: Bea <{BEA_MAIL}>\r\nSubject: {subject}\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHola Bea, esto viene de IureDesk.\r\n"
    );
    r.check(
        "Ana envía un correo a Bea",
        ana.create_from_mime(mime.as_bytes(), "sentitems", MessageDisposition::SendAndSaveCopy)
            .await
            .map(|_| String::new())
            .map_err(|e| e.to_string()),
    );
    let started = Instant::now();
    let mut got = Err("no llegó en 60 s".to_string());
    while started.elapsed() < Duration::from_secs(60) {
        if let Ok(ids) = bea.find_since("inbox", Utc::now() - chrono::Duration::minutes(10), 50).await {
            let ids: Vec<String> = ids.into_iter().map(|(i, _)| i).collect();
            if let Ok(items) = bea.get_summaries(&ids).await
                && items.iter().any(|i| i.subject == subject)
            {
                got = Ok(format!("en {} s", started.elapsed().as_secs()));
                break;
            }
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    r.check("Bea lo recibe", got);

    println!("4. Calendario, contactos, tareas (sincronización PIM)");
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let store = match Store::open(dir.path()) {
        Ok(s) => Arc::new(s),
        Err(e) => panic!("{e}"),
    };
    let secrets = Arc::new(MemoryStore::default());
    let _ = secrets.set("ana-lab", SecretPurpose::Incoming, &ana_pw);
    let Some(pim) = PimAccount::from_account(&account("ana-lab", ANA_MAIL, ANA, &url)) else {
        eprintln!("la cuenta de Exchange no da PimAccount");
        std::process::exit(1);
    };
    let opts = PimSyncOptions {
        poll_interval: Duration::from_secs(3600),
        sync_on_start: false,
        user_tz: "America/Mexico_City".parse().unwrap_or(chrono_tz::UTC),
        ..Default::default()
    };
    let handle = PimSyncEngine::start(Arc::clone(&store), vec![pim], None, secrets, opts);
    r.check(
        "primera sincronización",
        handle.sync_now_wait("ana-lab").await.map(|_| String::new()).map_err(|e| e.to_string()),
    );
    let cal = store.calendars().ok().and_then(|c| c.into_iter().next());
    r.check("calendario de Ana", cal.as_ref().map(|c| c.name.clone()).ok_or_else(|| "sin calendarios".into()));
    let book = store.address_books().ok().and_then(|b| b.into_iter().next());
    let list = store.task_lists().ok().and_then(|l| l.into_iter().next());
    r.check(
        "libreta y lista de tareas",
        match (&book, &list) {
            (Some(b), Some(l)) => Ok(format!("{} / {}", b.name, l.name)),
            _ => Err(format!("libreta {:?}, lista {:?}", book.is_some(), list.is_some())),
        },
    );

    if let Some(cal) = &cal {
        let start =
            (Utc::now() + chrono::Duration::days(2)).date_naive().and_hms_opt(17, 0, 0).unwrap_or_default().and_utc();
        let meeting = Event {
            calendar: cal.id,
            summary: format!("Reunión F8.5 {stamp}"),
            location: "Sala de pruebas".into(),
            start: EventTime::DateTime { utc: start, tzid: Some("America/Mexico_City".into()) },
            end: EventTime::DateTime {
                utc: start + chrono::Duration::hours(1),
                tzid: Some("America/Mexico_City".into()),
            },
            organizer: Some(Mailbox::new("Ana", ANA_MAIL)),
            attendees: vec![Attendee {
                mailbox: Mailbox::new("Bea", BEA_MAIL),
                partstat: PartStat::NeedsAction,
                role: AttendeeRole::Required,
                rsvp: true,
            }],
            ..Default::default()
        };
        let saved = handle.save_event(&meeting);
        let _ = handle.sync_now_wait("ana-lab").await;
        r.check(
            "Ana crea una reunión con Bea (CreateItem con invitación)",
            match saved.ok().and_then(|id| store.get_event(id).ok().flatten()) {
                Some(e) if e.remote_id.is_some() => Ok(String::new()),
                Some(_) => Err("sin id remoto (no se subió)".into()),
                None => Err("no se guardó".into()),
            },
        );
        // La invitación llega al calendario de Bea (el asistente de calendario la pone provisional).
        let started = Instant::now();
        let mut seen = Err("no apareció en 90 s".to_string());
        while started.elapsed() < Duration::from_secs(90) {
            if let Ok(folders) = bea.calendar_folders().await
                && let Some(f) = folders.first()
                && let Ok((items, _)) = bea
                    .calendar_view(&f.id, start - chrono::Duration::hours(1), start + chrono::Duration::hours(2))
                    .await
                && !items.is_empty()
            {
                let ids: Vec<String> = items.into_iter().map(|i| i.id).collect();
                if let Ok(full) = bea.get_calendar_items(&ids).await
                    && full.iter().any(|e| {
                        e.child(iuredesk_ews::soap::T, "Subject").is_some_and(|s| s.text_trim().contains(&stamp))
                    })
                {
                    seen = Ok(format!("en {} s", started.elapsed().as_secs()));
                    break;
                }
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        r.check("la invitación llega al calendario de Bea", seen);
        let fb = handle
            .free_busy(
                cal.id,
                vec![BEA_MAIL.into()],
                start - chrono::Duration::hours(2),
                start + chrono::Duration::hours(3),
            )
            .await;
        r.check(
            "disponibilidad de Bea (GetUserAvailability)",
            match fb {
                Ok(v) => match v.first().and_then(|f| f.busy.clone()) {
                    Some(b) => Ok(format!("{} intervalo(s) ocupado(s)", b.len())),
                    None => Err("desconocida".into()),
                },
                Err(e) => Err(e.to_string()),
            },
        );
    }
    if let Some(book) = &book {
        let id = handle.save_contact(
            &Contact {
                address_book: book.id,
                full_name: format!("Contacto F8.5 {stamp}"),
                emails: vec![Labeled::new("", "contacto@ejemplo.test")],
                ..Default::default()
            },
            None,
        );
        let _ = handle.sync_now_wait("ana-lab").await;
        r.check(
            "contacto nuevo sube a Exchange",
            match id.ok().and_then(|i| store.get_contact(i).ok().flatten()) {
                Some(c) if c.remote_id.is_some() => Ok(String::new()),
                _ => Err("sin id remoto".into()),
            },
        );
    }
    if let Some(list) = &list {
        let id =
            handle.save_task(&Task { list: list.id, summary: format!("Tarea F8.5 {stamp}"), ..Default::default() });
        let _ = handle.sync_now_wait("ana-lab").await;
        r.check(
            "tarea nueva sube a Exchange",
            match id.ok().and_then(|i| store.get_task(i).ok().flatten()) {
                Some(t) if t.remote_id.is_some() => Ok(String::new()),
                _ => Err("sin id remoto".into()),
            },
        );
    }
    let found = handle.directory_search("bea", 10).await;
    r.check(
        "libreta global encuentra a Bea",
        if found.iter().any(|m| m.addr.eq_ignore_ascii_case(BEA_MAIL)) {
            Ok(format!("{} resultado(s)", found.len()))
        } else {
            Err(format!("{found:?}"))
        },
    );
    handle.shutdown().await;

    println!(
        "\n{} OK, {} FALLA{}",
        r.ok,
        r.failed.len(),
        if r.failed.is_empty() { String::new() } else { format!(": {}", r.failed.join("; ")) }
    );
    std::process::exit(if r.failed.is_empty() { 0 } else { 1 });
}
