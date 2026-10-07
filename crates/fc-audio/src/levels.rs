//! Peak and RMS over a block of samples, for the live level meter.
//!
//! Called at roughly 20 Hz from the capture thread, so this stays allocation-
//! free: a single pass with no intermediate `Vec` keeps the meter cheap enough
//! to never compete with the audio read loop for attention.

/// Largest absolute sample magnitude in `samples`. `0.0` for an empty slice.
pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0_f32, |acc, &s| acc.max(s.abs()))
}

/// Root-mean-square level of `samples`. `0.0` for an empty slice, rather than
/// `NaN` from a zero-length division.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|&s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_slice_is_silence_not_nan() {
        assert_eq!(peak(&[]), 0.0);
        assert_eq!(rms(&[]), 0.0);
    }

    #[test]
    fn peak_is_the_largest_magnitude_regardless_of_sign() {
        assert_eq!(peak(&[0.1, -0.9, 0.3]), 0.9);
    }

    #[test]
    fn rms_of_a_constant_signal_equals_its_magnitude() {
        let samples = [0.5_f32; 100];
        assert!((rms(&samples) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn rms_of_silence_is_zero() {
        assert_eq!(rms(&[0.0, 0.0, 0.0]), 0.0);
    }
}
