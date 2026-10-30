//! The shared-mode volume reaches the open stream, and every stream opened later.

#![cfg(feature = "test-support")]

mod support;

use pistolame_core::testing::{CD_STEREO, FakeOutputConfig, generate_fixture};
use pistolame_core::{EngineCommand, OutputModePreference, SharedModeVolume};
use support::{start_engine, whole_track_tuning};

#[test]
fn the_volume_reaches_the_open_stream() {
    let fixture = generate_fixture(&CD_STEREO);
    let mut run = start_engine(
        FakeOutputConfig::default(),
        OutputModePreference::Shared,
        whole_track_tuning(&CD_STEREO),
    );
    run.open(&fixture.path);
    run.wait_for_stream();
    run.send(EngineCommand::SetSharedModeVolume {
        volume: SharedModeVolume::at_decibels(-20.0),
    });
    run.send(EngineCommand::SetSharedModeVolume {
        volume: SharedModeVolume {
            is_muted: true,
            ..SharedModeVolume::default()
        },
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let calls = loop {
        let calls = run.controller.shared_mode_volume_calls();
        if calls.len() >= 3 || std::time::Instant::now() > deadline {
            break calls;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    // Unity at open, then -20 dB (0.1), then mute (0).
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert_eq!(calls[0], 1.0);
    assert!((calls[1] - 0.1).abs() < 1e-6);
    assert_eq!(calls[2], 0.0);
}

/// Regression: a pause and a resume reopen the stream, and WASAPI prefills the device buffer
/// while opening. The volume must be known by then, or the prefill sounds at full level.
#[test]
fn a_stream_reopened_after_a_pause_starts_at_the_chosen_volume() {
    let fixture = generate_fixture(&CD_STEREO);
    let mut run = start_engine(
        FakeOutputConfig::default(),
        OutputModePreference::Shared,
        whole_track_tuning(&CD_STEREO),
    );
    run.send(EngineCommand::SetSharedModeVolume {
        volume: SharedModeVolume::at_decibels(-20.0),
    });
    run.open(&fixture.path);
    run.wait_for_stream();
    run.send(EngineCommand::Pause);
    run.wait_for_state(pistolame_core::PlaybackState::Paused);
    run.send(EngineCommand::Play);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let gains = loop {
        let gains = run.controller.initial_shared_mode_gains();
        if gains.len() >= 2 || std::time::Instant::now() > deadline {
            break gains;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert_eq!(gains.len(), 2, "{gains:?}");
    assert!(
        gains.iter().all(|gain| (gain - 0.1).abs() < 1e-6),
        "{gains:?}"
    );
}

/// A seek while playing keeps the device: no reopen, so no audible stop.
#[test]
fn a_seek_while_playing_keeps_the_stream_open() {
    let fixture = generate_fixture(&CD_STEREO);
    let mut run = start_engine(
        FakeOutputConfig::default(),
        OutputModePreference::Shared,
        whole_track_tuning(&CD_STEREO),
    );
    run.open(&fixture.path);
    run.wait_for_stream();
    run.send(EngineCommand::SeekTo {
        position_in_track: std::time::Duration::from_millis(100),
    });
    run.send(EngineCommand::Pause);
    run.wait_for_state(pistolame_core::PlaybackState::Paused);
    assert_eq!(run.controller.initial_shared_mode_gains().len(), 1);
}
