//! Banco de la sincronización inicial (R2.1): un servidor IMAP falso con N mensajes (50 000 por
//! omisión) y la cuenta recién agregada. Mide el tiempo hasta tener todo el historial, la CPU y la
//! memoria del proceso y, mientras tanto, la latencia de lo que lee la interfaz (una página de la
//! bandeja cada 16 ms) para ver si la sincronización la estorba.
//!
//! ```text
//! cargo run --release -p iuredesk-sync --example sync_bench -- [N]
//! ```
//!
//! El servidor falso corre en el mismo proceso: la CPU medida incluye la suya.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chrono::Utc;
use iuredesk_accounts::{MemoryStore, SecretPurpose, SecretStore};
use iuredesk_imap::testing::{FakeCaps, FakeImap, PASSWORD, USER, sample_message};
use iuredesk_model::FolderRole;
use iuredesk_model::account::{Account, AccountKind, AuthMethod, Protocol, Security, ServerConfig};
use iuredesk_store::Store;
use iuredesk_sync::{SyncEngine, SyncOptions};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

fn status_mb(field: &str) -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .unwrap_or_default()
        .lines()
        .find(|l| l.starts_with(field))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse::<f64>().ok())
        .map_or(0.0, |kb| kb / 1024.0)
}

/// Segundos de CPU (usuario + sistema) del proceso (Linux, ticks de 100 Hz).
fn cpu_secs() -> f64 {
    let s = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    // Los campos tras el nombre del proceso (que va entre paréntesis).
    let rest = s.rsplit_once(')').map_or("", |(_, r)| r);
    let f: Vec<&str> = rest.split_whitespace().collect();
    let tick = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
    // utime y stime son los campos 14 y 15 (índices 11 y 12 tras el paréntesis).
    (tick(11) + tick(12)) / 100.0
}

fn pct(v: &mut [Duration], p: f64) -> Duration {
    v.sort_unstable();
    let i = ((v.len() as f64 - 1.0) * p).round() as usize;
    v.get(i).copied().unwrap_or_default()
}

fn ms(d: Duration) -> String {
    format!("{:.2} ms", d.as_secs_f64() * 1000.0)
}

