//! Captura sin pantalla de IureDesk con el buzón de ejemplo y un «ahora» fijo (determinista).
//!
//! `cargo run -p iuredesk-ui --example ui_shot -- /tmp/shot.png [--dark] [--density dense|compact|comfortable]
//!  [--module today|mail|calendar|contacts|notes|tasks|assistant|onboarding|provider] [--palette[=texto]]
//!  [--html] [--month] [--view day|agenda|week|work_week|month] [--editor] [--invite] [--compose] [--lang en|es] [--size 1440x900] [--ppp 1]
//!  [--ai reading|datascope|compose|triage|chat|minutes] [--import]
//!  [--native-decorations | --chrome adwaita|windows|macos] [--hc] [--crop-height N]`
//!
//! Por omisión la ventana lleva la barra de título integrada con botones al estilo Adwaita;
//! `--native-decorations` la muestra como queda con la barra del sistema (sin botones).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use egui::vec2;
use egui_kittest::Harness;
use iuredesk_accounts::MemoryStore;
use iuredesk_engine::{DensityPref, Module, Session, SessionConfig, ThemePref, Zone};
use iuredesk_store::{Store, sample};
use iuredesk_ui::{DeskApp, Options, WindowChrome};

/// Cuentas de ejemplo para `--settings`.
fn demo_accounts() -> Vec<iuredesk_model::account::Account> {
    use iuredesk_model::account::{Account, AccountKind, AuthMethod, OAuthProvider, Protocol, Security, ServerConfig};
    let server = |protocol, host: &str, port, auth: AuthMethod, user: &str| ServerConfig {
        protocol,
        host: host.into(),
        port,
        security: Security::Tls,
        auth,
        username: user.into(),
    };
    let google = AuthMethod::OAuth2 { provider: OAuthProvider::Google };
    let ms = AuthMethod::OAuth2 { provider: OAuthProvider::Microsoft };
    vec![
        Account {
            id: "gmail-laura".into(),
            display_name: "Laura Méndez".into(),
            email: "laura.mendez@gmail.com".into(),
            kind: AccountKind::Gmail,
            incoming: server(Protocol::Imap, "imap.gmail.com", 993, google, "laura.mendez@gmail.com"),
            outgoing: server(Protocol::Smtp, "smtp.gmail.com", 465, google, "laura.mendez@gmail.com"),
            caldav_url: None,
            carddav_url: None,
            identities: Vec::new(),
        },
        Account {
            id: "m365-laura".into(),
            display_name: "Laura Méndez".into(),
            email: "laura@outlook.com".into(),
            kind: AccountKind::Microsoft365,
            incoming: server(Protocol::Imap, "outlook.office365.com", 993, ms, "laura@outlook.com"),
            outgoing: server(Protocol::Smtp, "smtp.office365.com", 587, ms, "laura@outlook.com"),
            caldav_url: None,
            carddav_url: None,
            identities: Vec::new(),
        },
        Account {
            id: "imap-despacho".into(),
            display_name: "Despacho Méndez".into(),
            email: "laura@despacho.example.org".into(),
            kind: AccountKind::Generic,
            incoming: server(Protocol::Imap, "mail.despacho.example.org", 993, AuthMethod::Password, "laura"),
            outgoing: server(Protocol::Smtp, "mail.despacho.example.org", 465, AuthMethod::Password, "laura"),
            caldav_url: Some("https://dav.despacho.example.org/cal/".into()),
            carddav_url: None,
            identities: Vec::new(),
        },
    ]
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
            .or_else(|| args.iter().find_map(|a| a.strip_prefix(&format!("{name}="))).map(str::to_string))
    };
    let out = args
        .iter()
        .find(|a| a.ends_with(".png"))
        .cloned()
        .ok_or("uso: ui_shot RUTA.png [--dark] [--density …] [--module …] [--palette]")?;
    let module = flag("--module").unwrap_or_else(|| "mail".into());
    let dark = args.iter().any(|a| a == "--dark");
    let hc = args.iter().any(|a| a == "--hc");
    let density = flag("--density").and_then(|d| DensityPref::from_key(&d)).unwrap_or(DensityPref::Compact);
    if let Some(l) = flag("--lang") {
        iuredesk_ui::iurefficient_ui::lang::set_lang(iuredesk_ui::iurefficient_ui::lang::from_tag(&l));
        iuredesk_engine::lang::set_lang(iuredesk_engine::lang::from_tag(&l));
    } else {
        iuredesk_ui::iurefficient_ui::lang::set_lang(iuredesk_ui::iurefficient_ui::lang::Lang::Es);
        iuredesk_engine::lang::set_lang(iuredesk_engine::lang::Lang::Es);
    }
    let (w, h) = flag("--size")
        .and_then(|s| s.split_once('x').and_then(|(a, b)| Some((a.parse::<f32>().ok()?, b.parse::<f32>().ok()?))))
        .unwrap_or((1440.0, 900.0));
    let ppp = flag("--ppp").and_then(|p| p.parse::<f32>().ok()).unwrap_or(1.0).clamp(0.5, 3.0);

    // Miércoles 21 de octubre de 2026, 11:20 UTC.
    let now = Utc.with_ymd_and_hms(2026, 10, 21, 11, 20, 0).single().ok_or("fecha")?;
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let store = Store::open(dir.path()).map_err(|e| e.to_string())?;
    let onboarding = module == "onboarding" || module == "provider";
    if !onboarding {
        sample::populate(&store, sample::DEFAULT_SEED, now).map_err(|e| e.to_string())?;
    }
    // `--settings[=sección]`: tres cuentas de ejemplo (Gmail, Microsoft 365 y una IMAP con error).
    let settings =
        args.iter().find_map(|a| a.strip_prefix("--settings")).map(|s| s.trim_start_matches('=').to_string());
    let accounts = match &settings {
        Some(_) => {
            let st = iuredesk_accounts::AccountStore::at(dir.path().join("accounts.json"));
            for a in demo_accounts() {
                st.upsert(a).map_err(|e| e.to_string())?;
            }
            Some(st)
        }
        None => None,
    };
    let mut session = Session::new(SessionConfig {
        store: Arc::new(store),
        accounts,
        secrets: Arc::new(MemoryStore::default()),
        prefs_path: None,
        now: Some(now),
        zone: Zone::utc(),
        sample: true,
        sync: None,
        transport: Some(Arc::new(iuredesk_engine::OfflineTransport)),
    })
    .map_err(|e| e.text().en)?;
    session.prefs.theme = if hc {
        ThemePref::HighContrast
    } else if dark {
        ThemePref::Dark
    } else {
        ThemePref::Light
    };
    session.prefs.density = density;
    if let Some(f) = flag("--font") {
        session.prefs.font = f;
    }
    if let Some(p) = flag("--text-scale").and_then(|p| p.parse::<u32>().ok()) {
        session.prefs.text_scale_pct = p;
    }
    session.prefs.crisp_text = args.iter().any(|a| a == "--crisp");
    if settings.is_some() {
        use iuredesk_engine::AccountSync;
        session.demo_account_status("gmail-laura", AccountSync::Ready { at: now, backfill: false });
        session.demo_account_status(
            "m365-laura",
            AccountSync::Error {
                text: iuredesk_engine::lang::Text::new(
                    "Microsoft asks you to sign in again.",
                    "Microsoft pide volver a iniciar sesión.",
                ),
                reauth: true,
            },
        );
        session.demo_account_status(
            "imap-despacho",
            AccountSync::Error {
                text: iuredesk_engine::lang::Text::new(
                    "The server rejected the password.",
                    "El servidor rechazó la contraseña.",
                ),
                reauth: false,
            },
        );
    }
    session.module = match module.as_str() {
        "calendar" => Module::Calendar,
        "contacts" => Module::Contacts,
        "notes" => Module::Notes,
        "tasks" => Module::Tasks,
        "assistant" => Module::Assistant,
        "today" => Module::Today,
        _ => Module::Mail,
    };
    // Esperar a que la lista esté lista para elegir el mensaje que se muestra.
    let start = Instant::now();
    while !session.is_idle() && start.elapsed() < Duration::from_secs(30) {
        session.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
    session.poll();
    if session.module == Module::Mail && !onboarding {
        let html = args.iter().any(|a| a == "--html");
        let rows: Vec<_> = session.rows().iter().take(400).copied().collect();
        let invite = args.iter().any(|a| a == "--invite");
        let pick = rows
            .iter()
            .find(|r| {
                if invite {
                    return session.envelope(r.id).is_some_and(|e| e.subject.starts_with("Invitación:"));
                }
                session.envelope(r.id).is_some_and(|e| {
                    if html { e.subject.starts_with("Boletín") } else { e.subject.contains("241/2026") && r.count > 1 }
                })
            })
            .or(rows.get(2))
            .map(|r| r.id);
        // `--secure`: el mensaje elegido llega firmado y cifrado (PGP/MIME) con claves de prueba.
        if args.iter().any(|a| a == "--secure")
            && let (Some(id), Some(c)) = (pick, iuredesk_engine::security::get())
            && let (Ok(raw), Some(env)) = (session.store().get_raw(id), session.envelope(id).cloned())
            && let Some(from) = env.from.first().cloned()
        {
            let _ = c.keys.generate_pgp(from.display(), &from.addr);
            let req = iuredesk_engine::iuredesk_crypto::SealRequest { sign: true, encrypt: true, prefer_smime: false };
            if let Ok(sealed) = c.seal(&raw, std::slice::from_ref(&from.addr), req) {
                let _ = session.store().put_message(env, &sealed);
            }
        }
        if let Some(id) = pick {
            session.request_focus(id);
        }
    }
    let chrome = if args.iter().any(|a| a == "--native-decorations") {
        None
    } else {
        let name = flag("--chrome").unwrap_or_else(|| "adwaita".into());
        Some(WindowChrome::named(&name).ok_or(format!("--chrome desconocido: {name}"))?)
    };
    let mut app =
        DeskApp::new(session, Options { system_fonts: true, no_theme_watch: true, chrome, ..Default::default() });
    if args.iter().any(|a| a == "--compose") {
        // `--secure`: clave propia y de los destinatarios (Autocrypt mutuo) → «Cifrar» se enciende.
        if args.iter().any(|a| a == "--secure")
            && let Some(c) = iuredesk_engine::security::get()
        {
            use iuredesk_engine::iuredesk_crypto::{autocrypt::PreferEncrypt, keystore::KeySource, openpgp};
            if let Some((_, me)) = app.session.identities().first() {
                let _ = c.keys.generate_pgp(&me.name, &me.email);
            }
            for a in ["carmen.brown@example.net", "pablo@despacho.example.org"] {
                if let Ok(k) = openpgp::generate("", a, "x") {
                    let _ = c.keys.learn(a, &k.to_public_key(), KeySource::Autocrypt, PreferEncrypt::Mutual, chrono::Utc::now());
                }
            }
        }
        app.demo_compose();
    }
    if module == "provider" {
        app.demo_provider_step("laura.mendez@gmail.com");
    } else if module == "onboarding" {
        app.demo_email_step("laura@despacho.example.org", "Laura Méndez");
    }
    if args.iter().any(|a| a == "--month") {
        app.set_calendar_month();
    }
    if let Some(v) = flag("--view") {
        app.set_calendar_view(&v);
    }
    if args.iter().any(|a| a == "--editor") {
        app.demo_event_editor();
    }
    if !args.iter().any(|a| a == "--reminders") {
        app.dismiss_reminders();
    }
    if let Some(what) = flag("--ai") {
        app.demo_assistant(&what);
    }
    let import_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    if args.iter().any(|a| a == "--import") {
        use iuredesk_engine::iuredesk_import::{DetectedSource, SourceKind};
        let mbox = import_dir.path().join("Clientes 2025.mbox");
        std::fs::write(&mbox, "From a@example.com Mon Oct 20 10:00:00 2026\nSubject: Uno\n\nHola\n")
            .map_err(|e| e.to_string())?;
        let home = PathBuf::from("/home/laura");
        app.demo_import(
            vec![
                DetectedSource {
                    kind: SourceKind::ThunderbirdProfile,
                    path: home.join(".thunderbird/x1y2z3.default-release"),
                    label: "Thunderbird — default-release".into(),
                    bytes: 1_843_200_000,
                },
                DetectedSource {
                    kind: SourceKind::OutlookPst,
                    path: home.join("Documentos/Outlook/archivo2024.pst"),
                    label: "archivo2024.pst".into(),
                    bytes: 734_003_200,
                },
            ],
            vec![home.join(".thunderbird/x1y2z3.default-release"), mbox],
        );
    }
    if let Some(s) = &settings {
        let edit = args.iter().any(|a| a == "--edit-account").then_some("imap-despacho");
        app.demo_settings(s, edit);
    }
    // F5.3: reglas, posponer, etiquetas y «Solo Iurefficient».
    if args.iter().any(|a| a == "--labels") {
        app.demo_labels();
    }
    if let Some(r) = flag("--rules") {
        app.demo_rules(&r);
    }
    if args.iter().any(|a| a == "--snooze") {
        app.demo_snooze();
    }
    if args.iter().any(|a| a == "--iure-only") {
        app.session.set_iure_only(true);
    }
    if let Some(q) = args.iter().find_map(|a| a.strip_prefix("--palette")) {
        app.palette.open_with(q.strip_prefix('=').unwrap_or(""));
    }

    let mut harness = Harness::builder()
        .with_size(vec2(w, h))
        .with_pixels_per_point(ppp)
        .with_accessibility_check(false)
        .allow_missing_glyphs()
        .build_ui_state(
            |ui, app: &mut DeskApp| {
                let full = ui.ctx().content_rect();
                ui.scope_builder(egui::UiBuilder::new().max_rect(full), |ui| {
                    ui.set_clip_rect(full);
                    app.ui(ui);
                });
            },
            app,
        );
    // Hasta que no quede trabajo de fondo (cuerpos, conversaciones, HTML).
    let start = Instant::now();
    let mut quiet = 0;
    while start.elapsed() < Duration::from_secs(90) {
        harness.run_steps(1);
        if harness.state().is_settled() {
            quiet += 1;
            if quiet > 6 {
                break;
            }
        } else {
            quiet = 0;
            std::thread::sleep(Duration::from_millis(15));
        }
    }
    harness.run_steps(2);
    let mut image = harness.render()?;
    if let Some(ch) = flag("--crop-height").and_then(|v| v.parse::<u32>().ok()) {
        let (iw, ih) = image.dimensions();
        image = image::imageops::crop_imm(&image, 0, 0, iw, ch.clamp(1, ih)).to_image();
    }
    image.save(&out).map_err(|e| e.to_string())?;
    println!("{out}");
    Ok(())
}
