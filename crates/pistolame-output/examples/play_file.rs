//! Headless playback through the WASAPI backend (plan B10.5).
//!
//! Until the decoder (T5) and the engine (T9) exist, this plays a generated 1 kHz test tone
//! instead of a file:
//!
//! `cargo run -p pistolame-output --example play_file -- --tone [--exclusive | --exclusive-only]
//!  [--device "K11"] [--rate 96000] [--bits 24] [--seconds 10] [--pause-every 3]`
//!
//! Without a flag the stream opens in shared mode, the app's default. `--exclusive` asks for
//! exclusive mode and falls back to shared mode when the device refuses the format or exclusive
//! mode is disabled in Windows (plan B5.4); `--exclusive-only` never falls back.
//!
//! It prints the negotiated format (the mix format and the cause in shared mode), each pause
//! (with whether the queued audio was fully played out) and each resume delay (reopen,
//! renegotiate, prefill).

#![allow(clippy::print_stdout, reason = "command-line diagnostic tool")]

#[cfg(windows)]
#[path = "support/test_tone.rs"]
mod test_tone;

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    windows_play::run()
}

#[cfg(not(windows))]
fn main() {
    println!("play_file: only the WASAPI backend exists so far; nothing to play on this OS.");
}

#[cfg(windows)]
mod windows_play {
    use std::process::ExitCode;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    use super::test_tone::TestToneRenderSource;
    use pistolame_output::wasapi::WasapiOutput;
    use pistolame_output::{
        AudioOutput, ChannelMask, NegotiatedOutput, OpenPolicy, OpenStreamRequest,
        OutputDeviceInfo, OutputStream, OutputStreamStatus, QueuedAudioOnClose, RenderSource,
        StreamFormat,
    };

    /// How long the device stays released during each pause.
    const PAUSE_DURATION: Duration = Duration::from_secs(1);
    /// How often the status is checked while playing.
    const STATUS_POLL_INTERVAL: Duration = Duration::from_millis(100);

    struct PlaybackOptions {
        device_name_filter: Option<String>,
        policy: OpenPolicy,
        is_tone_requested: bool,
        file_path: Option<String>,
        sample_rate_hz: u32,
        bits_per_sample: u8,
        total_duration: Duration,
        pause_interval: Option<Duration>,
    }

    fn parse_options() -> Result<PlaybackOptions, String> {
        let mut options = PlaybackOptions {
            device_name_filter: None,
            policy: OpenPolicy::SharedOnly,
            is_tone_requested: false,
            file_path: None,
            sample_rate_hz: 44_100,
            bits_per_sample: 16,
            total_duration: Duration::from_secs(10),
            pause_interval: None,
        };
        let mut arguments = std::env::args().skip(1);
        while let Some(argument) = arguments.next() {
            let mut value = || arguments.next().ok_or(format!("{argument} needs a value"));
            match argument.as_str() {
                "--device" => options.device_name_filter = Some(value()?.to_lowercase()),
                "--exclusive" => options.policy = OpenPolicy::ExclusiveThenShared,
                "--exclusive-only" => options.policy = OpenPolicy::ExclusiveOnly,
                "--tone" => options.is_tone_requested = true,
                "--rate" => options.sample_rate_hz = parse_number(&value()?)?,
                "--bits" => options.bits_per_sample = parse_number(&value()?)?,
                "--seconds" => {
                    options.total_duration = Duration::from_secs(parse_number(&value()?)?)
                }
                "--pause-every" => {
                    let seconds: u64 = parse_number(value()?.trim_end_matches('s'))?;
                    options.pause_interval = Some(Duration::from_secs(seconds));
                }
                path if !path.starts_with("--") => options.file_path = Some(path.to_owned()),
                unknown => return Err(format!("unknown option {unknown}")),
            }
        }
        Ok(options)
    }

    fn parse_number<T: std::str::FromStr>(text: &str) -> Result<T, String> {
        text.parse().map_err(|_| format!("not a number: {text}"))
    }

