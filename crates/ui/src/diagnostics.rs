//! Ajustes → Avanzado → «Rendimiento» (R2.1): cuánto tarda cada cuadro, cuánta memoria usa la
//! app, tamaño del almacén, operaciones pendientes y última sincronización de cada cuenta. Sirve
//! para que el dueño reporte cómo se comporta con sus cuentas reales («Copiar informe»).
//!
//! Nada de esto bloquea: el tamaño del almacén recorre carpetas en disco y se calcula en segundo
//! plano; la memoria se lee como mucho una vez por segundo.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use egui::{RichText, Ui};
use iuredesk_engine::{AccountSync, JobId, JobOutput};
use iuredesk_store::StoreStats;
use iurefficient_ui::{lang, pick, section_label, theme};

use crate::app::DeskApp;

/// Cuadros recordados para las estadísticas.
const FRAMES: usize = 240;
/// Cada cuánto se vuelve a leer la memoria del proceso.
const MEM_EVERY: Duration = Duration::from_secs(1);

/// Tiempo de los últimos cuadros (trabajo de la app en cada uno) y separación entre ellos.
#[derive(Default)]
pub struct FrameStats {
    work_ms: VecDeque<f32>,
    gap_ms: VecDeque<f32>,
    last: Option<Instant>,
}

/// Resumen de [`FrameStats`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameSummary {
    pub p50_ms: f32,
    pub p99_ms: f32,
    pub max_ms: f32,
    /// Cuadros por segundo mientras hay actividad (la app no pinta si nada cambia).
    pub fps: f32,
    pub samples: usize,
}

impl FrameStats {
    /// Registra un cuadro que costó `work` y empezó en `start`.
    pub fn record(&mut self, start: Instant, work: Duration) {
        if let Some(prev) = self.last {
            let gap = start.saturating_duration_since(prev).as_secs_f32() * 1000.0;
            // Una pausa larga es reposo (nada que pintar), no un cuadro lento.
            if gap < 250.0 {
                push(&mut self.gap_ms, gap);
            }
        }
        self.last = Some(start);
        push(&mut self.work_ms, work.as_secs_f32() * 1000.0);
    }

    pub fn summary(&self) -> FrameSummary {
        let mut v: Vec<f32> = self.work_ms.iter().copied().collect();
        v.sort_by(f32::total_cmp);
        let at = |p: f32| {
            let i = ((v.len().saturating_sub(1)) as f32 * p).round() as usize;
            v.get(i).copied().unwrap_or(0.0)
        };
        let gaps: f32 = self.gap_ms.iter().sum();
        let fps = if gaps > 0.0 { self.gap_ms.len() as f32 * 1000.0 / gaps } else { 0.0 };
        FrameSummary { p50_ms: at(0.5), p99_ms: at(0.99), max_ms: at(1.0), fps, samples: v.len() }
    }
}

fn push(q: &mut VecDeque<f32>, v: f32) {
    if q.len() >= FRAMES {
        q.pop_front();
    }
    q.push_back(v);
}

/// Memoria del proceso en bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemInfo {
    /// Residente total (incluye archivos mapeados, como el índice de búsqueda).
    pub rss: u64,
    /// Residente anónima (montículo de la app).
    pub anon: u64,
    /// Pico residente.
    pub peak: u64,
}

/// Lee la memoria del proceso: `/proc/self/status` en Linux (residente, anónima y pico); en
/// Windows y macOS solo la residente, por las llamadas del sistema (R1.2). `None` si no se puede.
pub fn process_memory() -> Option<MemInfo> {
    match std::fs::read_to_string("/proc/self/status") {
        Ok(s) => Some(parse_status(&s)),
        Err(_) => iuredesk_desktop::memory::resident_bytes().map(|rss| MemInfo { rss, anon: 0, peak: 0 }),
    }
}

/// Interpreta `/proc/self/status` (separado para probarlo).
pub fn parse_status(s: &str) -> MemInfo {
    let kb = |name: &str| -> u64 {
        s.lines()
            .find(|l| l.starts_with(name))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<u64>().ok())
            .map_or(0, |k| k.saturating_mul(1024))
    };
    MemInfo { rss: kb("VmRSS:"), anon: kb("RssAnon:"), peak: kb("VmHWM:") }
}

