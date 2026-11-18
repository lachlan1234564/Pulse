//! Sample-rate conversion that keeps the sound clean.
//!
//! Sound cards usually run at 48 kHz, while YouTube's audio (and most music files) is
//! 44.1 kHz. rodio converts between the two by drawing straight lines between samples,
//! which leaves faint, harsh extra tones: mirror images of the treble that land back in
//! the range you can hear. [`Resampled`] converts the textbook way instead ("band-limited
//! interpolation"): each output sample is a weighted sum of the ~96 input samples around
//! it, weighted by a windowed sinc, so only the music itself comes through. The song then
//! already runs at the sound card's rate and rodio's own converter has nothing to do.

use rodio::source::SeekError;
use rodio::{ChannelCount, SampleRate, Source};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// Filter length in input samples when converting up (longer when converting down).
const TAPS: usize = 96;
/// Kaiser window shape: about 87 dB between the music and what's filtered out.
const BETA: f64 = 8.6;
/// Stopband attenuation that `BETA` gives (Kaiser's formula), for sizing the filter.
const ATTENUATION_DB: f64 = BETA / 0.1102 + 8.7;
/// A rate pair whose ratio needs more filter phases than this is left to rodio.
const MAX_PHASES: u64 = 2048;
/// Drop played input from the front of the history once this much has piled up.
const COMPACT_AFTER: usize = 4096;

/// The filters for one pair of rates: one row of coefficients per position between two
/// input samples ("phase"). Output frame `n` sits at input position `n * down / up`.
struct Bank {
    up: u64,
    down: u64,
    taps: usize,
    /// `taps` coefficients per phase, phase after phase.
    coeffs: Vec<f32>,
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// Modified Bessel function of the first kind, order 0 (for the Kaiser window).
fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term, mut k) = (1.0, 1.0, 1.0);
    while term > sum * 1e-12 {
        term *= (x / (2.0 * k)).powi(2);
        sum += term;
        k += 1.0;
    }
    sum
}

/// Σ a·b, summed in 8 lanes so the compiler can use the CPU's vector instructions (one
/// running total can't be, since float addition order matters). About a quarter of the
/// time of a plain sum.
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut lanes = [0.0f32; 8];
    let (xs, ks) = (a.chunks_exact(8), b.chunks_exact(8));
    let tail: f32 = xs.remainder().iter().zip(ks.remainder()).map(|(x, k)| x * k).sum();
    for (x, k) in xs.zip(ks) {
        for i in 0..8 {
            lanes[i] += x[i] * k[i];
        }
    }
    lanes.iter().sum::<f32>() + tail
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 { 1.0 } else { (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x) }
}

impl Bank {
    fn new(from: u32, to: u32) -> Option<Bank> {
        let g = gcd(from as u64, to as u64);
        let (up, down) = (to as u64 / g, from as u64 / g);
        if up > MAX_PHASES {
            return None;
        }
        // Converting down, the filter cuts at the new, lower limit, so it's longer in input samples.
        let scale = (up as f64 / down as f64).min(1.0);
        let taps = (((TAPS as f64 / scale).ceil() as usize) + 1) & !1;
        let half = taps as f64 / 2.0;
        // Kaiser's estimate of the transition band this length allows (cycles per input sample);
        // the band ends exactly at the lower of the two rates' limits, so nothing folds back.
        let transition = (ATTENUATION_DB - 7.95) / (14.36 * taps as f64);
        let cutoff = 0.5 * scale - transition / 2.0;
        let i0_beta = bessel_i0(BETA);
        let mut coeffs = Vec::with_capacity(up as usize * taps);
        for phase in 0..up {
            let frac = phase as f64 / up as f64;
            let row: Vec<f64> = (0..taps)
                .map(|k| {
                    // Distance from the output position to input `j0 - (half - 1) + k`.
                    let t = frac + (half - 1.0) - k as f64;
                    let w = (1.0 - (t / half).powi(2)).max(0.0).sqrt();
                    2.0 * cutoff * sinc(2.0 * cutoff * t) * bessel_i0(BETA * w) / i0_beta
                })
                .collect();
            // Every phase passes a steady level unchanged.
            let sum: f64 = row.iter().sum();
            coeffs.extend(row.iter().map(|c| (c / sum) as f32));
        }
        Some(Bank { up, down, taps, coeffs })
    }