fn message(i: u32) -> Vec<u8> {
    let date = Utc::now().fixed_offset() - chrono::Duration::minutes(i64::from(i) * 30);
    let root = i - i % 4;
    let reply = !i.is_multiple_of(4);
    let subject = if reply { format!("Re: Asunto del hilo {root}") } else { format!("Asunto del hilo {root}") };
    let parent = format!("m{}@bench.example.com", i.saturating_sub(1));
    // 20 % con cuerpo largo (~6 KB), el resto ~400 B.
    let words = if i.is_multiple_of(5) { 900 } else { 60 };
    let body: String = (0..words).map(|k| ["contrato ", "factura ", "reunión ", "plazo ", "cliente "][k % 5]).collect();
    sample_message(
        &format!("Remitente {} <r{}@example.com>", i % 97, i % 97),
        "Yo <yo@example.org>",
        &subject,
        &format!("m{i}@bench.example.com"),
        reply.then_some(parent.as_str()),
        date,
        &body,
        i.is_multiple_of(40).then_some("factura.pdf"),
    )
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Res<()> {
    let n: u32 = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(50_000);
    let server = FakeImap::start(FakeCaps::default()).await?;
    let msgs = (0..n)
        .rev()
        .map(|i| {
            let date = Utc::now().fixed_offset() - chrono::Duration::minutes(i64::from(i) * 30);
            (message(i), if i % 3 == 0 { vec![] } else { vec!["\\Seen".to_string()] }, date)
        })
        .collect();
    server.deliver_many("INBOX", msgs);
    let rss_server = status_mb("VmRSS:");
    println!("servidor falso: {n} mensajes · RSS {rss_server:.0} MB");

    let dir = tempfile::tempdir()?;
    let store = Arc::new(Store::open(dir.path())?);
    let sv = |protocol| ServerConfig {
        protocol,
        host: "127.0.0.1".into(),
        port: server.port(),
        security: Security::Plain,
        auth: AuthMethod::Password,
        username: USER.into(),
    };
    let account = Account {
        id: "bench".into(),
        display_name: "Bench".into(),
        email: USER.into(),
        kind: AccountKind::Generic,
        incoming: sv(Protocol::Imap),
        outgoing: sv(Protocol::Smtp),
        caldav_url: None,
        carddav_url: None,
        identities: vec![],
    };
    let secrets = MemoryStore::default();
    secrets.set("bench", SecretPurpose::Incoming, PASSWORD)?;
    let opts = SyncOptions { poll_interval: Duration::from_secs(3600), idle: false, ..Default::default() };

    // Sonda de la interfaz: una página de 100 sobres cada 16 ms.
    let stop = Arc::new(AtomicBool::new(false));
    let probe = {
        let store = Arc::clone(&store);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let mut lat = Vec::new();
            let mut wlat = Vec::new();
            let mut tick = 0u32;
            while !stop.load(Ordering::Relaxed) {
                tick = tick.wrapping_add(1);
                let inbox = store.account_labels().ok().and_then(|l| l.first().map(|a| a.id)).and_then(|a| {
                    store.folders(a).ok()?.into_iter().find(|f| f.role == FolderRole::Inbox).map(|f| f.id)
                });
                if let Some(f) = inbox {
                    let t = Instant::now();
                    let page = store.list_envelopes(f, 0, 100).unwrap_or_default();
                    let _ = std::hint::black_box(store.count(f));
                    lat.push(t.elapsed());
                    // Cada ~250 ms, «marcar como leído/no leído» como lo hace la interfaz.
                    if tick.is_multiple_of(15)
                        && let Some(e) = page.first()
                    {
                        let mut flags = e.flags;
                        flags.seen = !flags.seen;
                        let t = Instant::now();
                        let _ = iuredesk_sync::ops::set_flags(&store, e.id, flags);
                        wlat.push(t.elapsed());
                    }
                }
                std::thread::sleep(Duration::from_millis(16));
            }
            (lat, wlat)
        })
    };

    let cpu0 = cpu_secs();
    let t = Instant::now();
    let handle = SyncEngine::start_with(Arc::clone(&store), vec![account], None, Arc::new(secrets), opts);
    handle.sync_now_wait("bench").await?;
    let first = t.elapsed();
    let inbox = {
        let a = store.account_labels()?.first().map(|a| a.id).ok_or("sin cuenta")?;
        store.folders(a)?.into_iter().find(|f| f.role == FolderRole::Inbox).map(|f| f.id).ok_or("sin INBOX")?
    };
    let after_first = store.count(inbox)?.total;
    let mut peak = 0f64;
    let deadline = Instant::now() + Duration::from_secs(900);
    while store.count(inbox)?.total < u64::from(n) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
        peak = peak.max(status_mb("VmRSS:"));
    }
    let full = t.elapsed();
    let cpu = cpu_secs() - cpu0;
    let total = store.count(inbox)?.total;
    stop.store(true, Ordering::Relaxed);
    let (mut lat, mut wlat) = probe.join().unwrap_or_default();
    let st = store.stats()?;
    println!("ventana inicial: {after_first} mensajes en {:.2} s", first.as_secs_f64());
    println!(
        "historial completo: {total}/{n} en {:.1} s ({:.0} msg/s) · CPU {cpu:.1} s ({:.0} % de un núcleo)",
        full.as_secs_f64(),
        f64::from(n) / full.as_secs_f64().max(0.001),
        cpu / full.as_secs_f64().max(0.001) * 100.0
    );
    println!(
        "RSS durante la sincronización: pico {peak:.0} MB (servidor falso incluido, {rss_server:.0} MB al empezar) · VmHWM {:.0} MB",
        status_mb("VmHWM:")
    );
    println!(
        "lectura de la UI durante la sincronización ({} muestras): p50 {} · p99 {} · máx {}",
        lat.len(),
        ms(pct(&mut lat, 0.5)),
        ms(pct(&mut lat, 0.99)),
        ms(pct(&mut lat, 1.0))
    );
    println!(
        "escritura de la UI (marcar leído) durante la sincronización ({} muestras): p50 {} · p99 {} · máx {}",
        wlat.len(),
        ms(pct(&mut wlat, 0.5)),
        ms(pct(&mut wlat, 0.99)),
        ms(pct(&mut wlat, 1.0))
    );
    println!(
        "almacén: redb {:.0} MB · blobs {:.0} MB · índice {:.0} MB · hilos {}",
        st.db_bytes as f64 / 1_048_576.0,
        st.blob_bytes as f64 / 1_048_576.0,
        st.search_bytes as f64 / 1_048_576.0,
        st.threads
    );
    handle.shutdown().await;
    Ok(())
}
