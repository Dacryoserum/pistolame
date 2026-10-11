//! Criterion 2: a worker thread paints a scrolling fake spectrogram into a 1024x256
//! `SharedPixelBuffer` at 60 Hz and hands it to the UI thread through a `Weak` handle.

use crate::MainWindow;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

pub const SPECTROGRAM_WIDTH_PIXELS: u32 = 1024;
pub const SPECTROGRAM_HEIGHT_PIXELS: u32 = 256;
const UPDATES_PER_SECOND: u64 = 60;
/// Each update scrolls the picture left by this many columns.
const COLUMNS_PER_UPDATE: usize = 2;

/// Counters shared with the UI so it can display the real delivery rate.
#[derive(Default)]
pub struct SpectrogramStats {
    pub frames_delivered: AtomicU64,
}

/// Blue to red to yellow gradient for a magnitude in `0.0..=1.0`.
fn magnitude_to_color(magnitude: f32) -> Rgba8Pixel {
    let clamped = magnitude.clamp(0.0, 1.0);
    let red = (clamped * 2.0).min(1.0);
    let green = ((clamped - 0.5) * 2.0).max(0.0);
    let blue = (0.4 - clamped).max(0.0) * 2.0 + 0.1;
    Rgba8Pixel {
        r: (red * 255.0) as u8,
        g: (green * 255.0) as u8,
        b: (blue.min(1.0) * 255.0) as u8,
        a: 255,
    }
}

/// Fake spectrum: a few moving ridges plus a harmonic comb.
fn fake_magnitude(row: u32, time_seconds: f32) -> f32 {
    let frequency = row as f32 / SPECTROGRAM_HEIGHT_PIXELS as f32;
    let ridge_center = 0.5 + 0.35 * (time_seconds * 0.7).sin();
    let ridge = (-((frequency - ridge_center) * 14.0).powi(2)).exp();
    let comb = ((frequency * 40.0 + time_seconds).sin() * 0.5 + 0.5) * (1.0 - frequency) * 0.4;
    (ridge + comb).clamp(0.0, 1.0)
}

fn scroll_and_paint(buffer: &mut SharedPixelBuffer<Rgba8Pixel>, time_seconds: f32) {
    let width = SPECTROGRAM_WIDTH_PIXELS as usize;
    let pixels = buffer.make_mut_slice();
    for row_pixels in pixels.chunks_exact_mut(width) {
        row_pixels.copy_within(COLUMNS_PER_UPDATE.., 0);
    }
    for (row, row_pixels) in pixels.chunks_exact_mut(width).enumerate() {
        // Highest frequency at the top of the picture.
        let row_from_bottom = SPECTROGRAM_HEIGHT_PIXELS - 1 - row as u32;
        for column in 0..COLUMNS_PER_UPDATE {
            let sample_time = time_seconds + column as f32 * 0.008;
            row_pixels[width - COLUMNS_PER_UPDATE + column] =
                magnitude_to_color(fake_magnitude(row_from_bottom, sample_time));
        }
    }
}

/// Starts the worker. It stops when `is_running` becomes false or the window is gone.
pub fn spawn_worker(
    window: Weak<MainWindow>,
    stats: Arc<SpectrogramStats>,
    is_running: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let frame_period = Duration::from_micros(1_000_000 / UPDATES_PER_SECOND);
        let started_at = Instant::now();
        let mut next_frame_at = started_at;
        let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(
            SPECTROGRAM_WIDTH_PIXELS,
            SPECTROGRAM_HEIGHT_PIXELS,
        );
        // Skip a tick instead of queueing frames when the UI thread is late.
        let is_previous_frame_pending = Arc::new(AtomicBool::new(false));

        while is_running.load(Ordering::Relaxed) {
            next_frame_at += frame_period;
            if let Some(wait) = next_frame_at.checked_duration_since(Instant::now()) {
                thread::sleep(wait);
            }
            if is_previous_frame_pending.load(Ordering::Acquire) {
                continue;
            }
            scroll_and_paint(&mut buffer, started_at.elapsed().as_secs_f32());

            is_previous_frame_pending.store(true, Ordering::Release);
            let frame = buffer.clone();
            let pending_flag = is_previous_frame_pending.clone();
            let stats = stats.clone();
            let is_window_alive = window
                .upgrade_in_event_loop(move |window| {
                    window.set_spectrogram(Image::from_rgba8(frame));
                    stats.frames_delivered.fetch_add(1, Ordering::Relaxed);
                    pending_flag.store(false, Ordering::Release);
                })
                .is_ok();
            if !is_window_alive {
                break;
            }
        }
    })
}
