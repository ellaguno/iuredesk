//! F5.3 en la sesión, sobre el buzón de ejemplo: reglas «Aplicar a la carpeta» con deshacer,
//! posponer con deshacer, categorías de la bandeja, etiquetas e insignias de Iurefficient.

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use iuredesk_accounts::MemoryStore;
use iuredesk_engine::iuredesk_rules::{Action, Category, Condition, Field, Rule, TextOp};
use iuredesk_engine::{OfflineTransport, Outcome, Session, SessionConfig, Zone};
use iuredesk_model::MessageId;
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

#[test]
fn apply_rules_to_folder_and_undo() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path());
    s.set_categories_enabled(false);
    settle(&mut s);
    let target =
        s.rows().iter().map(|r| r.id).find(|id| !s.store().get_envelope(*id).unwrap().unwrap().flags.flagged).unwrap();
    let sender = s.store().get_envelope(target).unwrap().unwrap().from[0].addr.clone();
    s.save_rule(Rule {
        name: "Prueba".into(),
        conditions: vec![Condition::Text { field: Field::From, op: TextOp::Is, value: sender.clone() }],
        actions: vec![Action::Flag, Action::Label { name: "Clientes".into() }],
        ..Rule::default()
    })
    .unwrap();
    let inbox = s.store().get_envelope(target).unwrap().unwrap().folder;
    let out = s.apply_rules_to_folder(inbox, None).unwrap();
    assert!(matches!(out, Outcome::Notice(n) if n.undo));
    let env = s.store().get_envelope(target).unwrap().unwrap();
    assert!(env.flags.flagged);
    assert!(env.keywords.iter().any(|k| k == "Clientes"));
    let labels = s.message_labels(&env);
    assert!(labels.iter().any(|(n, _)| n == "Clientes"), "{labels:?}");
    s.run("undo").unwrap();
    let env = s.store().get_envelope(target).unwrap().unwrap();
    assert!(!env.flags.flagged, "deshacer quita la bandera");
    assert!(!env.keywords.iter().any(|k| k == "Clientes"), "y la etiqueta");
}

#[test]
fn snooze_hides_and_undo_restores() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path());
    s.set_categories_enabled(false);
    settle(&mut s);
    let id = s.rows()[0].id;
    s.set_selection(vec![id], Some(id));
    settle(&mut s);
    let out = s.run("mail.snooze:tomorrow").unwrap();
    assert!(matches!(out, Outcome::Notice(n) if n.undo));
    settle(&mut s);
    assert!(s.is_snoozed(id));
    assert!(!s.rows().iter().any(|r| r.id == id), "pospuesto: fuera de la bandeja");
    assert_eq!(s.snoozed().len(), 1);
    // Sin argumento: la interfaz pregunta la hora.
    s.set_selection(vec![s.rows()[0].id], Some(s.rows()[0].id));
    assert_eq!(s.run("mail.snooze").unwrap(), Outcome::Rules("snooze".into()));
    s.run("undo").unwrap();
    settle(&mut s);
    assert!(!s.is_snoozed(id));
    assert!(s.rows().iter().any(|r| r.id == id), "vuelve a la bandeja");
}

#[test]
fn categories_split_the_inbox() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path());
    assert!(s.category_tabs_shown());
    let counts = s.category_counts();
    let total: usize = counts.iter().map(|c| c.0).sum();
    assert!(total > 300, "{counts:?}");
    assert!(counts[0].0 > 0 && counts[1].0 > 0, "Principal y Promociones: {counts:?}");
    let primary = s.rows().len();
    s.run("mail.category:promotions").unwrap();
    settle(&mut s);
    assert_eq!(s.category_tab(), Category::Promotions);
    assert_eq!(s.rows().len(), counts[1].0);
    assert!(s.rows().len() < total && primary < total);
    // «Mover siempre a Principal» al remitente de una promoción.
    let id = s.rows()[0].id;
    s.set_selection(vec![id], Some(id));
    s.run("mail.always_category:primary").unwrap();
    settle(&mut s);
    assert!(!s.rows().iter().any(|r| r.id == id));
    assert_eq!(s.category_of(id).map(|c| c.0), Some(Category::Primary));
    // Sin categorías: una sola bandeja.
    s.set_categories_enabled(false);
    settle(&mut s);
    assert_eq!(s.rows().len(), total);
}

#[test]
fn iurefficient_badges_and_quick_filter() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path());
    s.set_categories_enabled(false);
    settle(&mut s);
    let all = s.rows().len();
    let ids: Vec<MessageId> = s.rows().iter().map(|r| r.id).collect();
    let with: Vec<MessageId> = ids.iter().copied().filter(|id| s.iure_badge(*id).is_some()).collect();
    assert!(!with.is_empty() && with.len() < all);
    let b = s.iure_badge(with[0]).unwrap();
    assert!(b.tip.es.contains(':'), "{:?}", b.tip);
    s.run("mail.iure_only").unwrap();
    settle(&mut s);
    assert_eq!(s.rows().len(), with.len());
    s.run("mail.iure_only").unwrap();
    settle(&mut s);
    assert_eq!(s.rows().len(), all);
}
