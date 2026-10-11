//! Fake average spectrogram that makes a spectral cutoff visible.

pub const SPECTROGRAM_WIDTH_PIXELS: u32 = 480;
pub const SPECTROGRAM_HEIGHT_PIXELS: u32 = 128;
/// Color ramp stops (position, 0xRRGGBB), low energy to high energy.
const SPECTROGRAM_RAMP: [(f32, u32); 5] = [
    (0.00, 0x141414),
    (0.25, 0x3B2A0E),
    (0.50, 0x9C6F22),
    (0.75, 0xE5B454),
    (1.00, 0xFFF3D1),
];

fn ramp_color(energy: f32) -> [u8; 3] {
    let energy = energy.clamp(0.0, 1.0);
    for pair in SPECTROGRAM_RAMP.windows(2) {
        let (start_position, start_color) = pair[0];
        let (end_position, end_color) = pair[1];
        if energy <= end_position {
            let amount = (energy - start_position) / (end_position - start_position);
            let channel = |shift: u32| {
                let from = ((start_color >> shift) & 0xFF) as f32;
                let to = ((end_color >> shift) & 0xFF) as f32;
                (from + (to - from) * amount).round() as u8
            };
            return [channel(16), channel(8), channel(0)];
        }
    }
    let last = SPECTROGRAM_RAMP[SPECTROGRAM_RAMP.len() - 1].1;
    [(last >> 16) as u8, (last >> 8) as u8, last as u8]
}

/// Fake average spectrogram: time left to right, frequency bottom (0 Hz) to top
/// (Nyquist of the file). Energy stops at `cutoff_fraction` of the Nyquist frequency;
/// `has_lossy_shelf` adds the band-limited "shelf" typical of lossy encoders.
pub fn spectrogram_rgba(cutoff_fraction: f32, has_lossy_shelf: bool, seed: u64) -> Vec<u8> {
    let width = SPECTROGRAM_WIDTH_PIXELS as usize;
    let height = SPECTROGRAM_HEIGHT_PIXELS as usize;
    let mut noise_state = seed | 1;
    let mut pixels = Vec::with_capacity(width * height * 4);
    for row in 0..height {
        let frequency_fraction = 1.0 - (row as f32 + 0.5) / height as f32;
        for column in 0..width {
            noise_state ^= noise_state << 13;
            noise_state ^= noise_state >> 7;
            noise_state ^= noise_state << 17;
            let noise = (noise_state % 1000) as f32 / 1000.0;
            let time = column as f32;
            let loudness = 0.70
                + 0.18 * (time * 0.045 + seed as f32 * 0.001).sin()
                + 0.12 * (time * 0.17).cos();
            let energy = if frequency_fraction <= cutoff_fraction {
                let spectral_tilt = (1.0 - frequency_fraction).powf(1.8) * 0.9 + 0.08;
                let shelf = if has_lossy_shelf && frequency_fraction > cutoff_fraction * 0.82 {
                    0.55
                } else {
                    1.0
                };
                spectral_tilt * loudness * shelf * (0.75 + 0.25 * noise)
            } else {
                0.03 * noise
            };
            let [red, green, blue] = ramp_color(energy.powf(0.75));
            pixels.extend_from_slice(&[red, green, blue, 255]);
        }
    }
    pixels
}
