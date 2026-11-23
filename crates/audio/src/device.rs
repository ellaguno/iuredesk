//! Windows y macOS: la salida por omisión del sistema con `cpal` (WASAPI, CoreAudio). La tarjeta
//! pide muestras desde su propio hilo; aquí se le deja una cola con ~½ s convertida a su formato.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};

use crate::resample::Resampler;
use crate::{AudioError, Control, Sink, lock};

/// Segundos que se adelantan en la cola.
const AHEAD: f64 = 0.5;
/// Al terminar, espera como mucho esto a que la cola se vacíe.
const DRAIN: Duration = Duration::from_secs(5);

type Queue = Arc<Mutex<VecDeque<f32>>>;

pub(crate) struct DeviceSink {
    _stream: cpal::Stream,
    queue: Queue,
    paused: Arc<AtomicBool>,
    resampler: Resampler,
    cap: usize,
}

impl DeviceSink {
    pub(crate) fn open(rate: u32, channels: u16) -> Result<DeviceSink, AudioError> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or(AudioError::NoOutput)?;
        let supported = device.default_output_config().map_err(|_| AudioError::NoOutput)?;
        let format = supported.sample_format();
        let config: StreamConfig = supported.config();
        let queue: Queue = Arc::new(Mutex::new(VecDeque::new()));
        let paused = Arc::new(AtomicBool::new(false));
        let stream = match format {
            SampleFormat::F32 => build::<f32>(&device, &config, &queue, &paused),
            SampleFormat::I16 => build::<i16>(&device, &config, &queue, &paused),
            SampleFormat::U16 => build::<u16>(&device, &config, &queue, &paused),
            SampleFormat::I32 => build::<i32>(&device, &config, &queue, &paused),
            _ => Err(AudioError::NoOutput),
        }?;
        stream.play().map_err(|_| AudioError::NoOutput)?;
        let cap = (f64::from(config.sample_rate) * f64::from(config.channels) * AHEAD) as usize;
        Ok(DeviceSink {
            _stream: stream,
            queue,
            paused,
            resampler: Resampler::new(rate, channels, config.sample_rate, config.channels),
            cap: cap.max(1_024),
        })
    }
}

fn build<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: &StreamConfig,
    queue: &Queue,
    paused: &Arc<AtomicBool>,
) -> Result<cpal::Stream, AudioError> {
    let q = Arc::clone(queue);
    let p = Arc::clone(paused);
    device
        .build_output_stream::<T, _, _>(
            *config,
            move |data: &mut [T], _| {
                // Nunca se bloquea el hilo de la tarjeta: si la cola está ocupada, silencio.
                let mut guard = if p.load(Ordering::Relaxed) { None } else { q.try_lock().ok() };
                for s in data.iter_mut() {
                    let v = guard.as_mut().and_then(|g| g.pop_front()).unwrap_or(0.0);
                    *s = T::from_sample(v);
                }
            },
            |_| {},
            None,
        )
        .map_err(|_| AudioError::NoOutput)
}

impl Sink for DeviceSink {
    fn write(&mut self, samples: &[i16], ctl: &Control) -> Result<(), AudioError> {
        let out = self.resampler.process(samples);
        let mut rest: &[f32] = &out;
        while !rest.is_empty() {
            if ctl.stopping() {
                return Ok(());
            }
            let room = self.cap.saturating_sub(lock(&self.queue).len());
            if room == 0 {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            let (now, later) = rest.split_at(room.min(rest.len()));
            lock(&self.queue).extend(now.iter().copied());
            rest = later;
        }
        Ok(())
    }

    fn set_paused(&mut self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }

    fn flush(&mut self) {
        lock(&self.queue).clear();
    }

    fn finish(&mut self, ctl: &Control) {
        let start = Instant::now();
        while start.elapsed() < DRAIN && !ctl.stopping() && !lock(&self.queue).is_empty() {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
