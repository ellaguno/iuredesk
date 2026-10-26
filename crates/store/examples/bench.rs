//! Banco de pruebas del almacén.
//!
//! `cargo run --release -p iuredesk-store --example bench -- 100000`
//!
//! Inserta N mensajes sintéticos (lotes de 1000) y mide: rendimiento de inserción, latencia de
//! `list_envelopes` (páginas de 100 en desplazamientos aleatorios), latencia de búsqueda, tamaño
//! en disco y memoria residente. Objetivos: página < 2 ms, búsqueda < 50 ms.

use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use iuredesk_model::{AccountId, Folder, FolderRole};
use iuredesk_store::Store;
use iuredesk_store::sample::{Rng, synthetic_messages};

fn pct(v: &mut [Duration], p: f64) -> Duration {
    v.sort_unstable();
    let i = ((v.len() as f64 - 1.0) * p).round() as usize;
    v.get(i).copied().unwrap_or_default()
}

fn ms(d: Duration) -> String {
    format!("{:.3} ms", d.as_secs_f64() * 1000.0)
}

fn rss() -> String {
    std::fs::read_to_string("/proc/self/status")
        .map(|s| {
            s.lines()
                .filter(|l| l.starts_with("VmRSS") || l.starts_with("VmHWM"))
                .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_else(|_| "n/d".into())
}

fn mb(b: u64) -> String {
    format!("{:.1} MB", b as f64 / 1_048_576.0)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let n: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(100_000);
    // `--no-raw`: sin blobs (para separar el coste de la compresión).
    let no_raw = std::env::args().any(|a| a == "--no-raw");
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path())?;
    let mk = |name: &str, role| Folder {
        account: AccountId(1),
        name: name.into(),
        remote_path: name.into(),
        role,
        ..Default::default()
    };
    let inbox = store.put_folder(&mk("INBOX", FolderRole::Inbox))?;
    let others = [
        store.put_folder(&mk("Sent", FolderRole::Sent))?,
        store.put_folder(&mk("Archive", FolderRole::Archive))?,
        store.put_folder(&mk("Clientes", FolderRole::Custom))?,
    ];
    // 60 % a INBOX.
    let weights = [inbox, inbox, inbox, others[0], others[1], others[2]];
    let now = Utc.with_ymd_and_hms(2026, 10, 26, 12, 0, 0).single().unwrap_or_default();
    let mut rng = Rng::new(42);

    println!("== iuredesk-store bench: {n} mensajes en {}", dir.path().display());
    let batch = 1_000usize;
    let mut gen_time = Duration::ZERO;
    let t = Instant::now();
    let mut done = 0usize;
    while done < n {
        let k = batch.min(n - done);
        let g = Instant::now();
        let mut msgs = synthetic_messages(&mut rng, &weights, k, now);
        if no_raw {
            for m in &mut msgs {
                m.raw = None;
            }
        }
        gen_time += g.elapsed();
        store.put_messages(msgs)?;
        done += k;
    }
    let total = t.elapsed() - gen_time;
    println!(
        "insertar: {done} en {:.2} s → {:.0} msg/s (sin contar la generación, {:.2} s)",
        total.as_secs_f64(),
        done as f64 / total.as_secs_f64(),
        gen_time.as_secs_f64()
    );
    println!("memoria tras insertar: {}", rss());

    let count = store.count(inbox)?;
    println!("INBOX: {} mensajes, {} no leídos", count.total, count.unread);

    // Primera página: construye la caché de la carpeta.
    let t = Instant::now();
    let first = store.list_envelopes(inbox, 0, 100)?;
    println!("primera página (construye la caché): {} ({} filas)", ms(t.elapsed()), first.len());

    let pages = 2_000;
    let mut lat = Vec::with_capacity(pages);
    let total_inbox = count.total.max(1);
    for _ in 0..pages {
        let off = rng.below(total_inbox) as usize;
        let t = Instant::now();
        let page = store.list_envelopes(inbox, off, 100)?;
        lat.push(t.elapsed());
        std::hint::black_box(page);
    }
    println!(
        "list_envelopes(100) desplazamiento aleatorio: p50 {} · p99 {}",
        ms(pct(&mut lat, 0.5)),
        ms(pct(&mut lat, 0.99))
    );

    let queries = [
        "contrato",
        "factura octubre",
        "presu",
        "from:garcia",
        "has:attachment informe",
        "in:inbox is:unread reunión",
        "\"proyecto cliente\"",
        "subject:budget after:2026-06-01",
        "in:archive",
        "is:unread",
    ];
    let mut all = Vec::new();
    for q in queries {
        let mut v = Vec::new();
        let mut hits = 0;
        for _ in 0..20 {
            let t = Instant::now();
            let r = store.search(q, 100)?;
            v.push(t.elapsed());
            hits = r.len();
        }
        all.extend(v.iter().copied());
        println!("buscar {q:<34} {hits:>3} res · p50 {} · p99 {}", ms(pct(&mut v, 0.5)), ms(pct(&mut v, 0.99)));
    }
    println!("búsqueda global: p50 {} · p99 {}", ms(pct(&mut all, 0.5)), ms(pct(&mut all, 0.99)));

    let st = store.stats()?;
    println!(
        "disco: redb {} · blobs {} · índice {} · total {}",
        mb(st.db_bytes),
        mb(st.blob_bytes),
        mb(st.search_bytes),
        mb(st.db_bytes + st.blob_bytes + st.search_bytes)
    );
    println!("mensajes {} · hilos {} · indexados {}", st.messages, st.threads, st.indexed);
    // Segunda ronda de páginas, ya sin fusiones de tantivy en segundo plano.
    let mut lat = Vec::with_capacity(pages);
    for _ in 0..pages {
        let off = rng.below(total_inbox) as usize;
        let t = Instant::now();
        std::hint::black_box(store.list_envelopes(inbox, off, 100)?);
        lat.push(t.elapsed());
    }
    println!(
        "list_envelopes(100), 2ª ronda: p50 {} · p90 {} · p99 {} · máx {}",
        ms(pct(&mut lat, 0.5)),
        ms(pct(&mut lat, 0.9)),
        ms(pct(&mut lat, 0.99)),
        ms(pct(&mut lat, 1.0))
    );
    // Misma página repetida (todo en caché): mide el coste puro de leer y decodificar 100 sobres.
    let mut lat = Vec::with_capacity(pages);
    for _ in 0..pages {
        let t = Instant::now();
        std::hint::black_box(store.list_envelopes(inbox, 1_000, 100)?);
        lat.push(t.elapsed());
    }
    println!("list_envelopes(100), página fija: p50 {} · p99 {}", ms(pct(&mut lat, 0.5)), ms(pct(&mut lat, 0.99)));
    let mut store = store;
    let t = Instant::now();
    store.compact()?;
    println!("compactar redb: {} → {}", ms(t.elapsed()), mb(store.stats()?.db_bytes));

    println!("memoria final: {}", rss());
    Ok(())
}
