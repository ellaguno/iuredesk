//! S1.36: reproductor de audio de IureDesk (capa L3), para las noticias habladas de «Mi día».
//!
//! - **Decodificar**: `symphonia` (Rust puro): MP3, y WAV para las pruebas.
//! - **Sonar**: en Linux el PCM va por un tubo a la herramienta de sonido del sistema (`pw-cat`
//!   de PipeWire, si no `pacat` de PulseAudio, si no `aplay` de ALSA), como IureTranscribe para
//!   grabar; así no se enlaza `alsa-sys` (C). En Windows y macOS, `cpal` (WASAPI y CoreAudio por
//!   sus enlaces en Rust).
//!
//! Un hilo por reproducción; la UI solo manda órdenes (pausa, salto, alto) y lee un [`Status`].
//! Nunca bloquea al que llama y nunca entra en pánico: lo que falla queda en [`State::Failed`].
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::{FormatOptions, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;

#[cfg(not(target_os = "linux"))]
mod device;
#[cfg(target_os = "linux")]
mod process;
mod resample;

/// Paquetes como mucho por archivo (un MP3 de horas tiene decenas de miles).
const MAX_PACKETS: u64 = 5_000_000;
/// Cada cuánto mira el hilo las órdenes mientras está en pausa.
const IDLE: Duration = Duration::from_millis(40);

/// Por qué no suena.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AudioError {
    #[error("no se pudo abrir el archivo de audio")]
    Open,
    #[error("formato de audio no reconocido")]
    Format,
    #[error("no hay salida de sonido disponible")]
    NoOutput,
}

/// En qué está el reproductor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum State {
    #[default]
    Idle,
    Playing,
    Paused,
    /// Terminó el archivo.
    Ended,
    Failed(AudioError),
}

/// Lo que la UI pinta.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Status {
    pub state: State,
    /// Segundos reproducidos.
    pub position: f64,
    /// Duración, si el archivo la dice.
    pub duration: Option<f64>,
}

/// Salida de sonido (PCM entero de 16 bits, entrelazado).
pub(crate) trait Sink {
    /// Escribe; espera si la salida va llena. Vuelve antes si `ctl` pide parar.
    fn write(&mut self, samples: &[i16], ctl: &Control) -> Result<(), AudioError>;
    fn set_paused(&mut self, paused: bool);
    /// Tras un salto: tira lo que aún no sonó (si se puede).
    fn flush(&mut self);
    /// Fin del archivo: espera a que suene lo que queda (o a que `ctl` pida parar).
    fn finish(&mut self, ctl: &Control);
}

/// Abre la salida para `rate` Hz y `channels` canales.
pub(crate) type SinkFactory = Arc<dyn Fn(u32, u16) -> Result<Box<dyn Sink>, AudioError> + Send + Sync>;

