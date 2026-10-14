//! The output device choice through the engine and a `FakeOutput`: "Sortie du système" follows
//! the system default while playing (shared and exclusive), a specific device ignores it, a
//! device in use that disappears is never replaced by another one, and a backend without
//! notifications is re-read at the next open.
//!
//! Stepping: the fake holds after a given number of `render` calls, so every device change lands
//! mid-track at a known point; every wait is an event wait with a timeout.

#![cfg(feature = "test-support")]

mod support;

use pistolame_core::testing::{CD_STEREO, FakeOutputConfig, generate_fixture};
use pistolame_core::{
    DeviceAccessMode, EngineCommand, EngineEvent, EngineStartOptions, OutputDeviceId,
    OutputDeviceInfo, OutputDeviceSelection, OutputModePreference, PlaybackState,
    TransmissionStatus,
};
use support::{
    EVENT_WAIT, EngineRun, assert_played_exactly, packed, start_engine_with_options,
    whole_track_tuning,
};

const SPEAKERS: &str = "speakers";
const HEADPHONES: &str = "headphones";
/// Frames per fake `render` call (10 ms at 44.1 kHz).
const BUFFER_FRAMES: u32 = 441;
/// `render` calls before the fake holds: well inside the 3 s fixture.
const RENDER_CALLS_BEFORE_THE_CHANGE: u64 = 30;

fn device_id(id: &str) -> OutputDeviceId {
    OutputDeviceId(id.to_owned())
}

fn device(id: &str, display_name: &str, is_system_default: bool) -> OutputDeviceInfo {
    OutputDeviceInfo {
        id: device_id(id),
        display_name: display_name.to_owned(),
        is_system_default,
    }
}

/// The built-in speakers and plugged-in headphones; `default_id` is the system default.
fn speakers_and_headphones(default_id: &str) -> Vec<OutputDeviceInfo> {
    vec![
        device(
            SPEAKERS,
            "Haut-parleurs MacBook Air",
            default_id == SPEAKERS,
        ),
        device(HEADPHONES, "Écouteurs externes", default_id == HEADPHONES),
    ]
}

fn make_default(devices: &mut [OutputDeviceInfo], default_id: &str) {
    for device in devices {
        device.is_system_default = device.id.0 == default_id;
    }
}

/// Room for three buffers in the fake device, so a reopen always has frames queued to play out.
fn config(default_id: &str, can_watch_device_changes: bool) -> FakeOutputConfig {
    FakeOutputConfig {
        devices: speakers_and_headphones(default_id),
        buffer_frame_count: BUFFER_FRAMES,
        queued_frame_capacity: 3 * BUFFER_FRAMES,
        can_watch_device_changes,
        ..FakeOutputConfig::default()
    }
}

fn start(
    config: FakeOutputConfig,
    selection: OutputDeviceSelection,
    preference: OutputModePreference,
) -> EngineRun {
    start_engine_with_options(
        config,
        EngineStartOptions {
            output_mode_preference: preference,
            device_selection: selection,
            shared_mode_volume: Default::default(),
        },
        whole_track_tuning(&CD_STEREO),
    )
}

fn wait_until_held(run: &EngineRun) {
    assert!(
        run.controller.wait_until_held(EVENT_WAIT),
        "the fake device holds"
    );
}

/// Waits for a device list whose resolved device is `expected`.
fn wait_for_selected(run: &mut EngineRun, expected: &str) {
    let expected = device_id(expected);
    run.wait_for("a device list selecting the expected device", |event| {
        matches!(
            event,
            EngineEvent::DeviceListChanged { selected: Some(selected), .. } if *selected == expected
        )
    });
}

fn opened_device_ids(run: &EngineRun) -> Vec<String> {
    run.controller
        .negotiated_streams()
        .into_iter()
        .map(|negotiated| negotiated.device.id.0)
        .collect()
}

