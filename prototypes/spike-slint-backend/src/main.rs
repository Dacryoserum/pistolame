//! Slint 1.18.1 backend validation spike (ADR 0001, section 7). Throwaway code.
//!
//! Flags (all optional): `--rows N` (default 100000, 0 gives a RAM baseline),
//! `--no-spectrogram` (isolates the CPU cost of the worker), `--autobench` (runs the sort,
//! filter and scroll measurements without a human, prints them, then exits).

mod file_drop;
mod spectrogram;
mod tracks;
mod transport;

use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer, Timer, TimerMode};
use spectrogram::SpectrogramStats;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tracks::TrackModel;
use transport::{Command, CommandSource, Transport};

slint::include_modules!();

const DEFAULT_ROW_COUNT: usize = 100_000;
/// Budget from ADR 0001 section 7, criterion 1.
const SORT_FILTER_BUDGET_MILLISECONDS: f64 = 100.0;
const SCROLL_BENCHMARK_DURATION: Duration = Duration::from_secs(5);
const SCROLL_BENCHMARK_PIXELS_PER_SECOND: f32 = 20_000.0;
const SCROLL_TICK: Duration = Duration::from_millis(8);
/// The window must exist before SMTC can bind to its HWND.
const MEDIA_KEYS_SETUP_DELAY: Duration = Duration::from_millis(300);
const AUTOBENCH_START_DELAY: Duration = Duration::from_secs(4);
const TRAY_ICON_SIZE_PIXELS: u32 = 32;

struct Options {
    row_count: usize,
    is_spectrogram_enabled: bool,
    is_autobench_enabled: bool,
}

fn parse_options() -> Options {
    let mut options = Options {
        row_count: DEFAULT_ROW_COUNT,
        is_spectrogram_enabled: true,
        is_autobench_enabled: false,
    };
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--rows" => {
                options.row_count = arguments
                    .next()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(DEFAULT_ROW_COUNT);
            }
            "--no-spectrogram" => options.is_spectrogram_enabled = false,
            "--autobench" => options.is_autobench_enabled = true,
            other => eprintln!("ignored argument: {other}"),
        }
    }
    options
}

fn main() -> Result<(), slint::PlatformError> {
    let options = parse_options();
    // Force the winit backend: the file-drop hook is a no-op on any other backend.
    slint::BackendSelector::new().backend_name("winit".into()).select()?;

    let main_window = MainWindow::new()?;
    let mini_window = MiniPlayerWindow::new()?;
    let tray = AppTray::new()?;
    let transport = Transport::new(&main_window, &mini_window);

    let model = setup_track_list(&main_window, options.row_count);
    let rendered_frames = setup_render_counter(&main_window);
    let spectrogram_stats = Arc::new(SpectrogramStats::default());
    let is_worker_running = Arc::new(AtomicBool::new(true));
    let worker = if options.is_spectrogram_enabled {
        Some(spectrogram::spawn_worker(
            main_window.as_weak(),
            spectrogram_stats.clone(),
            is_worker_running.clone(),
        ))
    } else {
        main_window.set_spectrogram_text("spectrogram disabled (--no-spectrogram)".into());
        None
    };
    let _statistics_timer =
        start_statistics_timer(&main_window, rendered_frames.clone(), spectrogram_stats);

    file_drop::install_file_drop(&main_window);
    wire_transport(&main_window, &mini_window, &tray, &transport);
    wire_tray(&main_window, &mini_window, &tray);

    let benchmark_timer = Rc::new(Timer::default());
    let scroll_window = main_window.as_weak();
    let scroll_timer = benchmark_timer.clone();
    let scroll_frames = rendered_frames.clone();
    main_window.on_run_scroll_benchmark(move || {
        if let Some(window) = scroll_window.upgrade() {
            start_scroll_benchmark(&window, &scroll_timer, &scroll_frames, false);
        }
    });

    main_window.show()?;
    let media_keys_window = main_window.as_weak();
    let media_keys_transport = transport.clone();
    Timer::single_shot(MEDIA_KEYS_SETUP_DELAY, move || {
        if let Some(window) = media_keys_window.upgrade() {
            let status = transport::install_media_keys(&window, media_keys_transport);
            println!("media keys: {status}");
            window.set_media_key_text(status.into());
        }
    });

    if options.is_autobench_enabled {
        let bench_window = main_window.as_weak();
        let bench_model = model.clone();
        let bench_timer = benchmark_timer.clone();
        let bench_frames = rendered_frames.clone();
        Timer::single_shot(AUTOBENCH_START_DELAY, move || {
            if let Some(window) = bench_window.upgrade() {
                run_autobench(&window, &bench_model, &bench_timer, &bench_frames);
            }
        });
    }

    slint::run_event_loop_until_quit()?;

    is_worker_running.store(false, Ordering::Relaxed);
    if let Some(worker) = worker {
        let _ = worker.join();
    }
    Ok(())
}

