//! Shared helpers of the engine integration tests (plan B10.3): an engine on a `FakeOutput`,
//! event waits with a timeout (no test sleeps to wait for a result), and the expected device
//! bytes and digests computed from the generator's PCM, never from the decoder's.

#![allow(
    dead_code,
    reason = "each test binary uses a different subset of the helpers"
)]

use std::path::Path;
use std::time::{Duration, Instant};

use md5::{Digest, Md5};
use pistolame_core::testing::{
    AcceptedFormats, FakeOutput, FakeOutputConfig, FakeOutputController, FixtureSpec, FormatChoice,
};
use pistolame_core::{
    DecoderRegistry, DeviceSampleFormat, EngineCommand, EngineEvent, EngineHandle,
    EngineStartOptions, EngineTuning, OutputModePreference, PlaybackFailureKind, PlaybackState,
    SignalPathAssessment, StreamFormat, TrackId, ValidBitsAlignment, VerificationStatus,
    lossless_device_format_candidates, pack_samples_into_container,
};

/// Longest wait for any expected event; generous for unoptimized builds on a busy machine.
pub const EVENT_WAIT: Duration = Duration::from_secs(60);

/// Sizes for exact-bytes tests: the ring and the prefill hold the whole fixture, so it is all
/// in the ring before `play()` and the unpaced fake never underruns; the tap holds it too, so
/// the verifier never overflows however far behind it falls.
pub fn whole_track_tuning(spec: &FixtureSpec) -> EngineTuning {
    let track_duration = Duration::from_secs(u64::from(spec.duration_seconds) + 1);
    EngineTuning {
        playback_ring_duration: track_duration,
        start_prefill_duration: track_duration * 2,
        verification_tap_duration: track_duration,
    }
}

/// An engine running on a fake output, with every event it emitted so far.
pub struct EngineRun {
    pub handle: EngineHandle,
    pub controller: FakeOutputController,
    pub events: Vec<EngineEvent>,
}

/// Starts an engine on a `FakeOutput` with `config`, `preference` and `tuning`.
pub fn start_engine(
    config: FakeOutputConfig,
    preference: OutputModePreference,
    tuning: EngineTuning,
) -> EngineRun {
    start_engine_with_options(
        config,
        EngineStartOptions {
            output_mode_preference: preference,
            preferred_device_id: None,
        },
        tuning,
    )
}

pub fn start_engine_with_options(
    config: FakeOutputConfig,
    options: EngineStartOptions,
    tuning: EngineTuning,
) -> EngineRun {
    let (output, controller) = FakeOutput::new(config);
    let handle = EngineHandle::start_with_tuning(
        Box::new(output),
        DecoderRegistry::with_default_providers(),
        options,
        tuning,
    )
    .expect("the engine thread starts");
    EngineRun {
        handle,
        controller,
        events: Vec::new(),
    }
}

impl EngineRun {
    pub fn send(&self, command: EngineCommand) {
        self.handle.send(command);
    }

