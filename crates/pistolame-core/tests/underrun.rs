//! Plan B10.3 #4: the unpaced fake starves a deliberately small ring. Underruns are counted,
//! silence is inserted, the audio bytes around it are still exact, and level 1 is still
//! `Verified` (underrun silence is never tapped).

#![cfg(feature = "test-support")]

mod support;

use std::time::Duration;

use pistolame_core::testing::{CD_STEREO, FakeOutputConfig, generate_fixture};
use pistolame_core::{EngineTuning, OutputModePreference, TransmissionStatus};
use support::{packed, start_engine};

/// Drops every all-zero frame: the inserted silence, and the generator's one all-zero frame
/// (the forced extreme values include a 0 on every channel), on both sides alike.
fn without_silent_frames(bytes: &[u8], bytes_per_frame: usize) -> Vec<u8> {
    bytes
        .chunks(bytes_per_frame)
        .filter(|frame| frame.iter().any(|byte| *byte != 0))
        .flatten()
        .copied()
        .collect()
}

#[test]
fn a_starved_ring_inserts_counted_silence_and_keeps_the_audio_exact_and_verified() {
    let fixture = generate_fixture(&CD_STEREO);
    let tuning = EngineTuning {
        playback_ring_duration: Duration::from_millis(50),
        start_prefill_duration: Duration::from_millis(20),
        verification_tap_duration: Duration::from_secs(5),
    };
    let mut run = start_engine(
        FakeOutputConfig::default(),
        OutputModePreference::Exclusive,
        tuning,
    );
    let track_id = run.play_to_end(&fixture.path);

    let counters = run.handle.counters();
    assert!(counters.underrun_events > 0, "the ring was starved");
    assert!(counters.silent_frames_inserted > 0);
    let render_format = run.controller.negotiated_streams()[0].render_format;
    let bytes_per_frame = render_format.bytes_per_frame();
    let played = run.controller.played_bytes();
    let expected = packed(&fixture.original_interleaved_pcm, 16, &render_format);
    assert!(played.len() > expected.len(), "silence was inserted");
    assert_eq!(
        without_silent_frames(&played, bytes_per_frame),
        without_silent_frames(&expected, bytes_per_frame)
    );
    assert_eq!(
        run.final_status(track_id).transmission,
        TransmissionStatus::Verified {
            first_frame_index: 0,
            frame_count: CD_STEREO.frame_count() as u64,
            md5: fixture.streaminfo_md5
        }
    );
}
