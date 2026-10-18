//! WAV encode/decode and resampling to the canonical 16 kHz mono i16 format.

use anyhow::{Context, Result};
use std::io::Cursor;

pub const TARGET_RATE: u32 = 16_000;

/// Encode 16 kHz mono i16 samples to an in-memory WAV file.
pub fn encode_wav_16k_mono(samples: &[i16]) -> Result<Vec<u8>> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut buf = Cursor::new(Vec::with_capacity(44 + samples.len() * 2));
    {
        let mut w = hound::WavWriter::new(&mut buf, spec)?;
        for &s in samples {
            w.write_sample(s)?;
        }
        w.finalize()?;
    }
    Ok(buf.into_inner())
}

/// Average interleaved `channels` into mono.
pub fn downmix_mono(samples: &[f32], channels: u16) -> Vec<f32> {
    let ch = channels.max(1) as usize;
    if ch == 1 {
        samples.to_vec()
    } else {
        samples.chunks_exact(ch).map(|f| f.iter().sum::<f32>() / ch as f32).collect()
    }
}

/// Resample mono audio to 16 kHz. Downsampling uses area averaging (a box low-pass over the
/// exact input span of each output sample, with fractional edge weights), which suppresses
/// aliasing far better than plain linear interpolation; upsampling is linear.
pub fn resample_to_16k(mono: &[f32], in_rate: u32) -> Vec<f32> {
    if in_rate == TARGET_RATE || in_rate == 0 || mono.is_empty() {
        return mono.to_vec();
    }
    let ratio = in_rate as f64 / TARGET_RATE as f64;
    let out_len = ((mono.len() as f64) / ratio).floor() as usize;
    let last = mono.len() - 1;
    if ratio > 1.0 {
        // Prefix sums keep this O(n) regardless of ratio.
        let mut prefix = Vec::with_capacity(mono.len() + 1);
        let mut acc = 0f64;
        prefix.push(0f64);
        for &x in mono {
            acc += x as f64;
            prefix.push(acc);
        }
        // Integral of the piecewise-constant signal over [0, t].
        let integral = |t: f64| -> f64 {
            let i = (t.floor() as usize).min(mono.len());
            let frac = t - i as f64;
            prefix[i] + if i < mono.len() { mono[i] as f64 * frac } else { 0.0 }
        };
        (0..out_len)
            .map(|i| {
                let t0 = i as f64 * ratio;
                ((integral(t0 + ratio) - integral(t0)) / ratio) as f32
            })
            .collect()
    } else {
        (0..out_len)
            .map(|i| {
                let pos = i as f64 * ratio;
                let i0 = pos.floor() as usize;
                let frac = (pos - i0 as f64) as f32;
                let a = mono[i0.min(last)];
                let b = mono[(i0 + 1).min(last)];
                a + (b - a) * frac
            })
            .collect()
    }
}

/// Downmix interleaved `channels` to mono (average), resample `in_rate` -> 16 kHz,
/// and convert to i16 (clamped).
pub fn to_16k_mono(samples: &[f32], in_rate: u32, channels: u16) -> Vec<i16> {
    let mono = downmix_mono(samples, channels);
    resample_to_16k(&mono, in_rate)
        .into_iter()
        .map(|s| (s.clamp(-1.0, 1.0) * 32767.0).round() as i16)
        .collect()
}

/// Read any WAV (int or float, any rate/channels) into 16 kHz mono i16.
pub fn read_wav_16k_mono(bytes: &[u8]) -> Result<Vec<i16>> {
    let mut r = hound::WavReader::new(Cursor::new(bytes)).context("not a valid WAV file")?;
    let spec = r.spec();
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let max = (1u64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>().map(|s| s.map(|v| v as f32 / max)).collect::<Result<_, _>>()?
        }
    };
    Ok(to_16k_mono(&samples, spec.sample_rate, spec.channels))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_roundtrip() {
        let s: Vec<i16> = (0..1600).map(|i| ((i * 37) % 2000 - 1000) as i16).collect();
        let bytes = encode_wav_16k_mono(&s).unwrap();
        assert_eq!(&bytes[..4], b"RIFF");
        let mut r = hound::WavReader::new(Cursor::new(&bytes)).unwrap();
        assert_eq!(r.spec().sample_rate, 16_000);
        assert_eq!(r.spec().channels, 1);
        let back: Vec<i16> = r.samples::<i16>().map(|x| x.unwrap()).collect();
        assert_eq!(back, s);
        assert_eq!(read_wav_16k_mono(&bytes).unwrap(), s);
    }

    #[test]
    fn passthrough_16k_mono() {
        let out = to_16k_mono(&[0.0, 0.5, -0.5, 1.0], 16_000, 1);
        assert_eq!(out, vec![0, 16384, -16384, 32767]);
    }

    #[test]
    fn downmix_stereo() {
        let out = to_16k_mono(&[1.0, -1.0, 0.5, 0.5], 16_000, 2);
        assert_eq!(out, vec![0, 16384]);
    }

    #[test]
    fn resample_lengths() {
        let one_sec_48k = vec![0.25f32; 48_000];
        assert_eq!(to_16k_mono(&one_sec_48k, 48_000, 1).len(), 16_000);
        let one_sec_44k = vec![0.25f32; 44_100];
        let n = to_16k_mono(&one_sec_44k, 44_100, 1).len();
        assert!((15_999..=16_000).contains(&n), "{n}");
        // upsample 8k -> 16k
        assert_eq!(to_16k_mono(&vec![0.0; 8_000], 8_000, 1).len(), 16_000);
    }

    #[test]
    fn resample_preserves_ramp() {
        // Linear ramp stays linear under linear interpolation.
        let ramp: Vec<f32> = (0..4800).map(|i| i as f32 / 4800.0).collect();
        let out = to_16k_mono(&ramp, 48_000, 1);
        for w in out.windows(2) {
            assert!(w[1] >= w[0]);
        }
        // Area average over input samples [300, 303) of the ramp = value at 301.
        assert!((out[100] as f32 - (301.0 / 4800.0 * 32767.0)).abs() < 2.0);
    }

    #[test]
    fn common_rates_stereo_no_clipping() {
        for rate in [44_100u32, 48_000, 32_000, 22_050] {
            // 1 s of stereo 440 Hz sine at 0.8 amplitude.
            let n = rate as usize;
            let mut st = Vec::with_capacity(n * 2);
            for i in 0..n {
                let v = 0.8 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / rate as f32).sin();
                st.push(v);
                st.push(v);
            }
            let out = to_16k_mono(&st, rate, 2);
            assert!((15_990..=16_000).contains(&out.len()), "{rate}: {}", out.len());
            let peak = out.iter().map(|s| s.unsigned_abs()).max().unwrap();
            assert!(peak <= (0.81 * 32767.0) as u16, "{rate}: peak {peak} (gain overshoot)");
            assert!(peak >= (0.75 * 32767.0) as u16, "{rate}: peak {peak} (attenuated)");
        }
    }

    #[test]
    fn clamps_and_empty() {
        assert_eq!(to_16k_mono(&[2.0, -2.0], 16_000, 1), vec![32767, -32767]);
        assert!(to_16k_mono(&[], 48_000, 2).is_empty());
    }
}
