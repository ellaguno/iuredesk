//! Del formato del archivo al de la tarjeta de sonido (Windows y macOS): canales (mono ↔ estéreo)
//! y frecuencia por interpolación lineal (basta para voz). En Linux lo hace la herramienta del
//! sistema; aquí se compila igual para probarlo en todas partes.
#![cfg_attr(target_os = "linux", allow(dead_code))]

/// Convierte bloques entrelazados de `in_ch` canales a `in_rate` Hz en `out_ch` canales a
/// `out_rate` Hz, en `f32` de −1 a 1. Guarda el último cuadro entre bloques para no cortar.
pub(crate) struct Resampler {
    in_ch: usize,
    out_ch: usize,
    /// Cuadros de entrada por cuadro de salida.
    step: f64,
    /// Posición del siguiente cuadro de salida, contando el cuadro guardado como 0.
    t: f64,
    prev: Option<Vec<f32>>,
}

impl Resampler {
    pub(crate) fn new(in_rate: u32, in_ch: u16, out_rate: u32, out_ch: u16) -> Resampler {
        Resampler {
            in_ch: usize::from(in_ch.max(1)),
            out_ch: usize::from(out_ch.max(1)),
            step: f64::from(in_rate.max(1)) / f64::from(out_rate.max(1)),
            t: 0.0,
            prev: None,
        }
    }

    /// Un cuadro en los canales de salida.
    fn map(&self, frame: &[f32], out: &mut Vec<f32>) {
        match (self.in_ch, self.out_ch) {
            (i, o) if i == o => out.extend_from_slice(frame),
            (1, o) => {
                let v = frame.first().copied().unwrap_or(0.0);
                out.extend(std::iter::repeat_n(v, o));
            }
            (i, 1) => out.push(frame.iter().sum::<f32>() / i as f32),
            (_, o) => {
                for k in 0..o {
                    out.push(frame.get(k).copied().unwrap_or(0.0));
                }
            }
        }
    }

    pub(crate) fn process(&mut self, input: &[i16]) -> Vec<f32> {
        let ch = self.in_ch;
        let mut frames: Vec<Vec<f32>> = Vec::with_capacity(input.len() / ch + 1);
        if let Some(p) = self.prev.take() {
            frames.push(p);
        }
        for f in input.chunks_exact(ch) {
            frames.push(f.iter().map(|s| f32::from(*s) / 32_768.0).collect());
        }
        let n = frames.len();
        let mut out = Vec::new();
        if n == 0 {
            return out;
        }
        let mut mixed = vec![0.0f32; ch];
        while self.t + 1.0 < n as f64 {
            let i = self.t.floor() as usize;
            let frac = (self.t - i as f64) as f32;
            if let (Some(a), Some(b)) = (frames.get(i), frames.get(i + 1)) {
                for (k, m) in mixed.iter_mut().enumerate() {
                    let x = a.get(k).copied().unwrap_or(0.0);
                    let y = b.get(k).copied().unwrap_or(0.0);
                    *m = x + (y - x) * frac;
                }
                self.map(&mixed, &mut out);
            }
            self.t += self.step;
        }
        self.t -= (n - 1) as f64;
        self.prev = frames.pop();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn same_format_passes_through() {
        let mut r = Resampler::new(48_000, 2, 48_000, 2);
        let a = r.process(&[100, -100, 200, -200, 300, -300]);
        let b = r.process(&[400, -400]);
        // El último cuadro de cada bloque sale con el siguiente.
        assert_eq!(a.len() + b.len(), 6);
        assert!((a[0] - 100.0 / 32_768.0).abs() < 1e-6);
    }

    #[test]
    fn mono_to_stereo_and_double_rate() {
        let mut r = Resampler::new(8_000, 1, 16_000, 2);
        let out = r.process(&[0, 1_000, 2_000, 3_000, 4_000]);
        // 4 intervalos × 2 cuadros × 2 canales.
        assert_eq!(out.len(), 16);
        assert_eq!(out[0], out[1], "el mismo valor en ambos canales");
        assert!(out[2] > out[0] && out[2] < out[4], "el cuadro intermedio interpola");
    }

    #[test]
    fn stereo_to_mono_averages() {
        let mut r = Resampler::new(44_100, 2, 44_100, 1);
        let out = r.process(&[1_000, 3_000, 1_000, 3_000]);
        assert!((out[0] - 2_000.0 / 32_768.0).abs() < 1e-6);
    }

    proptest! {
        /// Con cualquier entrada y formato: sin pánico, valores en rango y cantidad proporcional.
        #[test]
        fn any_input_is_safe(
            input in prop::collection::vec(any::<i16>(), 0..4_000),
            in_rate in 1u32..200_000, out_rate in 1u32..200_000,
            in_ch in 0u16..8, out_ch in 0u16..8,
            split in 0usize..4_000,
        ) {
            let mut r = Resampler::new(in_rate, in_ch, out_rate, out_ch);
            let at = split.min(input.len());
            let mut out = r.process(&input[..at]);
            out.extend(r.process(&input[at..]));
            prop_assert!(out.iter().all(|v| v.is_finite() && (-1.0..=1.0).contains(v)));
            prop_assert_eq!(out.len() % usize::from(out_ch.max(1)), 0);
            let frames_in = input.len() / usize::from(in_ch.max(1));
            let expect = frames_in as f64 * f64::from(out_rate) / f64::from(in_rate);
            prop_assert!((out.len() / usize::from(out_ch.max(1))) as f64 <= expect + 2.0);
        }
    }
}
