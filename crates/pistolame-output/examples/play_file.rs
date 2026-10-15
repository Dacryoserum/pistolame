//! Headless playback through the real engine (plan B10.5).
//!
//! Play a FLAC file:
//!
//! `cargo run -p pistolame-output --example play_file -- [--quiet] [--exclusive] [--device "K11"]
//!  [--pause-every 3] [--seek-to 30] path/to/file.flac`
//!
//! Without `--exclusive` the stream opens in shared mode, the app's default; with it the engine
//! asks for exclusive mode and falls back to shared mode when the device refuses (B5.4). It
//! prints every engine event, the signal-path assessment (mode, render format, mix format,
//! cause), the counters each second and the resume delay after each resume. At the end of the
//! track it prints both verification levels with their digests and exits with 0 only when
//! they are conclusive and clean: level 1 `Verified` from frame 0 (or the `--seek-to` frame) to
//! the end of the track (`NotApplicableInSharedMode` is accepted only without `--exclusive`),
//! level 2 `Verified` or `NoEmbeddedSignature` (or "repositioned" after `--seek-to`), and no
//! playback failure. Anything else, including a failed resume, exits with 1.
//! `--quiet` keeps only opening information, failures and the final verdict for file playback.
//!
//! The output-path check with a generated 1 kHz tone at -20 dBFS is kept (WASAPI only):
//!
//! `cargo run -p pistolame-output --example play_file -- --tone [--exclusive | --exclusive-only]
//!  [--device "K11"] [--rate 96000] [--bits 24] [--seconds 10] [--pause-every 3]`
//!
//! `--exclusive-only` exists only for the tone: the engine never asks for
//! `OpenPolicy::ExclusiveOnly`, by design, so it is a backend check, not a playback mode.

#![allow(clippy::print_stdout, reason = "command-line diagnostic tool")]

use std::process::ExitCode;
use std::time::Duration;

#[cfg(windows)]
#[path = "support/test_tone.rs"]
mod test_tone;

/// Command-line options of both modes.
struct PlaybackOptions {
    device_name_filter: Option<String>,
    is_exclusive_requested: bool,
    is_exclusive_only_requested: bool,
    is_tone_requested: bool,
    is_quiet: bool,
    file_path: Option<String>,
    sample_rate_hz: u32,
    bits_per_sample: u8,
    total_duration: Duration,
    pause_interval: Option<Duration>,
    seek_position: Option<Duration>,
}

fn parse_options() -> Result<PlaybackOptions, String> {
    parse_arguments(std::env::args().skip(1))
}

fn parse_arguments(arguments: impl IntoIterator<Item = String>) -> Result<PlaybackOptions, String> {
    let mut options = PlaybackOptions {
        device_name_filter: None,
        is_exclusive_requested: false,
        is_exclusive_only_requested: false,
        is_tone_requested: false,
        is_quiet: false,
        file_path: None,
        sample_rate_hz: 44_100,
        bits_per_sample: 16,
        total_duration: Duration::from_secs(10),
        pause_interval: None,
        seek_position: None,
    };
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        let mut value = || arguments.next().ok_or(format!("{argument} needs a value"));
        match argument.as_str() {
            "--device" => options.device_name_filter = Some(value()?.to_lowercase()),
            "--exclusive" => options.is_exclusive_requested = true,
            "--exclusive-only" => options.is_exclusive_only_requested = true,
            "--tone" => options.is_tone_requested = true,
            "--quiet" => options.is_quiet = true,
            "--rate" => options.sample_rate_hz = parse_number(&value()?)?,
            "--bits" => options.bits_per_sample = parse_number(&value()?)?,
            "--seconds" => options.total_duration = Duration::from_secs(parse_number(&value()?)?),
            "--pause-every" => options.pause_interval = Some(parse_seconds(&value()?)?),
            "--seek-to" => options.seek_position = Some(parse_seconds(&value()?)?),
            path if !path.starts_with("--") => options.file_path = Some(path.to_owned()),
            unknown => return Err(format!("unknown option {unknown}")),
        }
    }
    if options.is_quiet && options.is_tone_requested {
        return Err("--quiet is available for file playback only".to_owned());
    }
    Ok(options)
}

fn parse_number<T: std::str::FromStr>(text: &str) -> Result<T, String> {
    text.parse().map_err(|_| format!("not a number: {text}"))
}