    /// The filter bank for `from` → `to`, built once per rate pair.
    fn shared(from: u32, to: u32) -> Option<Arc<Bank>> {
        static BANKS: OnceLock<Mutex<Vec<((u32, u32), Option<Arc<Bank>>)>>> = OnceLock::new();
        let banks = BANKS.get_or_init(Default::default);
        if let Some((_, b)) = banks.lock().unwrap().iter().find(|(k, _)| *k == (from, to)) {
            return b.clone();
        }
        let b = Bank::new(from, to).map(Arc::new);
        banks.lock().unwrap().push(((from, to), b.clone()));
        b
    }

    fn row(&self, phase: u64) -> &[f32] {
        let start = phase as usize * self.taps;
        &self.coeffs[start..start + self.taps]
    }
}

/// `inner`, converted to `out_rate`. Passes it through untouched when the rates already match.
pub struct Resampled<S: Source> {
    inner: S,
    out_rate: SampleRate,
    /// Converting from this rate (`None`: passing straight through).
    bank: Option<Arc<Bank>>,
    in_rate: u32,
    channels: usize,
    /// Input frames, one list per channel. Index 0 is padded frame `base`, where padded
    /// frame `i` is input frame `i - (taps/2 - 1)`: the song starts after silence, so the
    /// first output samples have input on both sides of them.
    hist: Vec<Vec<f32>>,
    base: u64,
    /// Real input frames read so far, and whether the input has ended.
    read: u64,
    ended: bool,
    /// The next output frame sits at input frame `j0` plus `phase / up`.
    j0: u64,
    phase: u64,
    frame: Vec<f32>,
    at: usize,
}

impl<S: Source> Resampled<S> {
    pub fn new(inner: S, out_rate: SampleRate) -> Self {
        let mut r = Resampled {
            channels: inner.channels().get() as usize,
            in_rate: inner.sample_rate().get(),
            inner,
            out_rate,
            bank: None,
            hist: vec![],
            base: 0,
            read: 0,
            ended: false,
            j0: 0,
            phase: 0,
            frame: vec![],
            at: 0,
        };
        r.restart();
        r
    }

    /// Starts converting afresh from wherever `inner` is now (at the start, or after a jump).
    fn restart(&mut self) {
        self.in_rate = self.inner.sample_rate().get();
        self.bank = if self.in_rate == self.out_rate.get() { None } else { Bank::shared(self.in_rate, self.out_rate.get()) };
        let pad = self.bank.as_ref().map_or(0, |b| b.taps / 2 - 1);
        self.hist = vec![vec![0.0; pad]; self.channels];
        self.base = 0;
        self.read = 0;
        self.ended = false;
        self.j0 = 0;
        self.phase = 0;
        self.frame = vec![0.0; self.channels];
        self.at = self.channels;
    }

    /// Reads one input frame into the history. False at the end of the song.
    fn pull_frame(&mut self) -> bool {
        // The decoder switched rates mid-song (very rare): convert from the new rate.
        if self.inner.sample_rate().get() != self.in_rate {
            self.restart();
            return self.bank.is_some() && self.pull_frame();
        }
        let n = self.inner.channels().get() as usize;
        let mut got = [0.0f32; 8];
        for c in 0..n {
            match self.inner.next() {
                Some(s) if c < got.len() => got[c] = s,
                Some(_) => {}
                None => return false,
            }
        }
        // A channel count that changed mid-song keeps the song's original layout.
        for (c, h) in self.hist.iter_mut().enumerate() {
            h.push(got[c.min(n.min(got.len()) - 1)]);
        }
        self.read += 1;
        true
    }

    /// Works out the next output frame. False once the song is over.
    fn next_frame(&mut self) -> bool {
        let Some(bank) = self.bank.clone() else { return false };
        // All of the song has been turned into output.
        if self.ended && self.j0 >= self.read {
            return false;
        }
        // The filter reads padded frames j0 ..= j0 + taps - 1.
        let need = self.j0 + bank.taps as u64;
        while self.base + (self.hist[0].len() as u64) < need {
            if self.ended || !self.pull_frame() {
                if !self.ended {
                    self.ended = true;
                    if self.j0 >= self.read {
                        return false;
                    }
                }
                // Silence after the end, so the last samples have input on both sides too.
                for h in &mut self.hist {
                    h.push(0.0);
                }
            }
        }
        let start = (self.j0 - self.base) as usize;
        let row = bank.row(self.phase);
        for (c, h) in self.hist.iter().enumerate() {
            self.frame[c] = dot(&h[start..start + bank.taps], row);
        }
        self.phase += bank.down;
        self.j0 += self.phase / bank.up;
        self.phase %= bank.up;
        if start > COMPACT_AFTER {
            for h in &mut self.hist {
                h.drain(..start);
            }
            self.base += start as u64;
        }
        true
    }
}

