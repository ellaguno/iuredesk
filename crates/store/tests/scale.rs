//! Guarda de regresión de rendimiento (R2.1): 4 cuentas × 50 000 mensajes.
//!
//! Se ejecuta en release (en depuración tantivy es decenas de veces más lento):
//! `cargo test --release -p iuredesk-store --test scale -- --ignored --nocapture`
//! (también `cargo xtask bench` y el paso «Banco de regresión» de CI).
//!
//! Los topes son generosos (≈ 10× lo medido en un equipo de escritorio, ver
//! `docs/rendimiento.md`) para no fallar por ruido en CI: atrapan regresiones de orden de
//! magnitud (una transacción por fila, un recorrido completo por consulta), no del 20 %.

use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use iuredesk_model::{FolderRole, MessageId};
use iuredesk_store::Store;
use iuredesk_store::sample::{Rng, ScaleSpec, populate_scale};

fn pct(v: &mut [Duration], p: f64) -> Duration {
    v.sort_unstable();
    let i = ((v.len() as f64 - 1.0) * p).round() as usize;
    v.get(i).copied().unwrap_or_default()
}

fn check(what: &str, got: Duration, max_ms: u64) {
    println!("{what}: {:.2} ms (tope {max_ms} ms)", got.as_secs_f64() * 1000.0);
    assert!(got <= Duration::from_millis(max_ms), "{what}: {got:?} > {max_ms} ms");
}

#[test]
#[ignore = "banco de 200 000 mensajes: en release, con --ignored"]
fn paging_grouping_and_search_at_200k() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let now = Utc.with_ymd_and_hms(2026, 10, 26, 12, 0, 0).unwrap();
    let t = Instant::now();
    let sum = populate_scale(&store, ScaleSpec::default(), now, |_| {}).unwrap();
    assert_eq!(sum.messages, 200_000);
    println!("generar 200 000 mensajes: {:.1} s", t.elapsed().as_secs_f64());

    let inboxes: Vec<_> =
        store.all_folders().unwrap().into_iter().filter(|f| f.role == FolderRole::Inbox).map(|f| f.id).collect();
    assert_eq!(inboxes.len(), 4);

    // Paginar: 100 sobres en desplazamientos al azar de una bandeja (lo que pide la lista).
    let mut rng = Rng::new(7);
    let mut lat = Vec::new();
    for i in 0..1_000 {
        let f = inboxes[i % inboxes.len()];
        let total = store.count(f).unwrap().total;
        let off = rng.below(total) as usize;
        let t = Instant::now();
        let page = store.list_envelopes(f, off, 100).unwrap();
        lat.push(t.elapsed());
        assert!(!page.is_empty());
    }
    check("página de 100 sobres p50", pct(&mut lat, 0.5), 20);
    check("página de 100 sobres p99", pct(&mut lat, 0.99), 60);

    // Bandeja unificada: mezclar por fecha y agrupar en conversaciones (en frío y luego).
    let t = Instant::now();
    let mut order: Vec<(u64, u64)> = Vec::new();
    for f in &inboxes {
        order.extend(store.list_order(*f).unwrap().iter().copied());
    }
    order.sort_unstable();
    let ids: Vec<MessageId> = order.iter().map(|&(_, m)| MessageId(m)).collect();
    check("orden de la bandeja unificada", t.elapsed(), 500);
    assert!(ids.len() > 100_000, "{}", ids.len());
    let t = Instant::now();
    let rows = store.group_by_thread(&ids).unwrap();
    check("agrupar conversaciones (en frío)", t.elapsed(), 2_000);
    assert!(rows.len() < ids.len() && rows.len() > ids.len() / 2, "{} de {}", rows.len(), ids.len());
    let t = Instant::now();
    assert_eq!(store.group_by_thread(&ids).unwrap(), rows);
    check("agrupar conversaciones (de nuevo)", t.elapsed(), 1_500);

    // Búsqueda: palabra, campo, frase y con filtros posteriores.
    for (what, q, max_ms) in [
        ("buscar palabra", "contrato", 300),
        ("buscar por campo", "from:garcia", 300),
        ("buscar frase", "\"factura proyecto\"", 400),
        ("buscar con filtros", "in:inbox is:unread contrato", 800),
        ("solo filtros", "is:unread", 800),
    ] {
        let mut v = Vec::new();
        for _ in 0..5 {
            let t = Instant::now();
            let r = store.search(q, 1_000).unwrap();
            v.push(t.elapsed());
            assert!(!r.is_empty(), "{q}");
        }
        check(&format!("{what} ({q}) p50"), pct(&mut v, 0.5), max_ms);
    }
}
