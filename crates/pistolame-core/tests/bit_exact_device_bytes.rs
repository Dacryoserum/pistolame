//! Plan B10.3 #1: every fixture, in every render format the fake accepts (each candidate forced
//! in turn), in both modes, through the real engine, decoder and feeder. The bytes that reached
//! the fake device, without the trailing silence, equal the generator's PCM packed into the
//! render format.

#![cfg(feature = "test-support")]

mod support;

use pistolame_core::testing::{
    CD_STEREO, FixtureSpec, HIRES_44_1, HIRES_48, HIRES_88_2, HIRES_96, HIRES_192, ODD_20BIT,
    generate_fixture,
};
use pistolame_core::{DeviceAccessMode, OutputModePreference};
use support::{
    assert_played_exactly, config_forcing, matrix_cases, packed, start_engine, whole_track_tuning,
};

fn plays_exact_bytes_in_every_render_format(spec: &FixtureSpec) {
    let fixture = generate_fixture(spec);
    for case in matrix_cases(&fixture.format) {
        let mut run = start_engine(
            config_forcing(&case),
            case.preference,
            whole_track_tuning(spec),
        );
        run.play_to_end(&fixture.path);
        let negotiated = run.controller.negotiated_streams();
        assert_eq!(negotiated.len(), 1, "{case:?}");
        assert_eq!(negotiated[0].render_format, case.render_format);
        let expected_mode = match case.preference {
            OutputModePreference::Exclusive => DeviceAccessMode::Exclusive,
            OutputModePreference::Shared => DeviceAccessMode::SharedThroughSystemMixer,
        };
        assert_eq!(negotiated[0].access_mode, expected_mode);
        let expected = packed(
            &fixture.original_interleaved_pcm,
            fixture.format.bits_per_sample,
            &case.render_format,
        );
        assert_played_exactly(&run.controller.played_bytes(), &expected);
        assert_eq!(run.handle.counters().underrun_events, 0, "{case:?}");
    }
}

#[test]
fn cd_stereo_bytes_are_exact() {
    plays_exact_bytes_in_every_render_format(&CD_STEREO);
}

#[test]
fn hires_44_1_bytes_are_exact() {
    plays_exact_bytes_in_every_render_format(&HIRES_44_1);
}

#[test]
fn hires_48_bytes_are_exact() {
    plays_exact_bytes_in_every_render_format(&HIRES_48);
}

#[test]
fn hires_88_2_bytes_are_exact() {
    plays_exact_bytes_in_every_render_format(&HIRES_88_2);
}

#[test]
fn hires_96_bytes_are_exact() {
    plays_exact_bytes_in_every_render_format(&HIRES_96);
}

#[test]
fn hires_192_bytes_are_exact() {
    plays_exact_bytes_in_every_render_format(&HIRES_192);
}

#[test]
fn odd_20bit_bytes_are_exact() {
    plays_exact_bytes_in_every_render_format(&ODD_20BIT);
}
