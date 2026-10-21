//! Plan B10.3 #7: `DeviceFeeder::begin_stream` and `DeviceFeeder::render` never allocate (or
//! free) memory, on any path.
//!
//! A counting `#[global_allocator]` wraps the system allocator. It counts only calls made on
//! the thread that armed it, so other test-harness threads cannot make the result flaky. The
//! test then drives the feeder through every path while armed: normal, ring wrap-around,
//! variable buffer lengths, underrun, stale-generation discard, end of stream (with its
//! `stream_ends_reached` count), format boundary, tap full (bytes and events), shared mode, reopen with another container, a
//! partial trailing frame, and `render` before `begin_stream`. Setup (ring pushes, tap drains)
//! runs unarmed.

// A global allocator needs `unsafe impl GlobalAlloc`; this test binary is its only user.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use pistolame_core::{
    ChannelMask, DeviceAccessMode, DeviceFeeder, DeviceSampleFormat, PlaybackCounters,
    PlaybackRingWriter, QueuedTrack, RenderOutcome, RenderSource, RenderStreamDescription,
    SampleBlockHeader, StreamFormat, TapEvent, TrackId, ValidBitsAlignment, VerificationTapReader,
    create_playback_ring, create_track_chain, create_verification_tap,
};

static ALLOCATOR_CALLS_WHILE_ARMED: AtomicU64 = AtomicU64::new(0);

thread_local! {
    // Const-initialized and without destructor: reading it never allocates.
    static IS_ARMED_ON_THIS_THREAD: Cell<bool> = const { Cell::new(false) };
}

fn count_if_armed() {
    if IS_ARMED_ON_THIS_THREAD.try_with(Cell::get).unwrap_or(false) {
        ALLOCATOR_CALLS_WHILE_ARMED.fetch_add(1, Ordering::Relaxed);
    }
}

struct CountingAllocator;