/// Criterion 1: custom model, sort and filter with measured timings.
fn setup_track_list(main_window: &MainWindow, row_count: usize) -> Rc<TrackModel> {
    let model = TrackModel::new(row_count);
    main_window.set_rows(model.as_model_rc());
    update_row_count(main_window, &model);

    let window = main_window.as_weak();
    let filter_model = model.clone();
    main_window.on_filter_changed(move |text| {
        let elapsed = filter_model.apply_filter(&text);
        if let Some(window) = window.upgrade() {
            report_timing(&window, &filter_model, "filter", elapsed);
        }
    });
    let window = main_window.as_weak();
    let sort_model = model.clone();
    main_window.on_sort_by(move |column| {
        let elapsed = sort_model.sort_by_column(column);
        if let Some(window) = window.upgrade() {
            report_timing(&window, &sort_model, "sort", elapsed);
        }
    });
    model
}

fn update_row_count(main_window: &MainWindow, model: &TrackModel) {
    main_window.set_row_count_text(
        format!("{} / {} rows", model.visible_count(), model.total_count()).into(),
    );
}

fn report_timing(main_window: &MainWindow, model: &TrackModel, label: &str, elapsed: Duration) {
    let milliseconds = elapsed.as_secs_f64() * 1000.0;
    let verdict = if milliseconds < SORT_FILTER_BUDGET_MILLISECONDS { "OK" } else { "TOO SLOW" };
    let text = format!("{label}: {milliseconds:.1} ms ({verdict}, budget 100 ms)");
    println!("{text}");
    main_window.set_list_timing_text(text.into());
    update_row_count(main_window, model);
}

/// Counts frames actually presented, via the rendering notifier (femtovg OpenGL).
fn setup_render_counter(main_window: &MainWindow) -> Arc<AtomicU64> {
    let rendered_frames = Arc::new(AtomicU64::new(0));
    let counter = rendered_frames.clone();
    let result = main_window.window().set_rendering_notifier(move |state, _api| {
        if matches!(state, slint::RenderingState::AfterRendering) {
            counter.fetch_add(1, Ordering::Relaxed);
        }
    });
    if let Err(error) = result {
        eprintln!("rendering notifier unavailable: {error}");
        main_window.set_render_fps_text("render fps: unavailable".into());
    }
    rendered_frames
}

/// Once per second: rendered fps and the spectrogram delivery rate.
fn start_statistics_timer(
    main_window: &MainWindow,
    rendered_frames: Arc<AtomicU64>,
    spectrogram_stats: Arc<SpectrogramStats>,
) -> Timer {
    let window = main_window.as_weak();
    let mut last_rendered = 0u64;
    let mut last_delivered = 0u64;
    let mut last_sampled_at = Instant::now();
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_secs(1), move || {
        let Some(window) = window.upgrade() else { return };
        let elapsed_seconds = last_sampled_at.elapsed().as_secs_f64();
        last_sampled_at = Instant::now();
        let rendered = rendered_frames.load(Ordering::Relaxed);
        let delivered = spectrogram_stats.frames_delivered.load(Ordering::Relaxed);
        let render_fps = (rendered - last_rendered) as f64 / elapsed_seconds;
        let delivery_fps = (delivered - last_delivered) as f64 / elapsed_seconds;
        (last_rendered, last_delivered) = (rendered, delivered);
        println!("stats: rendered {render_fps:.0} fps, spectrogram {delivery_fps:.1} Hz");
        window.set_render_fps_text(format!("rendered {render_fps:.0} fps").into());
        if delivered > 0 || last_delivered > 0 {
            window.set_spectrogram_text(
                format!(
                    "spectrogram {}x{} pushed from worker: {delivery_fps:.1} Hz (target 60)",
                    spectrogram::SPECTROGRAM_WIDTH_PIXELS,
                    spectrogram::SPECTROGRAM_HEIGHT_PIXELS
                )
                .into(),
            );
        }
    });
    timer
}