/// Órdenes y estado compartidos con el hilo.
#[derive(Default)]
pub(crate) struct Control {
    stop: AtomicBool,
    paused: AtomicBool,
    seek: Mutex<Option<f64>>,
    status: Mutex<Status>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

impl Control {
    pub(crate) fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    fn set(&self, f: impl FnOnce(&mut Status)) {
        f(&mut lock(&self.status));
    }
}

struct Playback {
    ctl: Arc<Control>,
    thread: Option<JoinHandle<()>>,
}

impl Playback {
    fn stop(&mut self) {
        self.ctl.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// El reproductor: un archivo a la vez.
pub struct Player {
    current: Option<Playback>,
    factory: SinkFactory,
}

impl Default for Player {
    fn default() -> Self {
        Player::new()
    }
}

impl Player {
    /// Con la salida de sonido del sistema.
    pub fn new() -> Player {
        Player { current: None, factory: system_sink() }
    }

    #[cfg(test)]
    pub(crate) fn with_sink(factory: SinkFactory) -> Player {
        Player { current: None, factory }
    }

    /// Empieza a sonar `path` (para lo que sonaba antes).
    pub fn play(&mut self, path: PathBuf) {
        self.stop();
        let ctl = Arc::new(Control::default());
        ctl.set(|s| s.state = State::Playing);
        let factory = Arc::clone(&self.factory);
        let worker = Arc::clone(&ctl);
        let thread = std::thread::Builder::new()
            .name("iuredesk-audio".into())
            .spawn(move || {
                if let Err(e) = run(&path, &worker, &factory) {
                    worker.set(|s| s.state = State::Failed(e));
                }
            })
            .ok();
        if thread.is_none() {
            ctl.set(|s| s.state = State::Failed(AudioError::NoOutput));
        }
        self.current = Some(Playback { ctl, thread });
    }

    /// Pausa o sigue.
    pub fn set_paused(&mut self, paused: bool) {
        if let Some(p) = &self.current {
            let st = lock(&p.ctl.status).state;
            if matches!(st, State::Playing | State::Paused) {
                p.ctl.paused.store(paused, Ordering::Relaxed);
                p.ctl.set(|s| s.state = if paused { State::Paused } else { State::Playing });
            }
        }
    }

    /// Pausa si suena; sigue si estaba en pausa.
    pub fn toggle_pause(&mut self) {
        let st = self.status().state;
        self.set_paused(st == State::Playing);
    }

    /// Salta a `secs` segundos.
    pub fn seek(&mut self, secs: f64) {
        if let Some(p) = &self.current
            && secs.is_finite()
        {
            *lock(&p.ctl.seek) = Some(secs.max(0.0));
            p.ctl.set(|s| s.position = secs.max(0.0));
        }
    }

    /// Para y suelta la salida.
    pub fn stop(&mut self) {
        if let Some(mut p) = self.current.take() {
            p.stop();
        }
    }

    /// Estado actual.
    pub fn status(&self) -> Status {
        self.current.as_ref().map(|p| *lock(&p.ctl.status)).unwrap_or_default()
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop();
    }
}

fn system_sink() -> SinkFactory {
    #[cfg(target_os = "linux")]
    {
        Arc::new(|rate, channels| process::ProcessSink::open(rate, channels).map(|s| Box::new(s) as Box<dyn Sink>))
    }
    #[cfg(not(target_os = "linux"))]
    {
        Arc::new(|rate, channels| device::DeviceSink::open(rate, channels).map(|s| Box::new(s) as Box<dyn Sink>))
    }
}

/// El hilo: decodifica paquete a paquete y escribe en la salida, atento a las órdenes.
fn run(path: &Path, ctl: &Control, factory: &SinkFactory) -> Result<(), AudioError> {
    let file = std::fs::File::open(path).map_err(|_| AudioError::Open)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|_| AudioError::Format)?;
    let mut format = probed.format;
    let track = format.tracks().iter().find(|t| t.codec_params.codec != CODEC_TYPE_NULL).ok_or(AudioError::Format)?;
    let track_id = track.id;
    let params = track.codec_params.clone();
    let time_base = params.time_base;
    let duration =
        params.n_frames.zip(params.sample_rate).filter(|(_, r)| *r > 0).map(|(n, r)| n as f64 / f64::from(r));
    ctl.set(|s| s.duration = duration);
    let mut decoder =
        symphonia::default::get_codecs().make(&params, &DecoderOptions::default()).map_err(|_| AudioError::Format)?;
    let mut sink: Option<Box<dyn Sink>> = None;
    let mut paused = false;
    for _ in 0..MAX_PACKETS {
        if ctl.stopping() {
            return Ok(());
        }
        let want_pause = ctl.paused.load(Ordering::Relaxed);
        if want_pause != paused {
            paused = want_pause;
            if let Some(s) = sink.as_mut() {
                s.set_paused(paused);
            }
        }
        if let Some(to) = lock(&ctl.seek).take() {
            let at = Time::new(to.trunc() as u64, to.fract());
            if format.seek(SeekMode::Coarse, SeekTo::Time { time: at, track_id: Some(track_id) }).is_ok() {
                decoder.reset();
                if let Some(s) = sink.as_mut() {
                    s.flush();
                }
            }
        }
        if paused {
            std::thread::sleep(IDLE);
            continue;
        }
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::ResetRequired) => {
                decoder.reset();
                continue;
            }
            // Fin del archivo (o uno truncado: suena lo que hubo).
            Err(_) => break,
        };
        if packet.track_id() != track_id {
            continue;
        }
        if let Some(tb) = time_base {
            let t = tb.calc_time(packet.ts());
            ctl.set(|s| s.position = t.seconds as f64 + t.frac);
        }
        let buf = match decoder.decode(&packet) {
            Ok(b) => b,
            // Un paquete dañado se salta; lo demás sigue.
            Err(SymError::DecodeError(_) | SymError::IoError(_)) => continue,
            Err(_) => return Err(AudioError::Format),
        };
        let spec = *buf.spec();
        if buf.frames() == 0 {
            continue;
        }
        let mut samples = SampleBuffer::<i16>::new(buf.capacity() as u64, spec);
        samples.copy_interleaved_ref(buf);
        if sink.is_none() {
            let channels = u16::try_from(spec.channels.count()).unwrap_or(2).max(1);
            sink = Some(factory(spec.rate, channels)?);
        }
        if let Some(s) = sink.as_mut() {
            s.write(samples.samples(), ctl)?;
        }
    }
    if let Some(s) = sink.as_mut() {
        s.finish(ctl);
    }
    if !ctl.stopping() {
        ctl.set(|s| {
            s.state = State::Ended;
            if let Some(d) = s.duration {
                s.position = d;
            }
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Salida en memoria: guarda lo escrito y cuántas veces se pausó o se vació.
    #[derive(Default)]
    struct Mem {
        written: usize,
        flushes: usize,
        finished: bool,
        rate: u32,
        channels: u16,
    }

    struct MemSink(Arc<Mutex<Mem>>, Duration);

    impl Sink for MemSink {
        fn write(&mut self, samples: &[i16], _ctl: &Control) -> Result<(), AudioError> {
            lock(&self.0).written += samples.len();
            std::thread::sleep(self.1);
            Ok(())
        }
        fn set_paused(&mut self, _paused: bool) {}
        fn flush(&mut self) {
            lock(&self.0).flushes += 1;
        }
        fn finish(&mut self, _ctl: &Control) {
            lock(&self.0).finished = true;
        }
    }

    fn player(mem: &Arc<Mutex<Mem>>, delay: Duration) -> Player {
        let m = Arc::clone(mem);
        Player::with_sink(Arc::new(move |rate, channels| {
            {
                let mut g = lock(&m);
                g.rate = rate;
                g.channels = channels;
            }
            Ok(Box::new(MemSink(Arc::clone(&m), delay)) as Box<dyn Sink>)
        }))
    }

    /// WAV PCM de 16 bits: `secs` segundos de un tono a `rate` Hz, `channels` canales.
    fn wav(dir: &Path, secs: u32, rate: u32, channels: u16) -> PathBuf {
        let frames = secs * rate;
        let data_len = frames * u32::from(channels) * 2;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data_len).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&channels.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
        b.extend_from_slice(&(channels * 2).to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data_len.to_le_bytes());
        for i in 0..frames {
            let v = ((i as f32 * 0.05).sin() * 8000.0) as i16;
            for _ in 0..channels {
                b.extend_from_slice(&v.to_le_bytes());
            }
        }
        let p = dir.join("tono.wav");
        std::fs::write(&p, b).unwrap();
        p
    }

    fn wait(p: &Player, f: impl Fn(&Status) -> bool) -> Status {
        for _ in 0..500 {
            let s = p.status();
            if f(&s) {
                return s;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        p.status()
    }

    #[test]
    fn plays_a_file_to_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let path = wav(dir.path(), 2, 16_000, 1);
        let mem = Arc::new(Mutex::new(Mem::default()));
        let mut p = player(&mem, Duration::ZERO);
        p.play(path);
        let s = wait(&p, |s| s.state == State::Ended);
        assert_eq!(s.state, State::Ended);
        assert_eq!(s.duration, Some(2.0));
        let m = lock(&mem);
        assert_eq!((m.rate, m.channels), (16_000, 1));
        assert_eq!(m.written, 32_000, "todas las muestras");
        assert!(m.finished);
    }

    #[test]
    fn pause_seek_and_stop() {
        let dir = tempfile::tempdir().unwrap();
        let path = wav(dir.path(), 30, 8_000, 2);
        let mem = Arc::new(Mutex::new(Mem::default()));
        // Escribir tarda: da tiempo de pausar a la mitad.
        let mut p = player(&mem, Duration::from_millis(5));
        p.play(path);
        wait(&p, |s| s.position > 0.0);
        p.toggle_pause();
        assert_eq!(p.status().state, State::Paused);
        std::thread::sleep(Duration::from_millis(80));
        let before = lock(&mem).written;
        std::thread::sleep(Duration::from_millis(120));
        assert!(lock(&mem).written <= before + 4_096, "en pausa no se escribe");
        p.seek(20.0);
        p.toggle_pause();
        // La posición pedida se ve al instante; el hilo salta en su siguiente vuelta.
        assert!(p.status().position >= 20.0);
        let mem2 = Arc::clone(&mem);
        wait(&p, move |_| lock(&mem2).flushes >= 1);
        assert!(lock(&mem).flushes >= 1, "el salto vacía la salida");
        let s = wait(&p, |s| s.position > 20.0);
        assert!(s.position > 20.0 && s.state == State::Playing, "{s:?}");
        p.stop();
        assert_eq!(p.status().state, State::Idle);
        assert!(!lock(&mem).finished, "parar no espera al final");
    }

    #[test]
    fn bad_files_fail_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let mem = Arc::new(Mutex::new(Mem::default()));
        let mut p = player(&mem, Duration::ZERO);
        p.play(dir.path().join("no-existe.mp3"));
        assert_eq!(wait(&p, |s| matches!(s.state, State::Failed(_))).state, State::Failed(AudioError::Open));
        let junk = dir.path().join("basura.mp3");
        std::fs::write(&junk, b"<html>no es audio</html>").unwrap();
        p.play(junk);
        assert_eq!(wait(&p, |s| matches!(s.state, State::Failed(_))).state, State::Failed(AudioError::Format));
    }

    /// Un MP3 real de punta a punta (sin sonar): `IUREDESK_TEST_MP3=/ruta.mp3 cargo test -p
    /// iuredesk-audio -- --ignored real_mp3`.
    #[test]
    #[ignore]
    fn real_mp3_decodes_to_the_end() {
        let Ok(path) = std::env::var("IUREDESK_TEST_MP3") else { return };
        let mem = Arc::new(Mutex::new(Mem::default()));
        let mut p = player(&mem, Duration::ZERO);
        p.play(PathBuf::from(path));
        let s = wait(&p, |s| matches!(s.state, State::Ended | State::Failed(_)));
        let m = lock(&mem);
        eprintln!("{s:?} · {} Hz × {} · {} muestras", m.rate, m.channels, m.written);
        assert_eq!(s.state, State::Ended);
        assert!(m.written > 0);
    }

    #[test]
    fn no_output_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = wav(dir.path(), 1, 8_000, 1);
        let mut p = Player::with_sink(Arc::new(|_, _| Err(AudioError::NoOutput)));
        p.play(path);
        assert_eq!(wait(&p, |s| matches!(s.state, State::Failed(_))).state, State::Failed(AudioError::NoOutput));
    }
}