/// Estadísticas del almacén calculadas en segundo plano.
pub struct StoreReport(pub Result<StoreStats, String>);

/// Estado del panel.
#[derive(Default)]
pub struct DiagState {
    pub frames: FrameStats,
    mem: Option<(Instant, Option<MemInfo>)>,
    store: Option<(Instant, Result<StoreStats, String>)>,
    job: Option<JobId>,
}

impl DiagState {
    fn memory(&mut self) -> Option<MemInfo> {
        match self.mem {
            Some((at, m)) if at.elapsed() < MEM_EVERY => m,
            _ => {
                let m = process_memory();
                self.mem = Some((Instant::now(), m));
                m
            }
        }
    }

    /// Llegaron las estadísticas del almacén.
    pub fn on_store(&mut self, job: JobId, r: StoreReport) {
        if self.job == Some(job) {
            self.job = None;
        }
        self.store = Some((Instant::now(), r.0));
    }
}

fn mb(b: u64) -> String {
    format!("{:.1} MB", b as f64 / 1_048_576.0)
}

/// Pide las estadísticas del almacén (recorre el disco: en segundo plano).
fn request_store(app: &mut DeskApp) {
    if app.diag.job.is_some() {
        return;
    }
    let store = std::sync::Arc::clone(app.session.store());
    let job = app
        .session
        .spawn_blocking(move || JobOutput::Custom(Box::new(StoreReport(store.stats().map_err(|e| e.to_string())))));
    app.diag.job = Some(job);
}

/// Texto del informe (lo que se ve y lo que copia «Copiar informe»).
pub fn report(app: &mut DeskApp) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = Vec::new();
    let f = app.diag.frames.summary();
    rows.push((
        pick("Frame time", "Tiempo por cuadro").into(),
        if f.samples == 0 {
            "—".into()
        } else {
            format!(
                "p50 {:.1} ms · p99 {:.1} ms · {} {:.1} ms · {:.0} fps",
                f.p50_ms,
                f.p99_ms,
                pick("max", "máx"),
                f.max_ms,
                f.fps
            )
        },
    ));
    let mem = app.diag.memory();
    rows.push((
        pick("Memory", "Memoria").into(),
        match mem {
            // Windows y macOS: solo la residente.
            Some(m) if m.anon == 0 && m.peak == 0 => mb(m.rss),
            Some(m) => format!(
                "{} ({} {}) · {} {}",
                mb(m.rss),
                mb(m.anon),
                pick("app", "de la app"),
                pick("peak", "pico"),
                mb(m.peak)
            ),
            None => pick("Not available on this system", "No disponible en este sistema").into(),
        },
    ));
    rows.push((
        pick("Message list", "Lista de mensajes").into(),
        format!("{} {}", app.session.rows().len(), pick("rows", "filas")),
    ));
    rows.push((
        pick("Rendered HTML", "HTML pintado").into(),
        format!(
            "{} · {} / {}",
            app.html.renders(),
            mb(app.html.texture_bytes() as u64),
            mb(crate::html::MAX_TEXTURE_BYTES as u64)
        ),
    ));
    match &app.diag.store {
        Some((_, Ok(st))) => {
            rows.push((
                pick("Local store", "Almacén local").into(),
                format!(
                    "{} {} · {} {} · {} ({} {}, {} {}, {} {})",
                    st.messages,
                    pick("messages", "mensajes"),
                    st.threads,
                    pick("conversations", "conversaciones"),
                    mb(st.db_bytes + st.blob_bytes + st.search_bytes),
                    pick("database", "base"),
                    mb(st.db_bytes),
                    pick("bodies", "cuerpos"),
                    mb(st.blob_bytes),
                    pick("search", "búsqueda"),
                    mb(st.search_bytes)
                ),
            ));
            rows.push((
                pick("Pending changes", "Cambios por enviar").into(),
                format!("{} {}", st.queued_ops, pick("in the sync queue", "en la cola de sincronización")),
            ));
        }
        Some((_, Err(e))) => rows.push((pick("Local store", "Almacén local").into(), e.clone())),
        None => rows.push((pick("Local store", "Almacén local").into(), pick("Measuring…", "Midiendo…").into())),
    }
    let now = app.session.now();
    let accounts = app.session.accounts_overview();
    if accounts.is_empty() {
        rows.push((pick("Sync", "Sincronización").into(), pick("No accounts", "Sin cuentas").into()));
    }
    for a in accounts {
        let state = match &a.status {
            Some(AccountSync::Ready { at, .. }) => {
                let mins = (now - *at).num_minutes().max(0);
                let ago = if mins < 1 {
                    pick("just now", "hace un momento").to_string()
                } else if mins < 120 {
                    if lang::lang() == lang::Lang::En { format!("{mins} min ago") } else { format!("hace {mins} min") }
                } else {
                    at.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string()
                };
                format!("{} · {ago}", pick("Up to date", "Al día"))
            }
            Some(s) => s.label(),
            None => pick("Not synced yet", "Aún sin sincronizar").into(),
        };
        rows.push((a.email.clone(), state));
    }
    rows
}