    pub(super) fn run() -> ExitCode {
        let options = match parse_options() {
            Ok(options) => options,
            Err(message) => {
                println!("{message}");
                return ExitCode::FAILURE;
            }
        };
        if let Some(path) = &options.file_path {
            println!(
                "File playback ({path}) needs the decoder and engine (tasks T5 and T9), not merged yet."
            );
            println!("Use --tone to test the output path with a generated 1 kHz tone.");
            return ExitCode::FAILURE;
        }
        if !options.is_tone_requested {
            println!("Nothing to play: pass --tone (file playback arrives with tasks T5 and T9).");
            return ExitCode::FAILURE;
        }
        if !matches!(options.bits_per_sample, 16 | 24) {
            println!("--bits must be 16 or 24");
            return ExitCode::FAILURE;
        }
        match play_tone(&options) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                println!("{message}");
                ExitCode::FAILURE
            }
        }
    }

    fn select_device(
        output: &mut WasapiOutput,
        filter: Option<&str>,
    ) -> Result<OutputDeviceInfo, String> {
        let devices = output
            .list_devices()
            .map_err(|error| format!("cannot list devices: {error}"))?;
        devices
            .into_iter()
            .find(|device| match filter {
                Some(filter) => device.display_name.to_lowercase().contains(filter),
                None => device.is_system_default,
            })
            .ok_or_else(|| "no matching output device".to_owned())
    }

    fn play_tone(options: &PlaybackOptions) -> Result<(), String> {
        let mut output = WasapiOutput::new();
        let device = select_device(&mut output, options.device_name_filter.as_deref())?;
        let source_format = StreamFormat {
            sample_rate_hz: options.sample_rate_hz,
            bits_per_sample: options.bits_per_sample,
            channel_count: 2,
            channel_mask: ChannelMask(0x3),
        };
        let tone = TestToneRenderSource::new(&source_format);
        let rendered_frame_counter = tone.rendered_frame_counter();
        let mut render_source: Box<dyn RenderSource> = Box::new(tone);
        println!("device: {}", device.display_name);
        let playback_started_at = Instant::now();
        let mut is_first_open = true;
        loop {
            let open_started_at = Instant::now();
            let request = OpenStreamRequest {
                device_id: device.id.clone(),
                source_format,
                policy: options.policy,
            };
            let mut stream = output
                .open_stream(request, render_source)
                .map_err(|failure| {
                    format!(
                        "open failed: {} ({} candidate attempts logged)",
                        failure.error,
                        tried_formats(&failure.error)
                    )
                })?;
            let open_duration = open_started_at.elapsed();
            if is_first_open {
                print_negotiated(stream.negotiated());
                println!("open + prefill: {open_duration:?}");
                is_first_open = false;
            } else {
                println!("resume delay (reopen, renegotiate, prefill): {open_duration:?}");
            }
            stream
                .play()
                .map_err(|error| format!("play failed: {error}"))?;
            let remaining = options
                .total_duration
                .saturating_sub(playback_started_at.elapsed());
            let play_for = options
                .pause_interval
                .map_or(remaining, |interval| interval.min(remaining));
            watch_stream(stream.as_ref(), play_for)?;
            let is_last_segment = play_for >= remaining;
            let closed = stream.close(QueuedAudioOnClose::PlayOut);
            println!(
                "{}: queued audio fully played = {}, tone frames rendered so far = {}",
                if is_last_segment { "stop" } else { "pause" },
                closed.was_queued_audio_fully_played,
                rendered_frame_counter.load(Ordering::Relaxed)
            );
            render_source = closed.render_source;
            if is_last_segment {
                println!("diagnostics: {:?}", output.diagnostics());
                return Ok(());
            }
            std::thread::sleep(PAUSE_DURATION);
        }
    }

    fn watch_stream(stream: &dyn OutputStream, play_for: Duration) -> Result<(), String> {
        let started_at = Instant::now();
        while started_at.elapsed() < play_for {
            if let OutputStreamStatus::Failed(failure) = stream.status() {
                return Err(format!("stream failed: {failure:?}"));
            }
            std::thread::sleep(STATUS_POLL_INTERVAL);
        }
        Ok(())
    }

    fn tried_formats(error: &pistolame_output::OutputError) -> usize {
        match error {
            pistolame_output::OutputError::FormatNotSupported { tried } => tried.len(),
            _ => 0,
        }
    }

    fn print_negotiated(negotiated: &NegotiatedOutput) {
        println!("access mode: {:?}", negotiated.access_mode);
        if let Some(cause) = negotiated.shared_mode_cause {
            println!("shared mode cause: {cause:?}");
        }
        if let Some(mix) = negotiated.system_mix_format {
            println!(
                "system mix format: {} Hz, {}-bit {}, {} channels",
                mix.sample_rate_hz,
                mix.bits_per_sample,
                if mix.is_float { "float" } else { "integer" },
                mix.channel_count
            );
        }
        let render_format = negotiated.render_format;
        println!(
            "render format: {} Hz, {}-byte container, {} valid bits",
            render_format.sample_rate_hz,
            render_format.container_bytes,
            render_format.valid_bits_per_sample
        );
        if let Some(format) = negotiated.device_format {
            println!(
                "device format: {} Hz, {}-byte container, {} valid bits, {} channels, mask {:#x}",
                format.sample_rate_hz,
                format.container_bytes,
                format.valid_bits_per_sample,
                format.channel_count,
                format.channel_mask.0
            );
        }
        println!(
            "buffer: {} frames, period {} x 100 ns",
            negotiated.buffer_frame_count, negotiated.device_period_in_hundred_nanoseconds
        );
        for attempt in &negotiated.negotiation_log {
            println!(
                "  tried {:?} {}B v{}: {:?}",
                attempt.access_mode,
                attempt.candidate.container_bytes,
                attempt.candidate.valid_bits_per_sample,
                attempt.outcome
            );
        }
    }
}
