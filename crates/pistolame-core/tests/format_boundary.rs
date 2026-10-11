//! Plan B10.3 #6: two blocks with different formats pushed into a ring by hand. The feeder must
//! return `FormatBoundaryReached` and never pack the second block in the first block's format.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::time::Duration;

use pistolame_core::{
    ChannelMask, DeviceAccessMode, DeviceFeeder, DeviceSampleFormat, PlaybackCounters,
    RenderOutcome, RenderSource, RenderStreamDescription, SampleBlockHeader, StreamFormat,
    TapEvent, TrackId, ValidBitsAlignment, create_playback_ring, create_verification_tap,
    pack_samples_into_container,
};

const CHANNEL_COUNT: u8 = 2;
const TRACK: TrackId = TrackId(1);
const NEXT_TRACK: TrackId = TrackId(2);

fn source_format(sample_rate_hz: u32) -> StreamFormat {
    StreamFormat {
        sample_rate_hz,
        bits_per_sample: 16,
        channel_count: CHANNEL_COUNT,
        channel_mask: ChannelMask(0b11),
    }
}

fn render_format(sample_rate_hz: u32) -> DeviceSampleFormat {
    DeviceSampleFormat {
        sample_rate_hz,
        container_bytes: 2,
        valid_bits_per_sample: 16,
        alignment: ValidBitsAlignment::MostSignificant,
        channel_count: CHANNEL_COUNT,
        channel_mask: ChannelMask(0b11),
    }
}

fn header(track_id: TrackId, format: StreamFormat, frame_count: u32) -> SampleBlockHeader {
    SampleBlockHeader {
        track_id,
        flush_generation: 0,
        format,
        first_frame_index: 0,
        frame_count,
        is_track_start: true,
        is_track_end: true,
    }
}

fn packed(samples: &[i32], format: &DeviceSampleFormat) -> Vec<u8> {
    let mut bytes = vec![0; samples.len() * usize::from(format.container_bytes)];
    pack_samples_into_container(samples, 16, format, &mut bytes);
    bytes
}

#[test]
fn feeder_stops_at_a_format_boundary_and_resumes_after_reopen() {
    let first_format = source_format(44_100);
    let second_format = source_format(48_000);
    let (mut ring_writer, ring_reader) =
        create_playback_ring(&first_format, Duration::from_secs(1));
    let (tap_writer, mut tap_reader) = create_verification_tap(
        48_000,
        4 * usize::from(CHANNEL_COUNT),
        Duration::from_secs(1),
    );
    let counters = Arc::new(PlaybackCounters::new());
    let mut feeder = DeviceFeeder::new(
        ring_reader,
        tap_writer,
        Arc::new(AtomicU64::new(0)),
        Arc::new(AtomicBool::new(false)),
        Arc::clone(&counters),
    );
    let first_samples = [100, -100, 200, -200];
    let second_samples = [7, -7, 8, -8, 9, -9];
    ring_writer
        .try_push_block(header(TRACK, first_format, 2), &first_samples)
        .expect("room");
    ring_writer
        .try_push_block(header(NEXT_TRACK, second_format, 3), &second_samples)
        .expect("room");

    let first_render_format = render_format(44_100);
    feeder.begin_stream(RenderStreamDescription {
        render_format: first_render_format,
        access_mode: DeviceAccessMode::Exclusive,
    });
    let mut destination = vec![0xAA; 5 * first_render_format.bytes_per_frame()];
    assert_eq!(
        feeder.render(&mut destination),
        RenderOutcome::FormatBoundaryReached
    );
    let mut expected = packed(&first_samples, &first_render_format);
    expected.resize(destination.len(), 0);
    assert_eq!(destination, expected, "first block, then silence only");

    // Still at the boundary: nothing of the second block is ever packed at 44.1 kHz.
    destination.fill(0xAA);
    assert_eq!(
        feeder.render(&mut destination),
        RenderOutcome::FormatBoundaryReached
    );
    assert!(destination.iter().all(|byte| *byte == 0));

    // The engine reopens at the new rate; the same feeder continues with the second block.
    let second_render_format = render_format(48_000);
    feeder.begin_stream(RenderStreamDescription {
        render_format: second_render_format,
        access_mode: DeviceAccessMode::Exclusive,
    });
    let mut destination = vec![0xAA; 3 * second_render_format.bytes_per_frame()];
    assert_eq!(feeder.render(&mut destination), RenderOutcome::Filled);
    assert_eq!(destination, packed(&second_samples, &second_render_format));
    assert_eq!(counters.snapshot().underrun_events, 0);

    assert_eq!(
        counters.snapshot().format_boundaries_reached,
        1,
        "one boundary, counted once"
    );

    // Both tracks are announced and closed; only their own frames are tapped, each in the
    // layout of the stream it played in.
    let events: Vec<TapEvent> = std::iter::from_fn(|| tap_reader.pop_event()).collect();
    let first_byte_count = 2 * first_render_format.bytes_per_frame();
    let second_byte_count = 3 * second_render_format.bytes_per_frame();
    assert_eq!(
        events,
        vec![
            TapEvent::StreamBegan {
                render_format: first_render_format,
                access_mode: DeviceAccessMode::Exclusive,
            },
            TapEvent::SegmentStarted {
                track_id: TRACK,
                flush_generation: 0,
                first_frame_index: 0,
            },
            TapEvent::DeviceBytes {
                track_id: TRACK,
                first_frame_index: 0,
                frame_count: 2,
                byte_count: first_byte_count,
            },
            TapEvent::TrackEnded { track_id: TRACK },
            TapEvent::StreamBegan {
                render_format: second_render_format,
                access_mode: DeviceAccessMode::Exclusive,
            },
            TapEvent::SegmentStarted {
                track_id: NEXT_TRACK,
                flush_generation: 0,
                first_frame_index: 0,
            },
            TapEvent::DeviceBytes {
                track_id: NEXT_TRACK,
                first_frame_index: 0,
                frame_count: 3,
                byte_count: second_byte_count,
            },
            TapEvent::TrackEnded {
                track_id: NEXT_TRACK
            },
        ]
    );
    let mut first_tapped = vec![0; first_byte_count];
    tap_reader
        .pop_device_bytes(&mut first_tapped)
        .expect("first track bytes");
    assert_eq!(first_tapped, packed(&first_samples, &first_render_format));
    let mut second_tapped = vec![0; second_byte_count];
    tap_reader
        .pop_device_bytes(&mut second_tapped)
        .expect("second track bytes");
    assert_eq!(
        second_tapped,
        packed(&second_samples, &second_render_format)
    );
    assert!(
        tap_reader.pop_device_bytes(&mut [0u8; 1]).is_err(),
        "boundary silence is never tapped"
    );
}
