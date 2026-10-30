//! UX-5 en la sesión, sobre el buzón de ejemplo: cápsula de Iurefficient para el correo que
//! envía la propia instancia, pestaña «Iurefficient» (filtro transversal) e imágenes remotas
//! (modo y lista de permitidos por remitente y dominio).

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use iuredesk_accounts::MemoryStore;
use iuredesk_engine::iuredesk_rules::Category;
use iuredesk_engine::remote_images::RemoteImages;
use iuredesk_engine::{IureKind, OfflineTransport, Session, SessionConfig, Zone};
use iuredesk_model::{Mailbox, MessageId};
use iuredesk_store::{Store, sample};

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 21, 15, 0, 0).unwrap()
}

fn session(dir: &std::path::Path) -> Session {
    let store = Store::open(dir).unwrap();
    sample::populate(&store, sample::DEFAULT_SEED, now()).unwrap();
    let mut s = Session::new(SessionConfig {
        store: Arc::new(store),
        accounts: None,
        secrets: Arc::new(MemoryStore::default()),
        prefs_path: None,
        now: Some(now()),
        zone: Zone::utc(),
        sample: true,
        sync: None,
        transport: Some(Arc::new(OfflineTransport)),
    })
    .unwrap();
    s.set_conversations(false);
    settle(&mut s);
    s
}

fn settle(s: &mut Session) {
    let start = Instant::now();
    loop {
        s.poll();
        if s.is_idle() {
            s.poll();
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(30));
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Copia un mensaje de la bandeja con otro remitente y asunto (un aviso de la instancia).
fn put_notice(s: &mut Session, from: &str, subject: &str, mid: &str) -> MessageId {
    let base = s.rows()[0].id;
    let mut env = s.store().get_envelope(base).unwrap().unwrap();
    env.id = MessageId::UNASSIGNED;
    env.remote_id = Some(format!("ux5-{mid}"));
    env.message_id = Some(format!("<{mid}>"));
    env.from = vec![Mailbox { name: Some("Iurefficient".into()), addr: from.into() }];
    env.reply_to = Vec::new();
    env.subject = subject.into();
    env.flags.seen = false;
    let id = s.store().put_message(env, b"Subject: x\r\n\r\nhola").unwrap();
    s.rebuild_rows();
    settle(s);
    id
}

#[test]
fn mail_sent_by_the_instance_gets_the_capsule() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path());
    // El ejemplo usa la instancia `demo.iurefficient.com`.
    let a = put_notice(&mut s, "noreply@demo.iurefficient.com", "[Iurefficient] Ticket #7 asignado a ti", "t7@x");
    let b = s.iure_badge(a).unwrap();
    assert_eq!(b.kind, IureKind::Ticket);
    assert!(b.tip.es.contains("Ticket #7 asignado a ti"), "{:?}", b.tip);
    assert!(!b.tip.es.contains("[Iurefficient]"));
    // Solo la etiqueta del asunto, desde otro dominio.
    let c = put_notice(&mut s, "avisos@mailer.example", "[Iurefficient] Te mencionaron en ACME", "m1@x");
    assert_eq!(s.iure_badge(c).map(|b| b.kind), Some(IureKind::Notice));
    // Un correo cualquiera de otro dominio, sin etiqueta: sin cápsula.
    let d = put_notice(&mut s, "zz-nadie@desconocido.example", "Comida del viernes", "z1@x");
    assert_eq!(s.iure_badge(d), None);
}

#[test]
fn iurefficient_tab_is_first_and_cross_cutting() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path());
    let notice = put_notice(&mut s, "noreply@demo.iurefficient.com", "[Iurefficient] Nueva tarea: revisar", "n1@x");
    assert!(s.category_tabs_shown());
    assert!(s.iure_tab_shown());
    let (total, unread) = s.iure_tab_counts();
    assert!(total > 0 && unread > 0 && unread <= total, "{total} {unread}");
    let primary = s.rows().len();
    s.run("mail.tab.iurefficient").unwrap();
    settle(&mut s);
    assert!(s.iure_tab());
    let rows: Vec<MessageId> = s.rows().iter().map(|r| r.id).collect();
    assert_eq!(rows.len(), total);
    assert!(rows.contains(&notice));
    assert!(rows.iter().all(|id| s.iure_badge(*id).is_some()));
    // Los mensajes siguen también en su categoría: la pestaña junta varias.
    let cats: HashSet<Category> = rows.iter().filter_map(|id| s.category_cached(*id)).collect();
    let home = s.category_cached(notice).unwrap();
    s.run(&format!("mail.category:{}", home.key())).unwrap();
    settle(&mut s);
    assert!(!s.iure_tab());
    assert!(s.rows().iter().any(|r| r.id == notice), "el aviso sigue en {home:?}");
    assert!(!cats.is_empty());
    // «Siguiente pestaña» desde Iurefficient vuelve a Principal.
    s.run("mail.category:iurefficient").unwrap();
    settle(&mut s);
    assert!(s.iure_tab());
    s.run("mail.category").unwrap();
    settle(&mut s);
    assert_eq!((s.iure_tab(), s.category_tab()), (false, Category::Primary));
    assert_eq!(s.rows().len(), primary);
}

#[test]
fn remote_images_follow_mode_and_allowlist() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path());
    s.set_categories_enabled(false);
    settle(&mut s);
    let contacts: HashSet<String> =
        s.contacts().iter().flat_map(|c| c.emails.iter().map(|e| e.value.to_lowercase())).collect();
    let ids: Vec<MessageId> = s.rows().iter().map(|r| r.id).collect();
    let stranger = put_notice(&mut s, "boletin@tienda-lejana.example", "Ofertas de la semana", "s1@x");
    assert!(!contacts.contains("boletin@tienda-lejana.example"));
    let related = ids.into_iter().find(|id| s.iure_badge(*id).is_some()).unwrap();

    // Nunca (preguntar): nada se carga solo.
    assert_eq!(s.prefs.remote_images, RemoteImages::Ask);
    assert!(!s.remote_images_auto(stranger));
    assert!(!s.remote_images_auto(related));
    // Solo de confianza: el remitente con relación en Iurefficient sí; el desconocido no.
    s.set_remote_images(RemoteImages::Trusted);
    assert!(s.remote_images_auto(related));
    assert!(!s.remote_images_auto(stranger));
    // «Mostrar siempre de este remitente» vale en cualquier modo.
    s.set_remote_images(RemoteImages::Ask);
    let (addr, domain) = s.image_sender(stranger).unwrap();
    assert_eq!(domain.as_deref(), Some("tienda-lejana.example"));
    assert!(s.allow_images_sender(&addr).is_some());
    assert!(s.remote_images_auto(stranger));
    s.remove_image_sender(&addr);
    assert!(!s.remote_images_auto(stranger));
    // «…de este dominio».
    assert!(s.allow_images_domain("tienda-lejana.example").is_some());
    assert!(s.remote_images_auto(stranger));
    s.remove_image_domain("tienda-lejana.example");
    assert!(s.prefs.image_domains.is_empty());
    // Siempre.
    s.set_remote_images(RemoteImages::Always);
    assert!(s.remote_images_auto(stranger));
    // Basura: no se agrega.
    assert!(s.allow_images_domain("no es dominio").is_none());
}