/// Whole seconds, with an optional `s` suffix ("3" or "3s").
fn parse_seconds(text: &str) -> Result<Duration, String> {
    parse_number(text.trim_end_matches('s')).map(Duration::from_secs)
}

fn main() -> ExitCode {
    let options = match parse_options() {
        Ok(options) => options,
        Err(message) => {
            println!("{message}");
            return ExitCode::FAILURE;
        }
    };
    if options.is_tone_requested {
        return run_tone(&options);
    }
    if options.is_exclusive_only_requested {
        println!("--exclusive-only is a backend check for --tone; use --exclusive for a file.");
        return ExitCode::FAILURE;
    }
    match options.file_path.as_deref() {
        Some(path) => file_play::run(path, &options),
        None => {
            println!("Nothing to play: pass a FLAC file, or --tone.");
            ExitCode::FAILURE
        }
    }
}

#[cfg(windows)]
fn run_tone(options: &PlaybackOptions) -> ExitCode {
    windows_tone::run(options)
}

#[cfg(not(windows))]
fn run_tone(_options: &PlaybackOptions) -> ExitCode {
    println!(
        "play_file --tone: only the WASAPI backend exists so far; nothing to play on this OS."
    );
    ExitCode::FAILURE
}

/// File playback through `EngineHandle` (B10.5).
mod file_play {
    use std::path::PathBuf;
    use std::process::ExitCode;
    use std::time::{Duration, Instant};

    use super::PlaybackOptions;
    use pistolame_core::{
        DecoderRegistry, EngineCommand, EngineEvent, EngineHandle, EngineStartOptions,
        FileIntegrityNotVerifiableReason, FileIntegrityStatus, OutputDeviceId,
        OutputModePreference, PlaybackCountersSnapshot, PlaybackState, SignalPathAssessment,
        TrackId, TransmissionStatus, VerificationStatus,
    };
    use pistolame_output::{
        DeviceAccessMode, OutputDeviceInfo, OutputDeviceSelection, platform_default_output,
    };

    /// How often events and counters are read (B10.5).
    const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(100);
    /// How often the counters are printed.
    const COUNTER_PRINT_INTERVAL: Duration = Duration::from_secs(1);
    /// How long the device stays released during each `--pause-every` pause.
    const PAUSE_DURATION: Duration = Duration::from_secs(1);
    /// Longest wait for the device list at start-up and for the final verification status.
    const ENGINE_REPLY_TIMEOUT: Duration = Duration::from_secs(5);

    /// What the run has seen so far.
    #[derive(Default)]
    struct PlaybackLog {
        track_id: Option<TrackId>,
        state: Option<PlaybackState>,
        has_played: bool,
        has_failed: bool,
        has_played_shared: bool,
        has_played_exclusive: bool,
        latest_status: Option<VerificationStatus>,
        resume_pending: bool,
        sample_rate_hz: u32,
        total_frames: Option<u64>,
    }

    pub(super) fn run(path: &str, options: &PlaybackOptions) -> ExitCode {
        let preference = if options.is_exclusive_requested {
            OutputModePreference::Exclusive
        } else {
            OutputModePreference::Shared
        };
        let handle = match EngineHandle::start(
            platform_default_output(),
            DecoderRegistry::with_default_providers(),
            EngineStartOptions {
                output_mode_preference: preference,
                device_selection: OutputDeviceSelection::SystemDefault,
                shared_mode_volume: Default::default(),
            },
        ) {
            Ok(handle) => handle,
            Err(error) => {
                println!("cannot start the engine: {error}");
                return ExitCode::FAILURE;
            }
        };
        let mut log = PlaybackLog::default();
        if let Err(message) = select_device(&handle, options, &mut log) {
            println!("{message}");
            handle.shutdown();
            return ExitCode::FAILURE;
        }
        println!("mode preference: {preference:?}");
        handle.send(EngineCommand::OpenFileAndPlay {
            path: PathBuf::from(path),
        });
        play_until_stopped(&handle, options, &mut log);
        wait_for_final_status(&handle, &mut log, options.is_quiet);
        handle.shutdown();
        report_and_judge(&log, options)
    }