/// La sección «Avanzado» de los ajustes.
pub fn section(app: &mut DeskApp, ui: &mut Ui) {
    let c = theme::colors(ui);
    // Las estadísticas del almacén se renuevan cada 10 s mientras el panel está a la vista.
    if app.diag.store.as_ref().is_none_or(|(at, _)| at.elapsed() > Duration::from_secs(10)) {
        request_store(app);
    }
    section_label(ui, pick("Performance", "Rendimiento"));
    ui.label(
        RichText::new(pick(
            "How IureDesk is doing on this computer. If something feels slow, copy this report and send it with your description.",
            "Cómo va IureDesk en esta computadora. Si algo se siente lento, copia este informe y envíalo junto con lo que pasó.",
        ))
        .color(c.text_muted)
        .size(theme::density(ui.ctx()).small_size()),
    );
    ui.add_space(8.0);
    let rows = report(app);
    egui::Grid::new("perf-grid").num_columns(2).spacing([16.0, 6.0]).striped(true).show(ui, |ui| {
        for (k, v) in &rows {
            ui.label(RichText::new(k).color(c.text_muted));
            ui.label(RichText::new(v).color(c.text));
            ui.end_row();
        }
    });
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        if ui.button(pick("Copy report", "Copiar informe")).clicked() {
            let mut text = format!("IureDesk {}\n", env!("CARGO_PKG_VERSION"));
            text.push_str(&format!("{} {}\n", std::env::consts::OS, std::env::consts::ARCH));
            for (k, v) in &rows {
                text.push_str(&format!("{k}: {v}\n"));
            }
            ui.ctx().copy_text(text);
        }
        if ui.button(pick("Refresh", "Actualizar")).clicked() {
            app.diag.store = None;
            request_store(app);
        }
    });
    // Mientras se ve, se repinta para que el tiempo por cuadro y la memoria estén al día.
    ui.ctx().request_repaint_after(Duration::from_millis(500));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_summary() {
        let mut f = FrameStats::default();
        let t0 = Instant::now();
        for i in 0..100u64 {
            f.record(t0 + Duration::from_millis(i * 16), Duration::from_millis(if i == 50 { 40 } else { 2 }));
        }
        let s = f.summary();
        assert_eq!(s.samples, 100);
        assert!((s.p50_ms - 2.0).abs() < 0.01);
        assert!((s.max_ms - 40.0).abs() < 0.01);
        assert!((s.fps - 62.5).abs() < 0.5, "{}", s.fps);
        // Solo se guardan los últimos cuadros.
        for i in 0..1_000u64 {
            f.record(t0 + Duration::from_secs(10) + Duration::from_millis(i * 16), Duration::from_millis(1));
        }
        assert_eq!(f.summary().samples, FRAMES);
        assert!((f.summary().max_ms - 1.0).abs() < 0.01);
    }

    #[test]
    fn parses_proc_status() {
        let s = "Name:\tiuredesk\nVmHWM:\t  204800 kB\nVmRSS:\t  102400 kB\nRssAnon:\t   76800 kB\n";
        let m = parse_status(s);
        assert_eq!(m.rss, 100 * 1024 * 1024);
        assert_eq!(m.anon, 75 * 1024 * 1024);
        assert_eq!(m.peak, 200 * 1024 * 1024);
        assert_eq!(parse_status("basura"), MemInfo::default());
    }
}