    /// Waits for the next event matching `predicate` (recording every event on the way).
    pub fn wait_for(
        &mut self,
        what: &str,
        predicate: impl Fn(&EngineEvent) -> bool,
    ) -> EngineEvent {
        let deadline = Instant::now() + EVENT_WAIT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let event = self
                .handle
                .receive_event_timeout(remaining)
                .unwrap_or_else(|| {
                    panic!("timed out waiting for {what}; events: {:#?}", self.events)
                });
            self.events.push(event.clone());
            if predicate(&event) {
                return event;
            }
        }
    }

    pub fn wait_for_state(&mut self, state: PlaybackState) {
        self.wait_for(&format!("state {state:?}"), |event| {
            matches!(event, EngineEvent::PlaybackStateChanged { state: reached } if *reached == state)
        });
    }

    /// Sends `OpenFileAndPlay` and waits for `TrackLoaded`.
    pub fn open(&mut self, path: &Path) -> TrackId {
        self.send(EngineCommand::OpenFileAndPlay {
            path: path.to_path_buf(),
        });
        match self.wait_for("TrackLoaded", |event| {
            matches!(event, EngineEvent::TrackLoaded { .. })
        }) {
            EngineEvent::TrackLoaded { track_id, .. } => track_id,
            _ => unreachable!("filtered above"),
        }
    }

    /// Opens `path` and waits until the track ends (`Stopped`).
    pub fn play_to_end(&mut self, path: &Path) -> TrackId {
        let track_id = self.open(path);
        self.wait_for_state(PlaybackState::Stopped);
        track_id
    }

    /// Waits for the next stream to open and returns its assessment.
    pub fn wait_for_stream(&mut self) -> SignalPathAssessment {
        match self.wait_for("an open stream", |event| {
            matches!(
                event,
                EngineEvent::SignalPathChanged {
                    assessment: Some(_)
                }
            )
        }) {
            EngineEvent::SignalPathChanged {
                assessment: Some(assessment),
            } => assessment,
            _ => unreachable!("filtered above"),
        }
    }

    /// The latest verification status published for `track_id`.
    pub fn final_status(&self, track_id: TrackId) -> VerificationStatus {
        self.events
            .iter()
            .rev()
            .find_map(|event| match event {
                EngineEvent::VerificationChanged {
                    track_id: event_track,
                    status,
                } if *event_track == track_id => Some(status.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no verification status; events: {:#?}", self.events))
    }

    pub fn failures(&self) -> Vec<PlaybackFailureKind> {
        self.events
            .iter()
            .filter_map(|event| match event {
                EngineEvent::PlaybackFailed { kind, .. } => Some(*kind),
                _ => None,
            })
            .collect()
    }
}

/// `pack_samples_into_container(original PCM)` in `format`.
pub fn packed(samples: &[i32], source_bits: u8, format: &DeviceSampleFormat) -> Vec<u8> {
    let mut bytes = vec![0; samples.len() * usize::from(format.container_bytes)];
    pack_samples_into_container(samples, source_bits, format, &mut bytes);
    bytes
}

/// The recorded bytes are exactly `expected`, followed only by silence (the end-of-stream
/// tail the device kept playing until the engine closed the stream).
pub fn assert_played_exactly(played: &[u8], expected: &[u8]) {
    assert!(
        played.len() >= expected.len(),
        "played {} bytes, expected at least {}",
        played.len(),
        expected.len()
    );
    let (audio, tail) = played.split_at(expected.len());
    if let Some(first_difference) = audio.iter().zip(expected).position(|(a, b)| a != b) {
        panic!("played bytes differ from the expected bytes at byte {first_difference}");
    }
    assert!(
        tail.iter().all(|byte| *byte == 0),
        "only silence after the track"
    );
}

/// MD5 of `samples` in the FLAC canonical layout (RFC 9639 section 8.2).
pub fn canonical_md5(samples: &[i32], bits_per_sample: u8) -> [u8; 16] {
    let bytes_per_sample = usize::from(bits_per_sample).div_ceil(8);
    let mut hasher = Md5::new();
    for sample in samples {
        hasher.update(&sample.to_le_bytes()[..bytes_per_sample]);
    }
    hasher.finalize().into()
}

/// One cell of the B10.3 matrix: a mode and the one render format the fake accepts in it.
#[derive(Clone, Copy, Debug)]
pub struct MatrixCase {
    pub preference: OutputModePreference,
    pub render_format: DeviceSampleFormat,
}

/// Every lossless candidate of `source`, in both modes (B10.3 #1 and #2).
pub fn matrix_cases(source: &StreamFormat) -> Vec<MatrixCase> {
    let candidates = lossless_device_format_candidates(source, ValidBitsAlignment::MostSignificant);
    [
        OutputModePreference::Exclusive,
        OutputModePreference::Shared,
    ]
    .into_iter()
    .flat_map(|preference| {
        candidates.iter().map(move |render_format| MatrixCase {
            preference,
            render_format: *render_format,
        })
    })
    .collect()
}

/// A fake that accepts only `case.render_format`, in `case`'s mode.
pub fn config_forcing(case: &MatrixCase) -> FakeOutputConfig {
    let forced = AcceptedFormats::Only(vec![FormatChoice::of(&case.render_format)]);
    match case.preference {
        OutputModePreference::Exclusive => FakeOutputConfig {
            exclusive_formats: forced,
            shared_formats: AcceptedFormats::Nothing,
            ..FakeOutputConfig::default()
        },
        OutputModePreference::Shared => FakeOutputConfig {
            exclusive_formats: AcceptedFormats::Nothing,
            shared_formats: forced,
            ..FakeOutputConfig::default()
        },
    }
}