/// Scrolls the whole list at a fixed speed for a few seconds and reports rendered fps.
fn start_scroll_benchmark(
    main_window: &MainWindow,
    timer: &Rc<Timer>,
    rendered_frames: &Arc<AtomicU64>,
    should_quit_when_done: bool,
) {
    let window = main_window.as_weak();
    let frames_at_start = rendered_frames.load(Ordering::Relaxed);
    let rendered_frames = rendered_frames.clone();
    let started_at = Instant::now();
    let timer_handle = timer.clone();
    timer.start(TimerMode::Repeated, SCROLL_TICK, move || {
        let Some(window) = window.upgrade() else { return };
        let elapsed = started_at.elapsed();
        if elapsed >= SCROLL_BENCHMARK_DURATION {
            let frames = rendered_frames.load(Ordering::Relaxed) - frames_at_start;
            let fps = frames as f64 / elapsed.as_secs_f64();
            let text = format!("scroll benchmark: {fps:.1} rendered fps over {elapsed:.1?}");
            println!("{text}");
            window.set_list_timing_text(text.into());
            window.set_list_viewport_y(0.0);
            if should_quit_when_done {
                let _ = slint::quit_event_loop();
            }
            timer_handle.stop();
            return;
        }
        window.set_list_viewport_y(-(elapsed.as_secs_f32() * SCROLL_BENCHMARK_PIXELS_PER_SECOND));
    });
}

/// Unattended run of the criterion 1 measurements, for the release-build numbers.
fn run_autobench(
    main_window: &MainWindow,
    model: &Rc<TrackModel>,
    timer: &Rc<Timer>,
    rendered_frames: &Arc<AtomicU64>,
) {
    for column in [0, 1, 2, 3, 0] {
        report_timing(main_window, model, "sort", model.sort_by_column(column));
    }
    for filter in ["night", "ember hollow", "zzz", ""] {
        report_timing(main_window, model, "filter", model.apply_filter(filter));
    }
    start_scroll_benchmark(main_window, timer, rendered_frames, true);
}

fn wire_transport(
    main_window: &MainWindow,
    mini_window: &MiniPlayerWindow,
    tray: &AppTray,
    transport: &Transport,
) {
    let handle = transport.clone();
    main_window.on_toggle_play(move || handle.dispatch(Some(Command::Toggle), CommandSource::Window));
    let handle = transport.clone();
    main_window.on_next(move || handle.dispatch(Some(Command::Next), CommandSource::Window));
    let handle = transport.clone();
    mini_window.on_toggle_play(move || handle.dispatch(Some(Command::Toggle), CommandSource::Window));
    let handle = transport.clone();
    mini_window.on_next(move || handle.dispatch(Some(Command::Next), CommandSource::Window));
    let handle = transport.clone();
    tray.on_toggle_play(move || handle.dispatch(Some(Command::Toggle), CommandSource::Tray));
    let handle = transport.clone();
    tray.on_next(move || handle.dispatch(Some(Command::Next), CommandSource::Tray));
}

/// Criterion 5: Slint's built-in `SystemTrayIcon` (no `tray-icon` crate needed).
fn wire_tray(main_window: &MainWindow, mini_window: &MiniPlayerWindow, tray: &AppTray) {
    tray.set_tray_image(build_tray_icon());
    main_window.set_tray_text("Tray: Slint built-in SystemTrayIcon, right-click for the menu".into());

    let window = main_window.as_weak();
    tray.on_show_main(move || {
        if let Some(window) = window.upgrade() {
            if let Err(error) = window.show() {
                eprintln!("cannot show main window: {error}");
            }
        }
    });
    let mini = mini_window.as_weak();
    tray.on_show_mini(move || show_mini_player(&mini));
    let mini = mini_window.as_weak();
    main_window.on_open_mini_player(move || show_mini_player(&mini));
    tray.on_quit(|| {
        let _ = slint::quit_event_loop();
    });
}

fn show_mini_player(mini_window: &slint::Weak<MiniPlayerWindow>) {
    if let Some(mini_window) = mini_window.upgrade() {
        if let Err(error) = mini_window.show() {
            eprintln!("cannot show mini player: {error}");
        }
    }
}

/// A plain accent-colored disc, generated so the spike needs no image asset.
fn build_tray_icon() -> Image {
    let mut buffer =
        SharedPixelBuffer::<Rgba8Pixel>::new(TRAY_ICON_SIZE_PIXELS, TRAY_ICON_SIZE_PIXELS);
    let radius = TRAY_ICON_SIZE_PIXELS as f32 / 2.0;
    for (index, pixel) in buffer.make_mut_slice().iter_mut().enumerate() {
        let x = (index as u32 % TRAY_ICON_SIZE_PIXELS) as f32 + 0.5 - radius;
        let y = (index as u32 / TRAY_ICON_SIZE_PIXELS) as f32 + 0.5 - radius;
        let is_inside = x * x + y * y <= radius * radius;
        *pixel = Rgba8Pixel { r: 250, g: 88, b: 106, a: if is_inside { 255 } else { 0 } };
    }
    Image::from_rgba8(buffer)
}