// SAFETY: every method forwards its arguments unchanged to the system allocator and returns its
// result, so this allocator upholds exactly the `GlobalAlloc` contract `System` upholds.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_if_armed();
        // SAFETY: the caller's `layout` meets `alloc`'s preconditions; passed through unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count_if_armed();
        // SAFETY: as in `alloc`.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count_if_armed();
        // SAFETY: `pointer` was allocated by `System` (every allocation goes through it) with
        // `layout`; the caller guarantees `new_size` is valid. Passed through unchanged.
        unsafe { System.realloc(pointer, layout, new_size) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        count_if_armed();
        // SAFETY: `pointer` was allocated by `System` with `layout`. Passed through unchanged.
        unsafe { System.dealloc(pointer, layout) }
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

/// Runs `action` with counting armed on this thread.
fn armed<Output>(action: impl FnOnce() -> Output) -> Output {
    IS_ARMED_ON_THIS_THREAD.with(|is_armed| is_armed.set(true));
    let output = action();
    IS_ARMED_ON_THIS_THREAD.with(|is_armed| is_armed.set(false));
    output
}

const SAMPLE_RATE_HZ: u32 = 1_000;
const CHANNEL_COUNT: u8 = 2;
const BLOCK_FRAMES: u32 = 7;
/// Largest buffer the test renders; the destination is allocated once with this size.
const MAX_BUFFER_FRAMES: usize = 9;

fn source_format(sample_rate_hz: u32) -> StreamFormat {
    StreamFormat {
        sample_rate_hz,
        bits_per_sample: 24,
        channel_count: CHANNEL_COUNT,
        channel_mask: ChannelMask(0b11),
    }
}

fn render_format(container_bytes: u8, valid_bits_per_sample: u8) -> DeviceSampleFormat {
    DeviceSampleFormat {
        sample_rate_hz: SAMPLE_RATE_HZ,
        container_bytes,
        valid_bits_per_sample,
        alignment: ValidBitsAlignment::MostSignificant,
        channel_count: CHANNEL_COUNT,
        channel_mask: ChannelMask(0b11),
    }
}

fn description(
    format: DeviceSampleFormat,
    access_mode: DeviceAccessMode,
) -> RenderStreamDescription {
    RenderStreamDescription {
        render_format: format,
        access_mode,
    }
}

/// Everything the scenario needs, allocated up front.
struct Scenario {
    ring_writer: PlaybackRingWriter,
    tap_reader: VerificationTapReader,
    feeder: DeviceFeeder,
    flush_generation: Arc<AtomicU64>,
    is_playback_queue_finished: Arc<AtomicBool>,
    counters: Arc<PlaybackCounters>,
    block_samples: Vec<i32>,
    destination: Vec<u8>,
    track_id: TrackId,
    next_frame_index: u64,
    seen_outcomes: [bool; 4],
    render_calls: u64,
}

fn outcome_slot(outcome: RenderOutcome) -> usize {
    match outcome {
        RenderOutcome::Filled => 0,
        RenderOutcome::FilledWithUnderrun => 1,
        RenderOutcome::FormatBoundaryReached => 2,
        RenderOutcome::EndOfStream => 3,
    }
}

impl Scenario {
    fn new(tap_duration: Duration) -> Self {
        Self::with_counters(tap_duration, Arc::new(PlaybackCounters::new()))
    }

    fn with_counters(tap_duration: Duration, counters: Arc<PlaybackCounters>) -> Self {
        // 50 ms of ring: blocks of 7 frames wrap the samples ring every few blocks.
        let (ring_writer, ring_reader) =
            create_playback_ring(&source_format(SAMPLE_RATE_HZ), Duration::from_millis(50));
        let (tap_writer, tap_reader) =
            create_verification_tap(SAMPLE_RATE_HZ, 4 * usize::from(CHANNEL_COUNT), tap_duration);
        let flush_generation = Arc::new(AtomicU64::new(0));
        let is_playback_queue_finished = Arc::new(AtomicBool::new(false));
        let feeder = DeviceFeeder::new(
            ring_reader,
            tap_writer,
            Arc::clone(&flush_generation),
            Arc::clone(&is_playback_queue_finished),
            Arc::clone(&counters),
        );
        let sample_count = BLOCK_FRAMES as usize * usize::from(CHANNEL_COUNT);
        Self {
            ring_writer,
            tap_reader,
            feeder,
            flush_generation,
            is_playback_queue_finished,
            counters,
            block_samples: (0..sample_count as i32)
                .map(|value| value * 1_001 - 7_000)
                .collect(),
            destination: vec![0; MAX_BUFFER_FRAMES * 4 * usize::from(CHANNEL_COUNT)],
            track_id: TrackId(1),
            next_frame_index: 0,
            seen_outcomes: [false; 4],
            render_calls: 0,
        }
    }

    /// Pushes blocks (unarmed) while the ring has room. With `is_last_block_track_end`, the
    /// last block that fits ends the track.
    fn fill_ring(&mut self, is_last_block_track_end: bool) {
        let block_frames = BLOCK_FRAMES as usize;
        while self.ring_writer.free_frames(CHANNEL_COUNT) >= block_frames {
            let is_track_end = is_last_block_track_end
                && self.ring_writer.free_frames(CHANNEL_COUNT) < 2 * block_frames;
            self.push_block(source_format(SAMPLE_RATE_HZ), is_track_end);
        }
    }

    fn push_block(&mut self, format: StreamFormat, is_track_end: bool) {
        let header = SampleBlockHeader {
            track_id: self.track_id,
            flush_generation: self.flush_generation.load(Ordering::Acquire),
            format,
            first_frame_index: self.next_frame_index,
            frame_count: BLOCK_FRAMES,
            is_track_start: self.next_frame_index == 0,
            is_track_end,
        };
        self.ring_writer
            .try_push_block(header, &self.block_samples)
            .expect("caller checked for room");
        self.next_frame_index += u64::from(BLOCK_FRAMES);
    }

    /// Ends the current track with an empty end marker and moves on to the next track.
    fn change_track(&mut self) {
        let end_marker = SampleBlockHeader {
            track_id: self.track_id,
            flush_generation: self.flush_generation.load(Ordering::Acquire),
            format: source_format(SAMPLE_RATE_HZ),
            first_frame_index: self.next_frame_index,
            frame_count: 0,
            is_track_start: false,
            is_track_end: true,
        };
        // A full ring only means the marker is skipped; the next track still starts.
        let _ = self.ring_writer.try_push_block(end_marker, &[]);
        self.track_id = TrackId(self.track_id.0 + 1);
        self.next_frame_index = 0;
    }

    /// Shared-mode helper: a new one-block track, ended, rendered in one armed call.
    fn play_short_track(&mut self, bytes_per_frame: usize) {
        self.track_id = TrackId(self.track_id.0 + 1);
        self.next_frame_index = 0;
        self.push_block(source_format(SAMPLE_RATE_HZ), true);
        self.render(MAX_BUFFER_FRAMES, bytes_per_frame, 0);
    }

    /// Plays the verifier (unarmed): empties both tap rings.
    fn drain_tap(&mut self) {
        while self.tap_reader.pop_event().is_some() {}
        let mut scratch = [0u8; 64];
        while self.tap_reader.pop_device_bytes(&mut scratch).is_ok() {}
        while self.tap_reader.pop_device_bytes(&mut scratch[..1]).is_ok() {}
    }

    /// One armed `render` of `frame_count` frames (plus `extra_bytes` to break whole frames).
    fn render(&mut self, frame_count: usize, bytes_per_frame: usize, extra_bytes: usize) {
        let length = frame_count * bytes_per_frame + extra_bytes;
        let destination = &mut self.destination[..length];
        let feeder = &mut self.feeder;
        let outcome = armed(|| feeder.render(destination));
        self.seen_outcomes[outcome_slot(outcome)] = true;
        self.render_calls += 1;
    }

    fn begin_stream(&mut self, format: DeviceSampleFormat, access_mode: DeviceAccessMode) {
        let feeder = &mut self.feeder;
        armed(|| feeder.begin_stream(description(format, access_mode)));
    }

    /// Normal playback with buffer lengths 1..=9 frames, refilling the ring and draining the
    /// tap only every `tap_drain_interval` calls so the tap fills up in between.
    fn play(&mut self, call_count: u64, bytes_per_frame: usize, tap_drain_interval: u64) {
        for call_index in 0..call_count {
            self.fill_ring(false);
            if call_index % tap_drain_interval == 0 {
                self.drain_tap();
            }
            let frame_count = (call_index as usize % MAX_BUFFER_FRAMES) + 1;
            self.render(frame_count, bytes_per_frame, 0);
        }
    }

    fn drain_ring(&mut self, bytes_per_frame: usize) {
        for _ in 0..200 {
            self.render(MAX_BUFFER_FRAMES, bytes_per_frame, 0);
            self.drain_tap();
        }
    }
}

#[test]
fn begin_stream_and_render_never_allocate_on_any_path() {
    // The counter must see an allocation, or a zero below would prove nothing.
    armed(|| drop(std::hint::black_box(Vec::<u8>::with_capacity(16))));
    assert_eq!(ALLOCATOR_CALLS_WHILE_ARMED.swap(0, Ordering::Relaxed), 2);

    let three_bytes = render_format(3, 24);
    let four_bytes = render_format(4, 32);
    let mut scenario = Scenario::new(Duration::from_millis(20));

    // Contract violation: render before begin_stream.
    scenario.render(4, three_bytes.bytes_per_frame(), 0);
    scenario.begin_stream(three_bytes, DeviceAccessMode::Exclusive);
    // Normal, wrap-around, variable lengths, tap full in between drains.
    scenario.play(400, three_bytes.bytes_per_frame(), 10);
    // A partial trailing frame.
    scenario.render(3, three_bytes.bytes_per_frame(), 2);
    // Underrun: empty ring.
    scenario.drain_ring(three_bytes.bytes_per_frame());
    // Stale generation: blocks of generation N, then a bump before they play.
    for _ in 0..50 {
        scenario.fill_ring(false);
        scenario.flush_generation.fetch_add(1, Ordering::AcqRel);
        scenario.track_id = TrackId(scenario.track_id.0 + 1);
        scenario.next_frame_index = 0;
        scenario.fill_ring(false);
        scenario.render(MAX_BUFFER_FRAMES, three_bytes.bytes_per_frame(), 0);
        scenario.drain_tap();
    }
    // End of stream: the engine says the queue is finished.
    scenario.drain_ring(three_bytes.bytes_per_frame());
    scenario.fill_ring(true);
    scenario
        .is_playback_queue_finished
        .store(true, Ordering::Release);
    scenario.drain_ring(three_bytes.bytes_per_frame());
    scenario
        .is_playback_queue_finished
        .store(false, Ordering::Release);
    // Gapless track change, then a late next track (underrun after a track end).
    scenario.play(30, three_bytes.bytes_per_frame(), 3);
    scenario.change_track();
    scenario.play(30, three_bytes.bytes_per_frame(), 3);
    scenario.change_track();
    scenario.drain_ring(three_bytes.bytes_per_frame());
    // Shared mode: the tap is off.
    scenario.begin_stream(three_bytes, DeviceAccessMode::SharedThroughSystemMixer);
    scenario.play(200, three_bytes.bytes_per_frame(), 1);
    // Reopen in exclusive mode with another container.
    scenario.begin_stream(four_bytes, DeviceAccessMode::Exclusive);
    scenario.play(200, four_bytes.bytes_per_frame(), 7);
    // Format boundary: a block at another rate.
    scenario.drain_ring(four_bytes.bytes_per_frame());
    scenario.push_block(source_format(SAMPLE_RATE_HZ * 2), true);
    for _ in 0..50 {
        scenario.render(MAX_BUFFER_FRAMES, four_bytes.bytes_per_frame(), 0);
    }

    // Event ring full, nothing held yet: in shared mode each short track adds a
    // SegmentStarted and a TrackEnded and no device bytes. The tap is never drained, so the
    // event ring fills, then the pending queue, until the first event is dropped.
    let mut event_flood = Scenario::new(Duration::from_secs(1));
    event_flood.begin_stream(three_bytes, DeviceAccessMode::SharedThroughSystemMixer);
    let mut short_track_count = 0;
    while event_flood
        .counters
        .snapshot()
        .verification_tap_overflow_events
        == 0
    {
        assert!(short_track_count < 10_000, "the event ring never filled");
        event_flood.play_short_track(three_bytes.bytes_per_frame());
        short_track_count += 1;
    }
    // While frozen: a stream start is held, a second one replaces it, later events drop.
    event_flood.begin_stream(four_bytes, DeviceAccessMode::Exclusive);
    event_flood.begin_stream(three_bytes, DeviceAccessMode::SharedThroughSystemMixer);
    for _ in 0..5 {
        event_flood.play_short_track(three_bytes.bytes_per_frame());
    }
    assert_eq!(
        ALLOCATOR_CALLS_WHILE_ARMED.load(Ordering::Relaxed),
        0,
        "the feeder allocated or freed memory on the real-time path"
    );
    // The paths above were really taken.
    assert!(scenario.render_calls + event_flood.render_calls >= 1_000);
    assert_eq!(
        scenario.seen_outcomes, [true; 4],
        "every RenderOutcome seen"
    );
    let snapshot = scenario.counters.snapshot();
    assert!(snapshot.underrun_events > 0);
    assert!(snapshot.stale_frames_discarded > 0);
    assert!(
        snapshot.stream_ends_reached > 0,
        "the stream-end count ran armed"
    );
    assert!(snapshot.verification_tap_overflow_events > 0);
    assert!(snapshot.render_contract_violations >= 2);

    // The queued, held and replaced paths really ran: once the verifier drains the full event
    // ring, the feeder flushes queued segment and track events, then the latest stream start,
    // then the overflow marker. The replaced stream start never appears.
    let ring_events: Vec<TapEvent> =
        std::iter::from_fn(|| event_flood.tap_reader.pop_event()).collect();
    let mut flushed_events = Vec::new();
    loop {
        let is_flushed = event_flood.feeder.flush_verification_events();
        flushed_events.extend(std::iter::from_fn(|| event_flood.tap_reader.pop_event()));
        if is_flushed {
            break;
        }
    }
    let replaced_stream_start = TapEvent::StreamBegan {
        render_format: four_bytes,
        access_mode: DeviceAccessMode::Exclusive,
    };
    assert!(!ring_events.contains(&replaced_stream_start));
    assert!(!flushed_events.contains(&replaced_stream_start));
    assert!(
        flushed_events.len() >= 2,
        "a stream start and a marker were held"
    );
    let (queued_events, held_events) = flushed_events.split_at(flushed_events.len() - 2);
    assert!(
        !queued_events.is_empty(),
        "events waited in the pending queue"
    );
    assert!(queued_events.iter().all(|event| matches!(
        event,
        TapEvent::SegmentStarted { .. } | TapEvent::TrackEnded { .. }
    )));
    assert_eq!(
        held_events[0],
        TapEvent::StreamBegan {
            render_format: three_bytes,
            access_mode: DeviceAccessMode::SharedThroughSystemMixer,
        }
    );
    assert!(matches!(
        held_events[1],
        TapEvent::TapOverflowed { track_id: Some(_) }
    ));
}

/// One whole track in its own ring, ended and marked decoded, for the track chain test.
fn ended_track(track_id: TrackId, counters: &Arc<PlaybackCounters>) -> (DeviceFeeder, Scenario) {
    let mut track = Scenario::with_counters(Duration::from_secs(1), Arc::clone(counters));
    track.track_id = track_id;
    track.fill_ring(true);
    track
        .is_playback_queue_finished
        .store(true, Ordering::Release);
    // The scenario keeps the ring writer and tap reader alive; its feeder goes to the chain.
    let feeder = std::mem::replace(
        &mut track.feeder,
        Scenario::new(Duration::from_millis(20)).feeder,
    );
    (feeder, track)
}

/// `TrackChain::begin_stream` and `render` never allocate or free either: switching to the
/// queued track in the middle of a buffer, retiring a track taken back, and ending the stream.
#[test]
fn track_chain_switches_never_allocate() {
    let format = render_format(3, 24);
    let bytes_per_frame = format.bytes_per_frame();
    let counters = Arc::new(PlaybackCounters::new());
    let (first_feeder, _first) = ended_track(TrackId(1), &counters);
    let (second_feeder, _second) = ended_track(TrackId(2), &counters);
    let (taken_back_feeder, _taken_back) = ended_track(TrackId(3), &counters);
    let (mut chain, mut control) =
        create_track_chain(first_feeder, TrackId(1), source_format(SAMPLE_RATE_HZ));
    let mut destination = vec![0u8; MAX_BUFFER_FRAMES * bytes_per_frame];
    let mut seen_end_of_stream = false;
    let mut render = |chain: &mut pistolame_core::TrackChain, destination: &mut [u8]| {
        let outcome = armed(|| chain.render(destination));
        seen_end_of_stream |= outcome == RenderOutcome::EndOfStream;
    };

    assert!(control.queue_next(QueuedTrack {
        track_id: TrackId(2),
        feeder: second_feeder,
    }));
    armed(|| chain.begin_stream(description(format, DeviceAccessMode::Exclusive)));
    // 7-frame blocks in 9-frame buffers: the first track ends inside a buffer.
    for _ in 0..1_000 {
        if counters.snapshot().current_track_id == 2 {
            break;
        }
        render(&mut chain, &mut destination);
    }
    let finished = control.take_retired().expect("the first track finished");
    assert!(finished.was_played);
    // A track queued, then taken back before the second one ends.
    assert!(control.queue_next(QueuedTrack {
        track_id: TrackId(3),
        feeder: taken_back_feeder,
    }));
    assert!(control.cancel_next(TrackId(3)));
    for _ in 0..200 {
        render(&mut chain, &mut destination);
    }

    assert_eq!(
        ALLOCATOR_CALLS_WHILE_ARMED.load(Ordering::Relaxed),
        0,
        "the track chain allocated or freed memory on the real-time path"
    );
    let taken_back = control.take_retired().expect("the track taken back");
    assert!(!taken_back.was_played && taken_back.track_id == TrackId(3));
    assert!(seen_end_of_stream);
    assert_eq!(counters.snapshot().stream_ends_reached, 1);
}