impl<S: Source> Iterator for Resampled<S> {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.bank.is_none() {
            return self.inner.next();
        }
        if self.at >= self.frame.len() {
            if !self.next_frame() {
                return None;
            }
            self.at = 0;
        }
        self.at += 1;
        Some(self.frame[self.at - 1])
    }
}

impl<S: Source> Source for Resampled<S> {
    fn current_span_len(&self) -> Option<usize> {
        // Converting: whole frames until the end, at one rate and channel count.
        if self.bank.is_some() { None } else { self.inner.current_span_len() }
    }

    fn channels(&self) -> ChannelCount {
        if self.bank.is_some() { ChannelCount::new(self.channels as u16).unwrap_or(ChannelCount::MIN) } else { self.inner.channels() }
    }

    fn sample_rate(&self) -> SampleRate {
        if self.bank.is_some() { self.out_rate } else { self.inner.sample_rate() }
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        self.inner.try_seek(pos)?;
        self.restart();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::buffer::SamplesBuffer;

    fn rate(r: u32) -> SampleRate {
        SampleRate::new(r).unwrap()
    }

    fn stereo_sine(freq: f64, sr: u32, secs: f64) -> SamplesBuffer {
        let n = (sr as f64 * secs) as usize;
        let mut v = Vec::with_capacity(n * 2);
        for i in 0..n {
            let s = (0.5 * (2.0 * std::f64::consts::PI * freq * i as f64 / sr as f64).sin()) as f32;
            v.push(s);
            v.push(s);
        }
        SamplesBuffer::new(ChannelCount::new(2).unwrap(), rate(sr), v)
    }

    /// Left channel only.
    fn left(samples: &[f32]) -> Vec<f64> {
        samples.chunks(2).map(|f| f[0] as f64).collect()
    }

    /// Everything that isn't the tone itself (distortion, added tones, noise), relative to
    /// the tone, in dB. The tone is fitted (any level and phase), so a delay doesn't count.
    fn junk_db(y: &[f64], freq: f64, sr: u32) -> f64 {
        // Skip the edges, where the song starts and stops.
        let y = &y[y.len() / 10..y.len() * 9 / 10];
        let w = 2.0 * std::f64::consts::PI * freq / sr as f64;
        let n = y.len() as f64;
        let (mut a, mut b) = (0.0, 0.0);
        for (i, v) in y.iter().enumerate() {
            a += v * (w * i as f64).sin();
            b += v * (w * i as f64).cos();
        }
        let (a, b) = (2.0 * a / n, 2.0 * b / n);
        let (mut sig, mut rest) = (0.0, 0.0);
        for (i, v) in y.iter().enumerate() {
            let fit = a * (w * i as f64).sin() + b * (w * i as f64).cos();
            sig += fit * fit;
            rest += (v - fit) * (v - fit);
        }
        10.0 * (rest / sig).log10()
    }

    fn rodio_convert(src: SamplesBuffer, to: u32) -> Vec<f32> {
        rodio::source::UniformSourceIterator::new(src, ChannelCount::new(2).unwrap(), rate(to)).collect()
    }

    #[test]
    fn matching_rates_pass_straight_through() {
        let src = stereo_sine(1000.0, 48_000, 0.1);
        let expected: Vec<f32> = src.clone().collect();
        let r = Resampled::new(src, rate(48_000));
        assert_eq!(r.sample_rate().get(), 48_000);
        assert_eq!(r.collect::<Vec<f32>>(), expected);
    }

    #[test]
    fn a_second_of_music_stays_a_second_long_and_level() {
        let n = 44_100;
        let src = SamplesBuffer::new(ChannelCount::new(2).unwrap(), rate(44_100), vec![0.25f32; n * 2]);
        let r = Resampled::new(src, rate(48_000));
        assert_eq!((r.channels().get(), r.sample_rate().get()), (2, 48_000));
        let out: Vec<f32> = r.collect();
        let frames = out.len() / 2;
        assert!((frames as i64 - 48_000).abs() <= 2, "{frames} frames");
        // A steady level stays exactly that level (away from the very start and end).
        for s in &out[2000..out.len() - 2000] {
            assert!((s - 0.25).abs() < 1e-4, "{s}");
        }
    }

    /// 44.1 kHz music on a 48 kHz sound card (the usual case): the treble comes through
    /// clean instead of with harsh extra tones.
    #[test]
    fn converting_up_adds_no_harsh_tones() {
        for freq in [1_000.0, 10_000.0, 15_000.0] {
            let ours = left(&Resampled::new(stereo_sine(freq, 44_100, 1.0), rate(48_000)).collect::<Vec<_>>());
            let theirs = left(&rodio_convert(stereo_sine(freq, 44_100, 1.0), 48_000));
            let (ours, theirs) = (junk_db(&ours, freq, 48_000), junk_db(&theirs, freq, 48_000));
            println!("{freq} Hz, 44.1 -> 48 kHz: extra sound {ours:.1} dB (rodio's straight lines: {theirs:.1} dB)");
            assert!(ours < -75.0, "{freq} Hz: {ours:.1} dB");
            assert!(ours < theirs - 20.0);
        }
    }

    /// 48 kHz audio on a 44.1 kHz sound card: sound too high for 44.1 kHz is filtered out,
    /// instead of folding back down as an audible whistle.
    #[test]
    fn converting_down_folds_nothing_back() {
        // 23 kHz can't exist at 44.1 kHz; dropped samples would turn it into 21.1 kHz.
        let tone = 23_000.0;
        let level = |v: &[f32]| (v.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / v.len() as f64).sqrt();
        let ours: Vec<f32> = Resampled::new(stereo_sine(tone, 48_000, 0.5), rate(44_100)).collect();
        let theirs = rodio_convert(stereo_sine(tone, 48_000, 0.5), 44_100);
        let input = 0.5 / 2f64.sqrt();
        let (ours, theirs) = (20.0 * (level(&ours[4000..ours.len() - 4000]) / input).log10(), 20.0 * (level(&theirs) / input).log10());
        println!("23 kHz, 48 -> 44.1 kHz: what's left {ours:.1} dB (rodio: {theirs:.1} dB)");
        assert!(ours < -60.0, "{ours:.1} dB");
        // Music below the limit still comes through clean.
        let clean = left(&Resampled::new(stereo_sine(12_000.0, 48_000, 1.0), rate(44_100)).collect::<Vec<_>>());
        assert!(junk_db(&clean, 12_000.0, 44_100) < -75.0);
    }

    #[test]
    fn jumping_in_the_song_starts_cleanly_again() {
        let mut r = Resampled::new(stereo_sine(440.0, 44_100, 2.0), rate(48_000));
        let _: Vec<f32> = r.by_ref().take(20_000).collect();
        r.try_seek(Duration::from_millis(500)).unwrap();
        let rest: Vec<f32> = r.collect();
        // Half a second was skipped: about 1.5 s of 48 kHz stereo is left.
        let frames = rest.len() as i64 / 2;
        assert!((frames - 72_000).abs() < 100, "{frames}");
        assert!(rest.iter().all(|s| s.is_finite() && s.abs() <= 0.51));
    }

    /// What converting costs: a minute of 44.1 kHz stereo to 48 kHz. Run it optimized:
    /// `cargo test --release resampling_cost -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn resampling_cost() {
        let src = stereo_sine(1000.0, 44_100, 60.0);
        let at = std::time::Instant::now();
        let n = Resampled::new(src, rate(48_000)).fold(0usize, |n, s| n + (s != 0.0) as usize);
        let took = at.elapsed().as_secs_f64();
        println!("60 s of stereo music converted in {:.0} ms ({n} samples): {:.2}% of one CPU core while playing", took * 1000.0, took / 60.0 * 100.0);
    }

    #[test]
    fn odd_rate_pairs_are_left_to_rodio() {
        // 44,101 → 48,000 would need 48,000 filters.
        assert!(Bank::new(44_101, 48_000).is_none());
        assert!(Bank::new(44_100, 48_000).is_some_and(|b| b.up == 160 && b.down == 147));
    }
}