    /// Waits for the start-up device list and selects the device `--device` names.
    fn select_device(
        handle: &EngineHandle,
        options: &PlaybackOptions,
        log: &mut PlaybackLog,
    ) -> Result<(), String> {
        let deadline = Instant::now() + ENGINE_REPLY_TIMEOUT;
        let (devices, selected) = loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let event = handle
                .receive_event_timeout(remaining)
                .ok_or("the engine sent no device list")?;
            print_event(&event, log, options.is_quiet);
            if let EngineEvent::DeviceListChanged {
                devices, selected, ..
            } = event
            {
                break (devices, selected);
            }
        };
        for device in devices.iter().filter(|_| !options.is_quiet) {
            println!(
                "device: {}{}",
                device.display_name,
                if device.is_system_default {
                    " (default)"
                } else {
                    ""
                }
            );
        }
        let Some(filter) = options.device_name_filter.as_deref() else {
            return match selected {
                Some(_) => Ok(()),
                None => Err("no default output device".to_owned()),
            };
        };
        let device = matching_device(&devices, filter)
            .ok_or_else(|| format!("no output device matches \"{filter}\""))?;
        println!("selected: {}", device.display_name);
        handle.send(EngineCommand::SelectOutputDevice {
            selection: OutputDeviceSelection::Specific(device.id.clone()),
        });
        Ok(())
    }

    fn matching_device<'devices>(
        devices: &'devices [OutputDeviceInfo],
        filter: &str,
    ) -> Option<&'devices OutputDeviceInfo> {
        devices
            .iter()
            .find(|device| device.display_name.to_lowercase().contains(filter))
    }

    /// Prints events every 100 ms and the counters every second; applies `--pause-every` and
    /// `--seek-to`; returns once the track is `Stopped` (or the start failed).
    fn play_until_stopped(handle: &EngineHandle, options: &PlaybackOptions, log: &mut PlaybackLog) {
        let mut last_counter_print = Instant::now();
        let mut playing_since: Option<Instant> = None;
        let mut paused_since: Option<Instant> = None;
        let mut is_seek_pending = options.seek_position.is_some();
        loop {
            std::thread::sleep(EVENT_POLL_INTERVAL);
            for event in handle.try_receive_events() {
                print_event(&event, log, options.is_quiet);
                if let EngineEvent::PlaybackStateChanged { state } = event {
                    playing_since = (state == PlaybackState::Playing).then(Instant::now);
                    paused_since = (state == PlaybackState::Paused).then(Instant::now);
                    if state == PlaybackState::Playing && log.resume_pending {
                        log.resume_pending = false;
                        if !options.is_quiet {
                            println!(
                                "resume delay (reopen, renegotiate, prefill): {} ms",
                                handle.counters().last_resume_delay_milliseconds
                            );
                        }
                    }
                }
            }
            if log.state == Some(PlaybackState::Stopped) || (log.has_failed && !log.has_played) {
                return;
            }
            if log.has_failed && log.state == Some(PlaybackState::Paused) {
                // A failed resume leaves the engine paused; nothing more will happen on its own.
                println!("playback failed while paused: giving up");
                return;
            }
            if !options.is_quiet && last_counter_print.elapsed() >= COUNTER_PRINT_INTERVAL {
                last_counter_print = Instant::now();
                print_counters(&handle.counters());
            }
            if is_seek_pending
                && playing_since.is_some()
                && let Some(position) = options.seek_position
            {
                is_seek_pending = false;
                if !options.is_quiet {
                    println!("seek to {position:?}");
                }
                handle.send(EngineCommand::SeekTo {
                    position_in_track: position,
                });
            }
            if let (Some(interval), Some(since)) = (options.pause_interval, playing_since)
                && since.elapsed() >= interval
            {
                playing_since = None;
                if !options.is_quiet {
                    println!("pause");
                }
                handle.send(EngineCommand::Pause);
            }
            if paused_since.is_some_and(|since| since.elapsed() >= PAUSE_DURATION) {
                paused_since = None;
                log.resume_pending = true;
                if !options.is_quiet {
                    println!("resume");
                }
                handle.send(EngineCommand::Play);
            }
        }
    }

    /// The engine forwards the final status before `Stopped`; events that are still on their
    /// way are read for a short while.
    fn wait_for_final_status(handle: &EngineHandle, log: &mut PlaybackLog, is_quiet: bool) {
        let deadline = Instant::now() + EVENT_POLL_INTERVAL * 2;
        while let Some(event) =
            handle.receive_event_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            print_event(&event, log, is_quiet);
        }
        if log.latest_status.is_none()
            && log.has_played
            && let Some(event) = handle.receive_event_timeout(ENGINE_REPLY_TIMEOUT)
        {
            print_event(&event, log, is_quiet);
        }
    }

    fn print_event(event: &EngineEvent, log: &mut PlaybackLog, is_quiet: bool) {
        record_event(event, log);
        if is_quiet && !should_print_quiet_event(event) {
            return;
        }
        match event {
            EngineEvent::DeviceListChanged { selected, .. } => {
                println!(
                    "event: device list changed, selected {}",
                    device_label(selected.as_ref())
                );
            }
            EngineEvent::PreferredDeviceUnavailable { device_id } => {
                println!("event: preferred device unavailable: {}", device_id.0);
            }
            EngineEvent::TrackLoaded {
                track_id, source, ..
            } => {
                println!(
                    "event: track {} loaded: {} Hz, {} bits, {} channels, {} frames, MD5 {}",
                    track_id.0,
                    source.format.sample_rate_hz,
                    source.format.bits_per_sample,
                    source.format.channel_count,
                    source
                        .total_frames
                        .map_or("unknown".to_owned(), |frames| frames.to_string()),
                    source
                        .embedded_md5
                        .map_or("absent".to_owned(), |md5| hex(&md5))
                );
            }
            EngineEvent::PlaybackStateChanged { state } => {
                println!("event: state {state:?}");
            }
            EngineEvent::SignalPathChanged { assessment } => match assessment {
                Some(assessment) => {
                    print_assessment(assessment);
                }
                None => println!("event: stream closed"),
            },
            EngineEvent::VerificationChanged { track_id, status } => {
                println!(
                    "event: verification of track {}: level 1 {:?}, level 2 {:?}",
                    track_id.0, status.transmission, status.file_integrity
                );
            }
            EngineEvent::PlaybackFailed { kind, detail } => {
                println!("event: playback failed: {kind:?} ({detail})");
            }
        }
    }

    // Quiet output must never change the state used to judge verification or exit status.
    fn record_event(event: &EngineEvent, log: &mut PlaybackLog) {
        match event {
            EngineEvent::TrackLoaded {
                track_id, source, ..
            } => {
                log.track_id = Some(*track_id);
                log.sample_rate_hz = source.format.sample_rate_hz;
                log.total_frames = source.total_frames;
            }
            EngineEvent::PlaybackStateChanged { state } => {
                log.state = Some(*state);
                log.has_played |= *state == PlaybackState::Playing;
            }
            EngineEvent::SignalPathChanged {
                assessment: Some(assessment),
            } => match assessment.negotiated.access_mode {
                DeviceAccessMode::Exclusive => log.has_played_exclusive = true,
                DeviceAccessMode::SharedThroughSystemMixer => log.has_played_shared = true,
            },
            EngineEvent::VerificationChanged { track_id, status }
                if log.track_id == Some(*track_id) =>
            {
                log.latest_status = Some(status.clone());
            }
            EngineEvent::PlaybackFailed { .. } => log.has_failed = true,
            _ => {}
        }
    }

    fn should_print_quiet_event(event: &EngineEvent) -> bool {
        matches!(
            event,
            EngineEvent::TrackLoaded { .. }
                | EngineEvent::SignalPathChanged {
                    assessment: Some(_)
                }
                | EngineEvent::PreferredDeviceUnavailable { .. }
                | EngineEvent::PlaybackFailed { .. }
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use pistolame_core::PlaybackFailureKind;

        #[test]
        fn quiet_still_records_hidden_states_and_verification() {
            let track_id = TrackId(7);
            let mut log = PlaybackLog {
                track_id: Some(track_id),
                ..PlaybackLog::default()
            };
            let playing = EngineEvent::PlaybackStateChanged {
                state: PlaybackState::Playing,
            };
            assert!(!should_print_quiet_event(&playing));
            print_event(&playing, &mut log, true);
            assert!(log.has_played);
            let status = VerificationStatus {
                transmission: TransmissionStatus::Verified {
                    first_frame_index: 0,
                    frame_count: 100,
                    md5: [1; 16],
                },
                file_integrity: FileIntegrityStatus::Verified { md5: [1; 16] },
            };
            let verified = EngineEvent::VerificationChanged {
                track_id,
                status: status.clone(),
            };
            assert!(!should_print_quiet_event(&verified));
            print_event(&verified, &mut log, true);
            assert_eq!(log.latest_status, Some(status));
            print_event(
                &EngineEvent::PlaybackStateChanged {
                    state: PlaybackState::Stopped,
                },
                &mut log,
                true,
            );
            assert_eq!(log.state, Some(PlaybackState::Stopped));
            let options =
                crate::parse_arguments(["--quiet", "song.flac"].map(str::to_owned)).unwrap();
            assert_eq!(report_and_judge(&log, &options), ExitCode::SUCCESS);
            let failure = EngineEvent::PlaybackFailed {
                kind: PlaybackFailureKind::DeviceUnplugged,
                detail: "disconnected".to_owned(),
            };
            assert!(should_print_quiet_event(&failure));
            print_event(&failure, &mut log, true);
            assert!(log.has_failed);
            assert_eq!(report_and_judge(&log, &options), ExitCode::FAILURE);
        }
    }

    fn device_label(device_id: Option<&OutputDeviceId>) -> String {
        device_id.map_or("none".to_owned(), |device_id| device_id.0.clone())
    }

    fn print_assessment(assessment: &SignalPathAssessment) {
        let negotiated = &assessment.negotiated;
        println!(
            "event: stream open on {}: {:?}",
            negotiated.device.display_name, negotiated.access_mode
        );
        if let Some(cause) = negotiated.shared_mode_cause {
            println!("  shared mode cause: {cause:?}");
        }
        let render_format = negotiated.render_format;
        println!(
            "  render format: {} Hz, {}-byte container, {} valid bits, {} channels",
            render_format.sample_rate_hz,
            render_format.container_bytes,
            render_format.valid_bits_per_sample,
            render_format.channel_count
        );
        if let Some(mix) = negotiated.system_mix_format {
            println!(
                "  system mix format: {} Hz, {}-bit {}, {} channels",
                mix.sample_rate_hz,
                mix.bits_per_sample,
                if mix.is_float { "float" } else { "integer" },
                mix.channel_count
            );
        }
        println!(
            "  buffer: {} frames, period {} x 100 ns",
            negotiated.buffer_frame_count, negotiated.device_period_in_hundred_nanoseconds
        );
        println!("  verdict: {:?}", assessment.verdict);
    }

    fn print_counters(counters: &PlaybackCountersSnapshot) {
        println!(
            "counters: frame {} of track {}, underruns {} ({} silent frames), tap overflows {}, \
             stale frames {}, contract violations {}",
            counters.frames_rendered_in_track,
            counters.current_track_id,
            counters.underrun_events,
            counters.silent_frames_inserted,
            counters.verification_tap_overflow_events,
            counters.stale_frames_discarded,
            counters.render_contract_violations
        );
    }

    /// Prints both levels and applies the exit rule described in the module documentation.
    fn report_and_judge(log: &PlaybackLog, options: &PlaybackOptions) -> ExitCode {
        let Some(status) = log.latest_status.as_ref() else {
            println!("no verification status was received");
            return ExitCode::FAILURE;
        };
        println!(
            "level 1 (transmission): {}",
            transmission_text(&status.transmission)
        );
        println!(
            "level 2 (file integrity): {}",
            file_integrity_text(&status.file_integrity)
        );
        let is_level_one_clean = match &status.transmission {
            TransmissionStatus::Verified {
                first_frame_index,
                frame_count,
                ..
            } => is_verified_range_complete(log, options, *first_frame_index, *frame_count),
            TransmissionStatus::NotApplicableInSharedMode if options.is_exclusive_requested => {
                println!("--exclusive was requested but the stream fell back to shared mode");
                false
            }
            TransmissionStatus::NotApplicableInSharedMode => {
                log.has_played_shared && !log.has_played_exclusive
            }
            _ => false,
        };
        let is_level_two_clean = match &status.file_integrity {
            FileIntegrityStatus::Verified { .. }
            | FileIntegrityStatus::NoEmbeddedSignature { .. } => true,
            FileIntegrityStatus::NotVerifiable {
                reason: FileIntegrityNotVerifiableReason::PlaybackRepositioned,
            } => options.seek_position.is_some(),
            _ => false,
        };
        if is_level_one_clean && is_level_two_clean && !log.has_failed {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }
    }

    /// Level 1 must cover the track from frame 0, or from the `--seek-to` frame, to its end
    /// when the file states its length.
    fn is_verified_range_complete(
        log: &PlaybackLog,
        options: &PlaybackOptions,
        first_frame_index: u64,
        frame_count: u64,
    ) -> bool {
        let seek_target_frame_index = options
            .seek_position
            .map(|position| seek_target_frame_index(position, log));
        let is_expected_start =
            first_frame_index == 0 || Some(first_frame_index) == seek_target_frame_index;
        let is_expected_length = log.total_frames.is_none_or(|total_frames| {
            frame_count == total_frames.saturating_sub(first_frame_index)
        });
        if !(is_expected_start && is_expected_length) {
            println!("level 1 does not cover the whole track from its start or the seek target");
        }
        is_expected_start && is_expected_length
    }

    /// The engine's own seek arithmetic: `position x rate` in integer nanoseconds, clamped to
    /// the track's length when known.
    fn seek_target_frame_index(position: Duration, log: &PlaybackLog) -> u64 {
        const NANOSECONDS_PER_SECOND: u128 = 1_000_000_000;
        let frames = position.as_nanos() * u128::from(log.sample_rate_hz) / NANOSECONDS_PER_SECOND;
        let frames = u64::try_from(frames).unwrap_or(u64::MAX);
        log.total_frames
            .map_or(frames, |total_frames| frames.min(total_frames))
    }

    fn transmission_text(status: &TransmissionStatus) -> String {
        match status {
            TransmissionStatus::Verified {
                first_frame_index,
                frame_count,
                md5,
            } => format!(
                "Verified, frames {first_frame_index}..{}, MD5 {}",
                first_frame_index + frame_count,
                hex(md5)
            ),
            TransmissionStatus::Mismatch {
                decoder_md5,
                device_md5,
                detail,
            } => format!(
                "MISMATCH ({detail:?}), decoder MD5 {}, device MD5 {}",
                hex(decoder_md5),
                hex(device_md5)
            ),
            other => format!("{other:?}"),
        }
    }

    fn file_integrity_text(status: &FileIntegrityStatus) -> String {
        match status {
            FileIntegrityStatus::Verified { md5 } => format!("Verified, MD5 {}", hex(md5)),
            FileIntegrityStatus::Mismatch {
                expected_md5,
                computed_md5,
                detail,
            } => format!(
                "MISMATCH ({detail:?}), file MD5 {}, decoded MD5 {}",
                hex(expected_md5),
                hex(computed_md5)
            ),
            FileIntegrityStatus::NoEmbeddedSignature { computed_md5 } => format!(
                "no MD5 in the file, decoded MD5 {}",
                computed_md5.map_or("not computed".to_owned(), |md5| hex(&md5))
            ),
            other => format!("{other:?}"),
        }
    }

    fn hex(digest: &[u8; 16]) -> String {
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

/// The test-tone check of the output path, unchanged since T10 (WASAPI only).
#[cfg(windows)]
mod windows_tone {
    use std::process::ExitCode;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    use super::PlaybackOptions;
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

    pub(super) fn run(options: &PlaybackOptions) -> ExitCode {
        if !matches!(options.bits_per_sample, 16 | 24) {
            println!("--bits must be 16 or 24");
            return ExitCode::FAILURE;
        }
        match play_tone(options) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                println!("{message}");
                ExitCode::FAILURE
            }
        }
    }

    fn policy(options: &PlaybackOptions) -> OpenPolicy {
        if options.is_exclusive_only_requested {
            OpenPolicy::ExclusiveOnly
        } else if options.is_exclusive_requested {
            OpenPolicy::ExclusiveThenShared
        } else {
            OpenPolicy::SharedOnly
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
                policy: policy(options),
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

#[cfg(test)]
mod tests {
    use super::parse_arguments;

    #[test]
    fn quiet_preserves_file_and_playback_options() {
        let options = parse_arguments(
            ["--quiet", "--exclusive", "--seek-to", "12", "song.flac"].map(str::to_owned),
        )
        .unwrap();
        assert!(options.is_quiet);
        assert!(options.is_exclusive_requested);
        assert_eq!(options.file_path.as_deref(), Some("song.flac"));
        assert_eq!(
            options.seek_position,
            Some(std::time::Duration::from_secs(12))
        );
        assert!(!parse_arguments(["song.flac".to_owned()]).unwrap().is_quiet);
    }

    #[test]
    fn quiet_rejects_tone_instead_of_ignoring_the_option() {
        assert!(parse_arguments(["--quiet", "--tone"].map(str::to_owned)).is_err());
    }
}
