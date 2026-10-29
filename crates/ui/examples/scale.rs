//! Banco de rendimiento a escala real (R2.1): almacén de 4 cuentas × 50 000 mensajes, arranque,
//! memoria, desplazamiento de la bandeja unificada, búsqueda, conversaciones, cambio de carpeta y
//! boletín HTML. Sin pantalla (egui_kittest, sin pintar en GPU: mide el cuadro de la CPU).
//!
//! ```text
//! cargo run --release -p iuredesk-ui --example scale -- [--dir RUTA] [--accounts 4] [--per 50000]
//! cargo run --release -p iuredesk-ui --example scale -- --startup RUTA   # un arranque (proceso hijo)
//! ```
//!
//! O bien `cargo xtask bench`. Con `--dir` el buzón generado se reutiliza entre corridas.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeZone, Utc};
use egui::vec2;
use egui_kittest::Harness;
use iuredesk_engine::{FolderSel, Session, SessionConfig, Zone};
use iuredesk_model::{FolderRole, MessageId};
use iuredesk_store::Store;
use iuredesk_store::sample::{ScaleSpec, populate_scale};
use iuredesk_ui::{DeskApp, Options};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 26, 12, 0, 0).single().unwrap_or_default()
}

fn ms(d: Duration) -> String {
    format!("{:.2} ms", d.as_secs_f64() * 1000.0)
}

fn pct(v: &mut [Duration], p: f64) -> Duration {
    v.sort_unstable();
    let i = ((v.len() as f64 - 1.0) * p).round() as usize;
    v.get(i).copied().unwrap_or_default()
}

/// Memoria anónima (`RssAnon`: montículo, sin contar archivos mapeados) en MB.
fn anon_mb() -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .unwrap_or_default()
        .lines()
        .find(|l| l.starts_with("RssAnon:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse::<f64>().ok())
        .map_or(0.0, |kb| kb / 1024.0)
}

/// `VmRSS` y `VmHWM` en MB (Linux).
fn rss_mb() -> (f64, f64) {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let field = |name: &str| {
        s.lines()
            .find(|l| l.starts_with(name))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<f64>().ok())
            .map_or(0.0, |kb| kb / 1024.0)
    };
    (field("VmRSS:"), field("VmHWM:"))
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

/// Genera el buzón si no existe (marca `scale.done` con el tamaño).
fn ensure_store(dir: &Path, spec: ScaleSpec) -> Res<()> {
    let mark = dir.join("scale.done");
    let want = format!("{}x{}", spec.accounts, spec.per_account);
    if std::fs::read_to_string(&mark).is_ok_and(|s| s.trim() == want) {
        println!("buzón reutilizado: {} ({want})", dir.display());
        return Ok(());
    }
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    let store = Store::open(dir.join("store"))?;
    let t = Instant::now();
    let total = spec.accounts * spec.per_account;
    populate_scale(&store, spec, now(), |n| {
        if n % 20_000 == 0 {
            eprint!("\r  generando {n}/{total}");
        }
    })?;
    eprintln!();
    let st = store.stats()?;
    println!(
        "generado: {} mensajes, {} hilos en {:.1} s · redb {:.0} MB · blobs {:.0} MB · índice {:.0} MB",
        st.messages,
        st.threads,
        t.elapsed().as_secs_f64(),
        st.db_bytes as f64 / 1_048_576.0,
        st.blob_bytes as f64 / 1_048_576.0,
        st.search_bytes as f64 / 1_048_576.0
    );
    drop(store);
    std::fs::write(&mark, want)?;
    Ok(())
}

fn harness(app: DeskApp) -> Harness<'static, DeskApp> {
    Harness::builder()
        .with_size(vec2(1440.0, 900.0))
        .with_pixels_per_point(1.0)
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
        )
}

/// Abre el almacén, la sesión y la app; devuelve también lo que tardó cada paso.
fn open_app(dir: &Path) -> Res<(DeskApp, [Duration; 3])> {
    let t = Instant::now();
    let store = Arc::new(Store::open(dir.join("store"))?);
    let t_store = t.elapsed();
    let mut cfg = SessionConfig::offline(store);
    cfg.now = Some(now());
    cfg.zone = Zone::utc();
    let t = Instant::now();
    let mut session = Session::new(cfg).map_err(|e| e.text().en)?;
    session.select_folder(FolderSel::Unified(FolderRole::Inbox));
    let t_session = t.elapsed();
    let t = Instant::now();
    let app = DeskApp::new(session, Options { system_fonts: true, no_theme_watch: true, ..Default::default() });
    Ok((app, [t_store, t_session, t.elapsed()]))
}

