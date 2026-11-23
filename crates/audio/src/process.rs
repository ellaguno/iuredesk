//! Linux: el PCM va por un tubo a la herramienta de sonido del sistema. Se prueba `pw-cat`
//! (PipeWire), luego `pacat` (PulseAudio) y luego `aplay` (ALSA): la primera que arranca y sigue
//! viva se queda. Muestras de 16 bits en little endian (todas las plataformas Linux que se
//! distribuyen lo son).

use std::io::Write;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use crate::{AudioError, Control, Sink};

/// Bytes por escritura: así se mira `stop` a menudo.
const CHUNK: usize = 8 * 1024;
/// Si la herramienta muere en este tiempo (sin servidor de sonido), se prueba la siguiente.
const SETTLE: Duration = Duration::from_millis(150);
/// Al terminar, espera como mucho esto a que suene lo que queda en el tubo.
const DRAIN: Duration = Duration::from_secs(10);

/// Herramientas y sus argumentos para PCM crudo de `rate` Hz y `channels` canales.
pub(crate) fn candidates(rate: u32, channels: u16) -> Vec<(&'static str, Vec<String>)> {
    let r = rate.to_string();
    let c = channels.to_string();
    let v = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    vec![
        ("pw-cat", v(&["--playback", "--raw", "--format", "s16", "--rate", &r, "--channels", &c, "-"])),
        ("pacat", v(&["--playback", "--format=s16le", &format!("--rate={r}"), &format!("--channels={c}")])),
        ("aplay", v(&["-q", "-t", "raw", "-f", "S16_LE", "-r", &r, "-c", &c, "-"])),
    ]
}

pub(crate) struct ProcessSink {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl ProcessSink {
    pub(crate) fn open(rate: u32, channels: u16) -> Result<ProcessSink, AudioError> {
        for (prog, args) in candidates(rate, channels) {
            let Ok(mut child) = Command::new(prog)
                .args(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            else {
                continue;
            };
            std::thread::sleep(SETTLE);
            if !matches!(child.try_wait(), Ok(None)) {
                let _ = child.wait();
                continue;
            }
            let stdin = child.stdin.take();
            if stdin.is_some() {
                return Ok(ProcessSink { child, stdin });
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        Err(AudioError::NoOutput)
    }
}

impl Sink for ProcessSink {
    fn write(&mut self, samples: &[i16], ctl: &Control) -> Result<(), AudioError> {
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let Some(stdin) = self.stdin.as_mut() else { return Err(AudioError::NoOutput) };
        for chunk in bytes.chunks(CHUNK) {
            if ctl.stopping() {
                return Ok(());
            }
            // La herramienta murió (se quitó el dispositivo, se cerró el servidor).
            stdin.write_all(chunk).map_err(|_| AudioError::NoOutput)?;
        }
        Ok(())
    }

    // En pausa simplemente no se escribe: la herramienta toca lo que tiene y calla.
    fn set_paused(&mut self, _paused: bool) {}

    // Lo que ya está en el tubo suena (una fracción de segundo).
    fn flush(&mut self) {}

    fn finish(&mut self, ctl: &Control) {
        // Cerrar la entrada: la herramienta toca lo que queda y termina.
        self.stdin = None;
        let start = Instant::now();
        while start.elapsed() < DRAIN && !ctl.stopping() {
            if !matches!(self.child.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    }
}

impl Drop for ProcessSink {
    fn drop(&mut self) {
        self.stdin = None;
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_pcm_arguments_for_each_tool() {
        let c = candidates(24_000, 1);
        let names: Vec<&str> = c.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["pw-cat", "pacat", "aplay"], "PipeWire primero");
        let pw = &c[0].1;
        assert!(pw.contains(&"--raw".to_string()) && pw.last().map(String::as_str) == Some("-"));
        assert!(pw.windows(2).any(|w| w[0] == "--rate" && w[1] == "24000"));
        assert!(c[1].1.contains(&"--channels=1".to_string()));
        assert!(c[2].1.windows(2).any(|w| w[0] == "-f" && w[1] == "S16_LE"));
    }
}
