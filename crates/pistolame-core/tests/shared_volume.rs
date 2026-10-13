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