/// Cuadros hasta que la sesión no tenga trabajo de fondo (máx. `limit`).
fn settle(h: &mut Harness<'static, DeskApp>, limit: Duration) -> Duration {
    let t = Instant::now();
    let mut quiet = 0;
    while t.elapsed() < limit {
        h.step();
        if h.state().is_settled() && !h.state().session.is_loading() {
            quiet += 1;
            if quiet > 2 {
                break;
            }
        } else {
            quiet = 0;
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    t.elapsed()
}

/// Un arranque en frío del proceso: abre el almacén, crea la sesión y la app, primer cuadro y
/// lista lista. Imprime una línea `startup <primer cuadro ms> <lista ms> <rss MB>`.
fn startup(dir: &Path, t0: Instant) -> Res<()> {
    let (app, steps) = open_app(dir)?;
    let t = Instant::now();
    let mut h = harness(app);
    h.step();
    let frame = t.elapsed();
    let first = t0.elapsed();
    // Lista lista: las conversaciones ya agrupadas (lo demás de fondo puede seguir).
    while h.state().session.is_loading() && t0.elapsed() < Duration::from_secs(60) {
        h.step();
        std::thread::sleep(Duration::from_millis(1));
    }
    let ready = t0.elapsed();
    settle(&mut h, Duration::from_secs(60));
    eprintln!("todo en reposo: {:.1} ms", t0.elapsed().as_secs_f64() * 1e3);
    let (rss, _) = rss_mb();
    let [a, b, c] = steps.map(|d| d.as_secs_f64() * 1e3);
    println!(
        "startup {:.1} {:.1} {:.1} {:.1} {a:.1} {b:.1} {c:.1} {:.1}",
        first.as_secs_f64() * 1e3,
        ready.as_secs_f64() * 1e3,
        rss,
        anon_mb(),
        frame.as_secs_f64() * 1e3
    );
    Ok(())
}

fn main() -> Res<()> {
    let t0 = Instant::now();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(d) = arg(&args, "--startup") {
        return startup(Path::new(&d), t0);
    }
    let spec = ScaleSpec {
        accounts: arg(&args, "--accounts").and_then(|v| v.parse().ok()).unwrap_or(4),
        per_account: arg(&args, "--per").and_then(|v| v.parse().ok()).unwrap_or(50_000),
        ..Default::default()
    };
    let dir = arg(&args, "--dir")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(format!("iuredesk-scale-{}x{}", spec.accounts, spec.per_account)));
    ensure_store(&dir, spec)?;

    // 1. Arranque en frío (5 procesos hijos; caché de páginas del SO ya caliente).
    let exe = std::env::current_exe()?;
    let mut firsts = Vec::new();
    let mut readies = Vec::new();
    let mut rsss = Vec::new();
    let mut parts: Vec<[f64; 5]> = Vec::new();
    for _ in 0..5 {
        let out = std::process::Command::new(&exe).arg("--startup").arg(&dir).output()?;
        let text = String::from_utf8_lossy(&out.stdout);
        if let Some(line) = text.lines().find(|l| l.starts_with("startup ")) {
            let v: Vec<f64> = line.split_whitespace().skip(1).filter_map(|x| x.parse().ok()).collect();
            if let [a, b, c, d, e, f, g, i] = v.as_slice() {
                firsts.push(Duration::from_secs_f64(a / 1e3));
                readies.push(Duration::from_secs_f64(b / 1e3));
                rsss.push(*c);
                parts.push([*d, *e, *f, *g, *i]);
            }
        } else {
            eprintln!("arranque fallido: {}", String::from_utf8_lossy(&out.stderr));
        }
    }
    rsss.sort_by(f64::total_cmp);
    println!(
        "arranque: primer cuadro p50 {} · lista lista p50 {} · RSS tras arrancar p50 {:.0} MB",
        ms(pct(&mut firsts, 0.5)),
        ms(pct(&mut readies, 0.5)),
        rsss.get(rsss.len() / 2).copied().unwrap_or(0.0)
    );
    if let Some([anon, st, se, ap, fr]) = parts.get(parts.len() / 2) {
        println!(
            "  desglose: abrir almacén {st:.1} ms · sesión {se:.1} ms · app {ap:.1} ms · primer cuadro {fr:.1} ms · RssAnon {anon:.0} MB"
        );
    }

    // 2. En este proceso: arranque, desplazamiento y lo demás.
    let mut h = harness(open_app(&dir)?.0);
    h.step();
    settle(&mut h, Duration::from_secs(60));
    let (rss0, _) = rss_mb();
    let anon0 = anon_mb();
    let rows = h.state().session.rows().len();
    let conv = h.state().session.prefs.conversations;
    println!(
        "bandeja unificada: {rows} filas ({}) · RSS {rss0:.0} MB (anónima {anon0:.0} MB)",
        if conv { "conversaciones" } else { "mensajes" }
    );

    // Desplazamiento de arriba abajo, ~1 pantalla por cuadro.
    let mut frames = Vec::new();
    let step = 30usize;
    let mut i = 0usize;
    let t = Instant::now();
    while i < rows {
        h.state_mut().demo_scroll_mail(i);
        let f = Instant::now();
        h.step();
        frames.push(f.elapsed());
        i += step;
    }
    let scroll_total = t.elapsed();
    let (rss1, hwm1) = rss_mb();
    let anon1 = anon_mb();
    let n = frames.len();
    println!(
        "desplazar {n} cuadros en {:.2} s: cuadro p50 {} · p99 {} · máx {} · RSS {rss1:.0} MB (anónima {anon1:.0} MB, pico {hwm1:.0} MB)",
        scroll_total.as_secs_f64(),
        ms(pct(&mut frames, 0.5)),
        ms(pct(&mut frames, 0.99)),
        ms(pct(&mut frames, 1.0))
    );

    // Cuadros en reposo (sin cambios).
    let mut idle = Vec::new();
    for _ in 0..120 {
        let f = Instant::now();
        h.step();
        idle.push(f.elapsed());
    }
    println!("cuadro en reposo: p50 {} · p99 {}", ms(pct(&mut idle, 0.5)), ms(pct(&mut idle, 0.99)));

    // Refresco de la lista como tras cada tanda de la sincronización: lo que cuesta en el hilo de la
    // interfaz y hasta tener las conversaciones de nuevo.
    let mut rf_ui = Vec::new();
    let mut rf = Vec::new();
    for _ in 0..8 {
        let t = Instant::now();
        h.state_mut().session.rebuild_rows();
        rf_ui.push(t.elapsed());
        while h.state().session.is_loading() && t.elapsed() < Duration::from_secs(30) {
            h.step();
            std::thread::sleep(Duration::from_millis(1));
        }
        rf.push(t.elapsed());
    }
    println!(
        "recalcular la lista: hilo de la UI p50 {} · máx {} · conversaciones listas p50 {}",
        ms(pct(&mut rf_ui, 0.5)),
        ms(pct(&mut rf_ui, 1.0)),
        ms(pct(&mut rf, 0.5))
    );

    // 3. Búsqueda (en el almacén, como la hace la sesión en segundo plano).
    let store = Arc::clone(h.state().session.store());
    for (kind, qs) in [
        ("simple", &["contrato", "factura", "reunión"][..]),
        ("campo", &["from:garcia", "subject:presupuesto", "has:attachment informe"][..]),
        ("frase", &["\"proyecto cliente\"", "\"factura proyecto\""][..]),
        ("con filtro", &["in:inbox is:unread contrato", "in:archive", "is:unread"][..]),
    ] {
        let mut v = Vec::new();
        let mut hits = 0;
        for q in qs {
            for _ in 0..10 {
                let t = Instant::now();
                hits += store.search(q, 1_000)?.len();
                v.push(t.elapsed());
            }
        }
        println!(
            "buscar ({kind}): p50 {} · p99 {} · {} res/consulta",
            ms(pct(&mut v, 0.5)),
            ms(pct(&mut v, 0.99)),
            hits / v.len().max(1)
        );
    }
    // Búsqueda de punta a punta desde la UI (escribir → filas).
    let t = Instant::now();
    h.state_mut().session.set_search("contrato");
    settle(&mut h, Duration::from_secs(30));
    println!("buscar desde la UI («contrato» → {} filas): {}", h.state().session.rows().len(), ms(t.elapsed()));
    h.state_mut().session.set_search("");
    settle(&mut h, Duration::from_secs(30));

    // 4. Agrupar en conversaciones toda la bandeja unificada.
    let inboxes: Vec<_> =
        store.all_folders()?.into_iter().filter(|f| f.role == FolderRole::Inbox).map(|f| f.id).collect();
    let mut order: Vec<(u64, u64)> = Vec::new();
    for f in &inboxes {
        order.extend(store.list_order(*f)?.iter().copied());
    }
    order.sort_unstable();
    let ids: Vec<MessageId> = order.iter().map(|&(_, m)| MessageId(m)).collect();
    let mut g = Vec::new();
    let mut groups = 0;
    for _ in 0..4 {
        let t = Instant::now();
        groups = iuredesk_engine::session::group_conversations(&store, &ids).len();
        g.push(t.elapsed());
    }
    let first_group = g.first().copied().unwrap_or_default();
    println!(
        "agrupar {} mensajes → {groups} conversaciones: primera vez {} · luego p50 {}",
        ids.len(),
        ms(first_group),
        ms(pct(g.get_mut(1..).unwrap_or(&mut []), 0.5))
    );

    // 5. Cambiar de carpeta (selección → lista lista), con el hilo de la UI medido aparte.
    let folders = store.all_folders()?;
    let targets: Vec<FolderSel> = folders
        .iter()
        .filter(|f| matches!(f.role, FolderRole::Archive | FolderRole::Sent | FolderRole::Custom))
        .take(6)
        .map(|f| FolderSel::Folder(f.id))
        .chain([FolderSel::Unified(FolderRole::Archive), FolderSel::Unified(FolderRole::Inbox)])
        .collect();
    let mut sw = Vec::new();
    let mut sw_ui = Vec::new();
    for sel in &targets {
        let t = Instant::now();
        h.state_mut().session.select_folder(*sel);
        sw_ui.push(t.elapsed());
        settle(&mut h, Duration::from_secs(30));
        sw.push(t.elapsed());
    }
    println!(
        "cambiar de carpeta: hilo de la UI p50 {} · máx {} · lista lista p50 {} · máx {}",
        ms(pct(&mut sw_ui, 0.5)),
        ms(pct(&mut sw_ui, 1.0)),
        ms(pct(&mut sw, 0.5)),
        ms(pct(&mut sw, 1.0))
    );

    // 6. Boletín HTML largo: abrir (render) y volver a abrir (caché).
    let news = folders.iter().find(|f| f.name == "Newsletters").map(|f| f.id);
    if let Some(nf) = news {
        let envs = store.list_envelopes(nf, 0, 200)?;
        let with_body: Vec<MessageId> = envs.iter().filter(|e| e.blob.is_some()).map(|e| e.id).take(2).collect();
        if let [a, b] = with_body.as_slice() {
            h.state_mut().session.select_folder(FolderSel::Folder(nf));
            settle(&mut h, Duration::from_secs(30));
            // Devuelve (hasta verlo pintado, cuadro más lento del hilo de la UI mientras tanto).
            let open = |h: &mut Harness<'static, DeskApp>, id: MessageId| {
                let t = Instant::now();
                h.state_mut().session.set_selection(vec![id], Some(id));
                let mut worst = Duration::ZERO;
                let mut quiet = 0;
                while t.elapsed() < Duration::from_secs(30) {
                    let f = Instant::now();
                    h.step();
                    worst = worst.max(f.elapsed());
                    if h.state().is_settled() {
                        quiet += 1;
                        if quiet > 2 {
                            break;
                        }
                    } else {
                        quiet = 0;
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
                (t.elapsed(), worst)
            };
            let first = open(&mut h, *a);
            let other = open(&mut h, *b);
            let again = open(&mut h, *a);
            let (rss2, _) = rss_mb();
            println!(
                "boletín HTML: abrir {} (cuadro más lento {}) · otro {} · reabrir (caché) {} · RSS {rss2:.0} MB",
                ms(first.0),
                ms(first.1),
                ms(other.0),
                ms(again.0)
            );
        }
    }
    let (rss3, hwm3) = rss_mb();
    println!("memoria final: RSS {rss3:.0} MB (anónima {:.0} MB) · pico {hwm3:.0} MB", anon_mb());
    h.state_mut().shutdown();
    Ok(())
}