/// The user's bug: playing on the speakers under "Sortie du système", headphones get plugged
/// in and become the default. The stream plays out on the speakers and reopens on the
/// headphones at the first unplayed frame, in the same mode; nothing is lost or played twice.
fn follow_the_new_default_mid_track(preference: OutputModePreference) {
    let fixture = generate_fixture(&CD_STEREO);
    let mut run = start(
        config(SPEAKERS, true),
        OutputDeviceSelection::SystemDefault,
        preference,
    );
    run.controller
        .hold_after_more_render_calls(RENDER_CALLS_BEFORE_THE_CHANGE);
    let track_id = run.open(&fixture.path);
    assert_eq!(
        run.wait_for_stream().negotiated.device.id,
        device_id(SPEAKERS)
    );
    wait_until_held(&run);

    assert!(
        run.controller
            .change_devices(|devices| make_default(devices, HEADPHONES)),
        "the engine registered a device-change listener"
    );
    wait_for_selected(&mut run, HEADPHONES);
    let moved = run.wait_for_stream();
    assert_eq!(moved.negotiated.device.id, device_id(HEADPHONES));
    assert_eq!(moved.negotiated.device.display_name, "Écouteurs externes");
    let expected_mode = match preference {
        OutputModePreference::Exclusive => DeviceAccessMode::Exclusive,
        OutputModePreference::Shared => DeviceAccessMode::SharedThroughSystemMixer,
    };
    assert_eq!(moved.negotiated.access_mode, expected_mode);

    run.controller.release();
    run.wait_for_state(PlaybackState::Stopped);
    assert_eq!(opened_device_ids(&run), vec![SPEAKERS, HEADPHONES]);
    let streams = run.controller.negotiated_streams();
    assert_eq!(streams[0].render_format, streams[1].render_format);
    assert_played_exactly(
        &run.controller.played_bytes(),
        &packed(
            &fixture.original_interleaved_pcm,
            16,
            &streams[0].render_format,
        ),
    );
    let transmission = run.final_status(track_id).transmission;
    match preference {
        OutputModePreference::Exclusive => assert!(
            matches!(transmission, TransmissionStatus::Verified { .. }),
            "bit-perfect across the device move: {transmission:?}"
        ),
        OutputModePreference::Shared => {
            assert_eq!(transmission, TransmissionStatus::NotApplicableInSharedMode)
        }
    }
}

#[test]
fn a_new_system_default_mid_track_moves_a_shared_stream_to_it() {
    follow_the_new_default_mid_track(OutputModePreference::Shared);
}

#[test]
fn a_new_system_default_mid_track_moves_an_exclusive_stream_to_it() {
    follow_the_new_default_mid_track(OutputModePreference::Exclusive);
}

#[test]
fn the_system_default_choice_starts_on_the_current_default() {
    let mut run = start(
        config(HEADPHONES, true),
        OutputDeviceSelection::SystemDefault,
        OutputModePreference::Shared,
    );
    let list = run.wait_for("the start-up device list", |event| {
        matches!(event, EngineEvent::DeviceListChanged { .. })
    });
    assert!(matches!(
        list,
        EngineEvent::DeviceListChanged {
            selection: OutputDeviceSelection::SystemDefault,
            selected: Some(selected),
            ..
        } if selected == device_id(HEADPHONES)
    ));
}

#[test]
fn a_specific_device_ignores_a_new_system_default() {
    let fixture = generate_fixture(&CD_STEREO);
    let mut run = start(
        config(SPEAKERS, true),
        OutputDeviceSelection::Specific(device_id(SPEAKERS)),
        OutputModePreference::Shared,
    );
    run.controller
        .hold_after_more_render_calls(RENDER_CALLS_BEFORE_THE_CHANGE);
    run.open(&fixture.path);
    run.wait_for_stream();
    wait_until_held(&run);

    assert!(
        run.controller
            .change_devices(|devices| make_default(devices, HEADPHONES))
    );
    wait_for_selected(&mut run, SPEAKERS);
    run.controller.release();
    run.wait_for_state(PlaybackState::Stopped);
    assert_eq!(opened_device_ids(&run), vec![SPEAKERS]);
}

