//! Band magnitudes for the visualiser: what the audio actually contains,
//! rather than a shape drawn from its volume.
//!
//! A level meter has one number, so an animation driven from it can only
//! breathe in and out — every sound looks the same, and the thing on screen
//! says nothing the meter did not already say. Speech has a shape: a voice
//! puts its energy between roughly 100 Hz and 4 kHz and moves it around
//! constantly, which is what makes a spectrum worth drawing at all.
//!
//! This runs on the capture thread, beside [`crate::levels`], at the same
//! [`crate::capture`] cadence of twenty blocks a second. A 512-point transform
//! at 16 kHz costs a few thousand multiplies — far less than the `pactl` read
//! that delivered the samples — but it is still the audio path, so the
//! analyser owns its buffers and allocates nothing per block.

use std::f32::consts::PI;

/// How many bars the visualiser gets.
///
/// Chosen for the narrowest place it is drawn: the compact caption bar is 760
/// points wide and gives the visualiser a strip across it, so 24 bars sit at
/// about 30 points each with their gaps — wide enough to read as bars rather
/// than as noise. More bands would not survive that width, and the styles that
/// draw a line instead interpolate between them anyway.
///
/// Defined in `fc-core` because the event that carries them is.
pub const BANDS: usize = fc_core::SPECTRUM_BANDS;

/// Points per transform. At 16 kHz this is 32 ms of audio and 31.25 Hz per
/// bin, which resolves the bottom of the speech range into separate bands
/// while still being short enough that a syllable does not smear across two
/// frames.
const FFT_SIZE: usize = 512;

/// The span the bands cover, in Hz.
///
/// The bottom is above the rumble that monitors of a desktop mix are full of —
/// fans, mains hum, the DC offset some capture paths carry — which would
/// otherwise peg the first bar permanently. The top is the Nyquist frequency
/// of the 16 kHz capture, so the last band ends where the signal does.
const LOW_HZ: f32 = 60.0;
const HIGH_HZ: f32 = 8_000.0;

/// The dynamic range the bars show, in dB below full scale.
///
/// Speech in a meeting arrives well below 0 dBFS, and a linear magnitude puts
/// all of it in the bottom tenth of the bar. 70 dB is the range a VU meter
/// shows and lands normal speech around half height.
const FLOOR_DB: f32 = -70.0;

/// How fast a band may fall, as a fraction of its distance to the new value
/// each block.
///
/// Rises are instant: the attack of a consonant is the part that makes the
/// visualiser look like it is listening, and smoothing it is what makes an
/// animation feel laggy. Falls are eased, because a band that drops straight
/// to zero between two blocks of the same word reads as flicker. Applied at
/// the capture rate of 20 Hz, so the slowest a bar takes to cross its full
/// height is about a fifth of a second.
const FALL: f32 = 0.35;

/// Rolling analyser: feed it samples, read bands.
///
/// Holds a window of the most recent [`FFT_SIZE`] samples and the smoothed
/// result, so a caller that hands over fewer samples than a transform needs
/// still gets an answer that is current rather than one assembled from
/// whatever happened to arrive together.
pub struct Analyzer {
    /// The most recent `FFT_SIZE` samples, oldest first.
    history: Box<[f32; FFT_SIZE]>,
    /// Where the next sample goes in `history`, which is a ring.
    cursor: usize,
    /// Hann coefficients, computed once.
    window: Box<[f32; FFT_SIZE]>,
    re: Box<[f32; FFT_SIZE]>,
    im: Box<[f32; FFT_SIZE]>,
    bands: [f32; BANDS],
    /// First bin of each band, plus a final entry for the last band's end.
    edges: [usize; BANDS + 1],
}

