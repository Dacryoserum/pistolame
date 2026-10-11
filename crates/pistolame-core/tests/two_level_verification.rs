//! Plan B10.3 #2: the same matrix as `bit_exact_device_bytes.rs`, checking both verification
//! levels. Exclusive: level 1 `Verified` over the whole track and level 2 `Verified` with the
//! fixture's STREAMINFO MD5. Shared: level 1 `NotApplicableInSharedMode`, level 2 `Verified`.

#![cfg(feature = "test-support")]

mod support;

use pistolame_core::testing::{
    CD_STEREO, FixtureSpec, HIRES_44_1, HIRES_48, HIRES_88_2, HIRES_96, HIRES_192, ODD_20BIT,
    generate_fixture,
};
use pistolame_core::{
    FileIntegrityStatus, OutputModePreference, TransmissionStatus, VerificationStatus,
};
use support::{config_forcing, matrix_cases, start_engine, whole_track_tuning};

fn both_levels_conclude_in_every_render_format(spec: &FixtureSpec) {
    let fixture = generate_fixture(spec);
    let total_frames = spec.frame_count() as u64;
    for case in matrix_cases(&fixture.format) {
        let mut run = start_engine(
            config_forcing(&case),
            case.preference,
            whole_track_tuning(spec),
        );
        let track_id = run.play_to_end(&fixture.path);
        let expected_transmission = match case.preference {
            OutputModePreference::Exclusive => TransmissionStatus::Verified {
                first_frame_index: 0,
                frame_count: total_frames,
                md5: fixture.streaminfo_md5,
            },
            OutputModePreference::Shared => TransmissionStatus::NotApplicableInSharedMode,
        };
        assert_eq!(
            run.final_status(track_id),
            VerificationStatus {
                transmission: expected_transmission,
                file_integrity: FileIntegrityStatus::Verified {
                    md5: fixture.streaminfo_md5
                },
            },
            "{case:?}"
        );
    }
}

#[test]
fn cd_stereo_is_verified_on_both_levels() {
    both_levels_conclude_in_every_render_format(&CD_STEREO);
}

#[test]
fn hires_44_1_is_verified_on_both_levels() {
    both_levels_conclude_in_every_render_format(&HIRES_44_1);
}

#[test]
fn hires_48_is_verified_on_both_levels() {
    both_levels_conclude_in_every_render_format(&HIRES_48);
}

#[test]
fn hires_88_2_is_verified_on_both_levels() {
    both_levels_conclude_in_every_render_format(&HIRES_88_2);
}

#[test]
fn hires_96_is_verified_on_both_levels() {
    both_levels_conclude_in_every_render_format(&HIRES_96);
}

#[test]
fn hires_192_is_verified_on_both_levels() {
    both_levels_conclude_in_every_render_format(&HIRES_192);
}

#[test]
fn odd_20bit_is_verified_on_both_levels() {
    both_levels_conclude_in_every_render_format(&ODD_20BIT);
}