/// Headphones unplugged while playing on them: the default moves to the speakers, but the
/// stream is not moved there (no music suddenly out of the speakers); the device failure stops
/// playback as before (B8).
#[test]
fn a_device_in_use_that_disappears_is_not_replaced() {
    let fixture = generate_fixture(&CD_STEREO);
    let mut run = start(
        config(HEADPHONES, true),
        OutputDeviceSelection::SystemDefault,
        OutputModePreference::Shared,
    );
    run.controller
        .hold_after_more_render_calls(RENDER_CALLS_BEFORE_THE_CHANGE);
    run.open(&fixture.path);
    run.wait_for_stream();
    wait_until_held(&run);

    assert!(run.controller.change_devices(|devices| {
        devices.retain(|device| device.id.0 != HEADPHONES);
        make_default(devices, SPEAKERS);
    }));
    wait_for_selected(&mut run, SPEAKERS);
    // Commands are handled after the notification, so the stop proves no reopen happened.
    run.send(EngineCommand::Stop);
    run.wait_for_state(PlaybackState::Stopped);
    assert_eq!(opened_device_ids(&run), vec![HEADPHONES]);
}

/// A backend without notifications: the default moved while paused, and the resume opens on
/// the new default because the engine re-reads the devices before opening.
#[test]
fn without_notifications_the_default_is_read_again_at_the_next_open() {
    let fixture = generate_fixture(&CD_STEREO);
    let mut run = start(
        config(SPEAKERS, false),
        OutputDeviceSelection::SystemDefault,
        OutputModePreference::Exclusive,
    );
    run.controller
        .hold_after_more_render_calls(RENDER_CALLS_BEFORE_THE_CHANGE);
    run.open(&fixture.path);
    run.wait_for_stream();
    wait_until_held(&run);
    run.send(EngineCommand::Pause);
    run.wait_for_state(PlaybackState::Paused);

    assert!(
        !run.controller
            .change_devices(|devices| make_default(devices, HEADPHONES)),
        "no listener was registered"
    );
    run.controller.release();
    run.send(EngineCommand::Play);
    assert_eq!(
        run.wait_for_stream().negotiated.device.id,
        device_id(HEADPHONES)
    );
    run.wait_for_state(PlaybackState::Stopped);
    let streams = run.controller.negotiated_streams();
    assert_played_exactly(
        &run.controller.played_bytes(),
        &packed(
            &fixture.original_interleaved_pcm,
            16,
            &streams[0].render_format,
        ),
    );
}

/// Choosing "Sortie du système" while playing on another device moves the stream at once.
#[test]
fn choosing_the_system_output_while_playing_moves_the_stream() {
    let fixture = generate_fixture(&CD_STEREO);
    let mut run = start(
        config(HEADPHONES, true),
        OutputDeviceSelection::Specific(device_id(SPEAKERS)),
        OutputModePreference::Shared,
    );
    run.controller
        .hold_after_more_render_calls(RENDER_CALLS_BEFORE_THE_CHANGE);
    run.open(&fixture.path);
    assert_eq!(
        run.wait_for_stream().negotiated.device.id,
        device_id(SPEAKERS)
    );
    wait_until_held(&run);

    run.send(EngineCommand::SelectOutputDevice {
        selection: OutputDeviceSelection::SystemDefault,
    });
    assert_eq!(
        run.wait_for_stream().negotiated.device.id,
        device_id(HEADPHONES)
    );
    run.controller.release();
    run.wait_for_state(PlaybackState::Stopped);
    assert_eq!(opened_device_ids(&run), vec![SPEAKERS, HEADPHONES]);
}