impl Default for Analyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer {
    pub fn new() -> Self {
        let mut window = Box::new([0.0_f32; FFT_SIZE]);
        for (i, w) in window.iter_mut().enumerate() {
            // Hann. Without a window the transform of a tone that does not sit
            // exactly on a bin smears across the whole spectrum, which on
            // screen is every bar twitching at once whenever anyone speaks.
            *w = 0.5 * (1.0 - (2.0 * PI * i as f32 / FFT_SIZE as f32).cos());
        }
        Self {
            history: Box::new([0.0; FFT_SIZE]),
            cursor: 0,
            window,
            re: Box::new([0.0; FFT_SIZE]),
            im: Box::new([0.0; FFT_SIZE]),
            bands: [0.0; BANDS],
            edges: band_edges(),
        }
    }

    /// Adds a block of samples and recomputes the bands.
    ///
    /// Only the most recent [`FFT_SIZE`] samples are transformed, whatever the
    /// block size: a longer block means the earlier part of it is skipped
    /// rather than averaged in, which is what keeps the bars showing the sound
    /// happening now.
    pub fn push(&mut self, samples: &[f32]) {
        for &sample in samples {
            self.history[self.cursor] = sample;
            self.cursor = (self.cursor + 1) % FFT_SIZE;
        }
        self.recompute();
    }

    /// The bands as of the last [`push`](Self::push), each 0.0 to 1.0.
    pub fn bands(&self) -> [f32; BANDS] {
        self.bands
    }

    fn recompute(&mut self) {
        // Unroll the ring into the transform's input, oldest sample first, so
        // the window function lines up with the audio rather than with
        // wherever the cursor happens to be.
        for i in 0..FFT_SIZE {
            let sample = self.history[(self.cursor + i) % FFT_SIZE];
            self.re[i] = sample * self.window[i];
            self.im[i] = 0.0;
        }
        fft(&mut self.re[..], &mut self.im[..]);

        // A Hann window passes half the signal's amplitude, and the forward
        // transform of a real sine splits its energy between the positive and
        // negative frequency bins. Both are undone here so that a full-scale
        // tone reads as 0 dBFS rather than as some number that depends on the
        // transform's length.
        let scale = 4.0 / FFT_SIZE as f32;

        for band in 0..BANDS {
            let (start, end) = (self.edges[band], self.edges[band + 1]);
            // The loudest bin in the band, not the average of them. Bands are
            // log-spaced, so the top ones are a dozen bins wide and the bottom
            // ones are two; averaging would divide a tone's energy by whatever
            // width the band it landed in happens to have, and the same sound
            // would read quieter for being high-pitched. Peak-per-band is also
            // what makes the scaling above mean anything: a full-scale tone
            // reaches the top of its bar wherever it sits.
            let mut magnitude = 0.0_f32;
            for bin in start..end {
                let power = self.re[bin] * self.re[bin] + self.im[bin] * self.im[bin];
                magnitude = magnitude.max(power.sqrt());
            }
            let magnitude = magnitude * scale;
            let target = normalise(magnitude);
            // Instant attack, eased release — see `FALL`.
            self.bands[band] = if target >= self.bands[band] {
                target
            } else {
                self.bands[band] + (target - self.bands[band]) * FALL
            };
        }
    }
}

/// Magnitude to a 0..1 bar height, through dB.
fn normalise(magnitude: f32) -> f32 {
    if magnitude <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * magnitude.log10();
    ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0)
}

/// Log-spaced bin boundaries, so each band covers the same musical interval.
///
/// Linear spacing would give the top half of the bars to 4–8 kHz, where speech
/// has almost nothing, and squeeze every vowel into the first two. Bands are
/// also kept at least one bin wide: the bottom of the range is narrower than a
/// bin, and a band with no bins in it is a bar that never moves and a division
/// by zero in the averaging.
fn band_edges() -> [usize; BANDS + 1] {
    let mut edges = [0_usize; BANDS + 1];
    let nyquist_bin = FFT_SIZE / 2;
    let ratio = HIGH_HZ / LOW_HZ;
    let mut previous = 0;
    for (i, edge) in edges.iter_mut().enumerate() {
        let hz = LOW_HZ * ratio.powf(i as f32 / BANDS as f32);
        let bin = (hz * FFT_SIZE as f32 / crate::SAMPLE_RATE as f32).round() as usize;
        let bin = bin.clamp(previous + usize::from(i > 0), nyquist_bin);
        *edge = bin;
        previous = bin;
    }
    edges
}

/// In-place iterative radix-2 Cooley–Tukey FFT.
///
/// Twiddle factors are computed from scratch for each butterfly rather than
/// carried forward by the usual recurrence. The recurrence drifts over the
/// nine stages a 512-point transform takes, and the cost of not using it —
/// a few thousand `sin`/`cos` twenty times a second — is nothing next to being
/// able to test this against a direct transform and have it agree.
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    debug_assert!(n.is_power_of_two(), "radix-2 needs a power of two");
    debug_assert_eq!(n, im.len());

    // Bit-reversal permutation, so the butterflies below can run in place.
    let mut j = 0_usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }

    let mut len = 2;
    while len <= n {
        let half = len / 2;
        for start in (0..n).step_by(len) {
            for k in 0..half {
                let angle = -2.0 * PI * k as f32 / len as f32;
                let (sin, cos) = angle.sin_cos();
                let (ur, ui) = (re[start + k], im[start + k]);
                let (xr, xi) = (re[start + k + half], im[start + k + half]);
                let (vr, vi) = (xr * cos - xi * sin, xr * sin + xi * cos);
                re[start + k] = ur + vr;
                im[start + k] = ui + vi;
                re[start + k + half] = ur - vr;
                im[start + k + half] = ui - vi;
            }
        }
        len <<= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Direct transform, straight from the definition. Slow and obviously
    /// correct, which is the point: the fast one is checked against it.
    fn dft(input: &[f32]) -> Vec<(f32, f32)> {
        let n = input.len();
        (0..n)
            .map(|k| {
                let (mut re, mut im) = (0.0, 0.0);
                for (t, &x) in input.iter().enumerate() {
                    let angle = -2.0 * PI * (k * t) as f32 / n as f32;
                    re += x * angle.cos();
                    im += x * angle.sin();
                }
                (re, im)
            })
            .collect()
    }

    #[test]
    fn the_fast_transform_agrees_with_the_direct_one() {
        let n = 64;
        let input: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / n as f32;
                (2.0 * PI * 3.0 * t).sin() + 0.5 * (2.0 * PI * 11.0 * t).cos()
            })
            .collect();

        let expected = dft(&input);
        let mut re = input.clone();
        let mut im = vec![0.0; n];
        fft(&mut re, &mut im);

        for (bin, (want_re, want_im)) in expected.iter().enumerate() {
            assert!(
                (re[bin] - want_re).abs() < 1e-2 && (im[bin] - want_im).abs() < 1e-2,
                "bin {bin}: got ({:.4}, {:.4}), want ({want_re:.4}, {want_im:.4})",
                re[bin],
                im[bin]
            );
        }
    }

    #[test]
    fn silence_lights_nothing() {
        let mut analyzer = Analyzer::new();
        analyzer.push(&[0.0; FFT_SIZE]);
        assert!(
            analyzer.bands().iter().all(|&b| b == 0.0),
            "{:?}",
            analyzer.bands()
        );
    }

    /// The whole claim of this module: the bars say *where* the sound is, not
    /// just how loud it is. A tone at 1 kHz has to light the band containing
    /// 1 kHz and leave the far ends of the spectrum alone.
    #[test]
    fn a_tone_lights_the_band_it_belongs_to() {
        let hz = 1_000.0;
        let mut analyzer = Analyzer::new();
        let samples: Vec<f32> = (0..FFT_SIZE)
            .map(|i| (2.0 * PI * hz * i as f32 / crate::SAMPLE_RATE as f32).sin())
            .collect();
        analyzer.push(&samples);
        let bands = analyzer.bands();

        let ratio = HIGH_HZ / LOW_HZ;
        let expected = ((hz / LOW_HZ).log10() / ratio.log10() * BANDS as f32) as usize;
        let loudest = bands
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i)
            .unwrap();

        assert_eq!(loudest, expected, "bands: {bands:?}");
        assert!(bands[loudest] > 0.5, "the tone should be well up the bar");
        assert!(bands[0] < 0.2, "nothing belongs at the bottom end");
        assert!(bands[BANDS - 1] < 0.2, "nothing belongs at the top end");
    }

    /// A louder tone reads higher, which is what makes the visualiser track
    /// someone speaking up rather than only the pitch of their voice.
    #[test]
    fn a_quieter_tone_reads_lower() {
        let tone = |amplitude: f32| {
            let mut analyzer = Analyzer::new();
            let samples: Vec<f32> = (0..FFT_SIZE)
                .map(|i| amplitude * (2.0 * PI * 1_000.0 * i as f32 / 16_000.0).sin())
                .collect();
            analyzer.push(&samples);
            analyzer.bands().iter().cloned().fold(0.0, f32::max)
        };
        assert!(tone(1.0) > tone(0.1));
        assert!(tone(0.1) > tone(0.01));
    }

    /// Full scale has to reach the top of the bar, or the visualiser can never
    /// look loud however loud the meeting is.
    #[test]
    fn a_full_scale_tone_reaches_the_top() {
        let mut analyzer = Analyzer::new();
        let samples: Vec<f32> = (0..FFT_SIZE)
            .map(|i| (2.0 * PI * 1_000.0 * i as f32 / 16_000.0).sin())
            .collect();
        analyzer.push(&samples);
        let loudest = analyzer.bands().iter().cloned().fold(0.0, f32::max);
        assert!(loudest > 0.9, "got {loudest}");
    }

    /// Bands must never share a bin or run backwards: an empty band divides by
    /// zero, and an overlapping one draws the same sound twice.
    #[test]
    fn the_bands_are_contiguous_and_none_is_empty() {
        let edges = band_edges();
        for pair in edges.windows(2) {
            assert!(pair[1] > pair[0], "empty or reversed band in {edges:?}");
        }
        assert!(*edges.last().unwrap() <= FFT_SIZE / 2);
    }

    /// Rises are instant and falls are eased: a bar that dropped to nothing
    /// the moment a syllable ended would read as flicker rather than speech.
    #[test]
    fn a_band_falls_slower_than_it_rises() {
        let mut analyzer = Analyzer::new();
        let loud: Vec<f32> = (0..FFT_SIZE)
            .map(|i| (2.0 * PI * 1_000.0 * i as f32 / 16_000.0).sin())
            .collect();
        analyzer.push(&loud);
        let peak = analyzer.bands().iter().cloned().fold(0.0, f32::max);
        assert!(peak > 0.9, "setup: expected a loud band, got {peak}");

        analyzer.push(&[0.0; FFT_SIZE]);
        let after = analyzer.bands().iter().cloned().fold(0.0, f32::max);
        assert!(
            after > 0.0 && after < peak,
            "silence should ease the bar down, not cut it: {peak} then {after}"
        );
    }

    /// The analyser is fed whatever the capture loop happens to have, which is
    /// not a multiple of the transform length and is often shorter than it.
    #[test]
    fn a_short_block_is_still_analysed() {
        let mut analyzer = Analyzer::new();
        for _ in 0..40 {
            let samples: Vec<f32> = (0..37)
                .map(|i| (2.0 * PI * 1_000.0 * i as f32 / 16_000.0).sin())
                .collect();
            analyzer.push(&samples);
        }
        assert!(analyzer.bands().iter().any(|&b| b > 0.0));
    }

    #[test]
    fn an_empty_block_changes_nothing_and_does_not_panic() {
        let mut analyzer = Analyzer::new();
        analyzer.push(&[]);
        assert_eq!(analyzer.bands(), [0.0; BANDS]);
    }
}
