//! `TrackChain`: the render source that plays several tracks of the same format in one open
//! stream, without a gap (AGENTS.md: "Same format → keep streaming").
//!
//! Each track keeps its own pipeline and `DeviceFeeder` (ring, tap, verifier). The chain holds
//! the feeder of the track playing now; the engine queues the feeder of the next track through
//! `TrackChainControl`. When the current track is drained (its last block rendered and its
//! decode thread finished), the chain begins the stream on the next feeder and goes on in the
//! same buffer. The finished feeder goes back to the engine, which tears its pipeline down.
//!
//! Real-time rules, for `begin_stream` and `render`: no allocation, no lock, no I/O. Feeders
//! move through two preallocated `rtrb` rings, and nothing is ever dropped on the real-time
//! thread: a feeder leaves the chain only when the ring of retired feeders has room for it.
//!
//! The engine can take a queued track back (the user changed what plays next): it clears the
//! accepted track id, and the chain retires the queued feeder unplayed instead of switching to
//! it. Both sides decide with one compare-exchange on that id: the chain claims the track
//! before it switches, the engine clears it only if not claimed, so exactly one of them wins
//! and the engine knows which. The chain never switches once the end of the stream was
//! counted: the engine has then seen the track end and is closing the stream.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rtrb::{Consumer, Producer, RingBuffer};

use crate::feeder::{DeviceFeeder, EndOfTrackHandling};
use crate::format::StreamFormat;
use crate::output_contract::{RenderOutcome, RenderSource, RenderStreamDescription};
use crate::playback_ring::TrackId;

/// Tracks queued ahead of the current one. The engine prepares one next track at a time.
const QUEUED_TRACK_CAPACITY: usize = 1;
/// Feeders waiting for the engine to take them back: one finished and one taken back can both
/// wait for the same engine poll, with room to spare so a switch is never refused for lack of
/// room while the engine is a poll late.
const RETIRED_FEEDER_CAPACITY: usize = 4;
/// Accepted track id meaning "no next track": track ids start at 1 (`EngineThread`).
const NO_ACCEPTED_TRACK: u64 = 0;
/// Accepted track id once the chain claimed the queued track to switch to it: the engine can no
/// longer take it back. No track id reaches it (ids count up from 1).
const CLAIMED_TRACK: u64 = u64::MAX;

/// A track the engine queued to follow the current one.
pub struct QueuedTrack {
    /// The track's id, compared with the accepted id before the chain switches to it.
    pub track_id: TrackId,
    /// Its feeder, never given a stream yet.
    pub feeder: DeviceFeeder,
}

/// A feeder the chain is done with, handed back to the engine.
pub struct RetiredFeeder {
    /// The track the feeder played (or would have played).
    pub track_id: TrackId,
    /// The feeder, so the engine can service it while tearing its pipeline down.
    pub feeder: DeviceFeeder,
    /// `true`: the track was played to its end and the chain went on with the next one.
    /// `false`: the engine took the queued track back before it played.
    pub was_played: bool,
}

/// Real-time side: the render source given to the stream.
pub struct TrackChain {
    current: DeviceFeeder,
    current_track_id: TrackId,
    queued_tracks: Consumer<QueuedTrack>,
    retired_feeders: Producer<RetiredFeeder>,
    accepted_next_track_id: Arc<AtomicU64>,
    /// The stream being fed, given to the next feeder when the chain switches to it.
    stream_description: Option<RenderStreamDescription>,
}

/// Engine side: queues the next track and takes finished feeders back.
pub struct TrackChainControl {
    queued_tracks: Producer<QueuedTrack>,
    retired_feeders: Consumer<RetiredFeeder>,
    accepted_next_track_id: Arc<AtomicU64>,
    /// Format of the first track, which the stream was opened for: only tracks of exactly this
    /// format may follow in the same stream.
    chained_format: StreamFormat,
}

/// A chain starting with `first_feeder`, the feeder of track `first_track_id` of format
/// `chained_format`.
pub fn create_track_chain(
    first_feeder: DeviceFeeder,
    first_track_id: TrackId,
    chained_format: StreamFormat,
) -> (TrackChain, TrackChainControl) {
    let (queued_producer, queued_consumer) = RingBuffer::new(QUEUED_TRACK_CAPACITY);
    let (retired_producer, retired_consumer) = RingBuffer::new(RETIRED_FEEDER_CAPACITY);
    let accepted_next_track_id = Arc::new(AtomicU64::new(NO_ACCEPTED_TRACK));
    let chain = TrackChain {
        current: first_feeder,
        current_track_id: first_track_id,
        queued_tracks: queued_consumer,
        retired_feeders: retired_producer,
        accepted_next_track_id: Arc::clone(&accepted_next_track_id),
        stream_description: None,
    };
    let control = TrackChainControl {
        queued_tracks: queued_producer,
        retired_feeders: retired_consumer,
        accepted_next_track_id,
        chained_format,
    };
    (chain, control)
}

impl TrackChainControl {
    /// The only format a queued track may have.
    pub fn chained_format(&self) -> StreamFormat {
        self.chained_format
    }

    /// `false` while a track taken back still waits in the queue (the chain retires it at its
    /// next render or service). Once `true`, it stays so until `queue_next`: the chain only
    /// ever removes tracks.
    pub fn can_queue_next(&self) -> bool {
        !self.queued_tracks.is_full()
    }

    /// Queues `track` to follow the current one; call only when `can_queue_next`. `false` if
    /// the queue was full anyway (the track is then dropped here, off the real-time thread).
    pub fn queue_next(&mut self, track: QueuedTrack) -> bool {
        // Accepted before it is pushed (Release), so the chain never sees it as taken back.
        self.accepted_next_track_id
            .store(track.track_id.0, Ordering::Release);
        self.queued_tracks.push(track).is_ok()
    }

    /// Takes queued track `track_id` back: the chain will retire it unplayed. `false` when the
    /// chain already claimed it to switch to it: it plays (or played), and `take_retired` soon
    /// hands back the track it finished, so the engine must keep the track's pipeline.
    pub fn cancel_next(&self, track_id: TrackId) -> bool {
        match self.accepted_next_track_id.compare_exchange(
            track_id.0,
            NO_ACCEPTED_TRACK,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => true,
            Err(accepted_track_id) => accepted_track_id != CLAIMED_TRACK,
        }
    }

    /// The next feeder the chain is done with, oldest first.
    pub fn take_retired(&mut self) -> Option<RetiredFeeder> {
        self.retired_feeders.pop().ok()
    }
}

impl TrackChain {
    /// Retires a queued track the engine took back, when there is room to hand it back.
    fn retire_cancelled_track(&mut self) {
        let accepted_track_id = self.accepted_next_track_id.load(Ordering::Acquire);
        let is_cancelled = self
            .queued_tracks
            .peek()
            .is_ok_and(|queued| queued.track_id.0 != accepted_track_id);
        if !is_cancelled || self.retired_feeders.is_full() {
            return;
        }
        if let Ok(queued) = self.queued_tracks.pop() {
            self.retire(RetiredFeeder {
                track_id: queued.track_id,
                feeder: queued.feeder,
                was_played: false,
            });
        }
    }

    /// Makes the queued track current, if it is still accepted and the stream has begun.
    /// `false` when there is nothing to switch to.
    fn switch_to_next_track(&mut self) -> bool {
        self.retire_cancelled_track();
        let Some(description) = self.stream_description else {
            return false;
        };
        if self.retired_feeders.is_full() {
            return false;
        }
        let Ok(queued_track_id) = self.queued_tracks.peek().map(|queued| queued.track_id.0) else {
            return false;
        };
        // Claimed before it is taken: a cancel after this point loses and keeps the pipeline.
        let is_claimed = self
            .accepted_next_track_id
            .compare_exchange(
                queued_track_id,
                CLAIMED_TRACK,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok();
        if !is_claimed {
            self.retire_cancelled_track();
            return false;
        }
        let Ok(mut next) = self.queued_tracks.pop() else {
            return false;
        };
        next.feeder.begin_stream(description);
        let finished_feeder = std::mem::replace(&mut self.current, next.feeder);
        let finished_track_id = std::mem::replace(&mut self.current_track_id, next.track_id);
        self.retire(RetiredFeeder {
            track_id: finished_track_id,
            feeder: finished_feeder,
            was_played: true,
        });
        true
    }

    /// Callers check `is_full` first, and the chain is the only producer, so the push cannot
    /// fail. Were it to, the feeder would be dropped here; that frees nothing (its pipeline
    /// still owns the other end of each ring and every shared counter), so it stays
    /// real-time safe.
    fn retire(&mut self, retired: RetiredFeeder) {
        let _ = self.retired_feeders.push(retired);
    }
}

impl RenderSource for TrackChain {
    /// Remembers the stream for the next feeder and begins it on the current one.
    fn begin_stream(&mut self, description: RenderStreamDescription) {
        self.stream_description = Some(description);
        self.current.begin_stream(description);
    }

    /// Renders the current track; when it is drained, switches to the queued track and renders
    /// the rest of `destination` from it. Without a queued track, the end of the stream is
    /// counted as for a single feeder. Bounded: each switch consumes one queued track.
    fn render(&mut self, destination: &mut [u8]) -> RenderOutcome {
        self.retire_cancelled_track();
        let mut rendered_byte_count = 0;
        loop {
            let Some(remaining) = destination.get_mut(rendered_byte_count..) else {
                return RenderOutcome::FilledWithUnderrun;
            };
            let (audio_frame_count, outcome) = self
                .current
                .render_with(remaining, EndOfTrackHandling::ReportDrained);
            if outcome != RenderOutcome::EndOfStream {
                return outcome;
            }
            let audio_byte_count =
                audio_frame_count * self.current.bytes_per_frame().unwrap_or_default();
            if self.current.has_reached_end_of_stream() || !self.switch_to_next_track() {
                self.current.enter_end_of_stream();
                return RenderOutcome::EndOfStream;
            }
            rendered_byte_count += audio_byte_count;
        }
    }

    /// Retires a queued track taken back (nothing renders while closed), then services the
    /// current feeder.
    fn service_while_closed(&mut self) -> bool {
        self.retire_cancelled_track();
        self.current.service_while_closed()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    use super::*;
    use crate::engine::PlaybackCounters;
    use crate::format::{ChannelMask, DeviceSampleFormat, ValidBitsAlignment};
    use crate::output_contract::DeviceAccessMode;
    use crate::pcm::pack_samples_into_container;
    use crate::playback_ring::{PlaybackRingWriter, SampleBlockHeader, create_playback_ring};
    use crate::verification::TapEvent;
    use crate::verification::tap::{VerificationTapReader, create_verification_tap};

    const SAMPLE_RATE_HZ: u32 = 1_000;
    const CHANNEL_COUNT: u8 = 2;
    const FIRST: TrackId = TrackId(1);
    const SECOND: TrackId = TrackId(2);

    fn source_format() -> StreamFormat {
        StreamFormat {
            sample_rate_hz: SAMPLE_RATE_HZ,
            bits_per_sample: 24,
            channel_count: CHANNEL_COUNT,
            channel_mask: ChannelMask(0b11),
        }
    }

    fn render_format() -> DeviceSampleFormat {
        DeviceSampleFormat {
            sample_rate_hz: SAMPLE_RATE_HZ,
            container_bytes: 3,
            valid_bits_per_sample: 24,
            alignment: ValidBitsAlignment::MostSignificant,
            channel_count: CHANNEL_COUNT,
            channel_mask: ChannelMask(0b11),
        }
    }

    /// One track's ring, tap and end flag, with its feeder until it is given to a chain.
    struct TrackFeed {
        track_id: TrackId,
        ring_writer: PlaybackRingWriter,
        tap_reader: VerificationTapReader,
        is_playback_queue_finished: Arc<AtomicBool>,
        feeder: Option<DeviceFeeder>,
    }

    fn track_feed(track_id: TrackId, counters: &Arc<PlaybackCounters>) -> TrackFeed {
        let (ring_writer, ring_reader) =
            create_playback_ring(&source_format(), Duration::from_secs(1));
        let (tap_writer, tap_reader) =
            create_verification_tap(SAMPLE_RATE_HZ, 4 * 2, Duration::from_secs(1));
        let is_playback_queue_finished = Arc::new(AtomicBool::new(false));
        let feeder = DeviceFeeder::new(
            ring_reader,
            tap_writer,
            Arc::new(AtomicU64::new(0)),
            Arc::clone(&is_playback_queue_finished),
            Arc::clone(counters),
        );
        TrackFeed {
            track_id,
            ring_writer,
            tap_reader,
            is_playback_queue_finished,
            feeder: Some(feeder),
        }
    }

    impl TrackFeed {
        /// Pushes the whole track (`frame_count` frames, distinct values per track) and its end
        /// marker, then marks the decoding finished. Returns the samples pushed.
        fn push_whole_track(&mut self, frame_count: u32) -> Vec<i32> {
            let offset = i32::try_from(self.track_id.0).expect("small id") * 100_000;
            let samples: Vec<i32> = (0..frame_count * u32::from(CHANNEL_COUNT))
                .map(|index| offset + i32::try_from(index).expect("small index") * 7)
                .collect();
            let header =
                |first_frame_index: u64, frame_count: u32, is_track_end: bool| SampleBlockHeader {
                    track_id: self.track_id,
                    flush_generation: 0,
                    format: source_format(),
                    first_frame_index,
                    frame_count,
                    is_track_start: first_frame_index == 0,
                    is_track_end,
                };
            self.ring_writer
                .try_push_block(header(0, frame_count, false), &samples)
                .expect("ring has room");
            self.ring_writer
                .try_push_block(header(u64::from(frame_count), 0, true), &[])
                .expect("ring has room");
            self.is_playback_queue_finished
                .store(true, Ordering::Release);
            samples
        }

        fn tap_events(&mut self) -> Vec<TapEvent> {
            std::iter::from_fn(|| self.tap_reader.pop_event()).collect()
        }

        fn queued(&mut self) -> QueuedTrack {
            QueuedTrack {
                track_id: self.track_id,
                feeder: self.feeder.take().expect("feeder not given yet"),
            }
        }
    }

    struct ChainHarness {
        counters: Arc<PlaybackCounters>,
        first: TrackFeed,
        second: TrackFeed,
        chain: TrackChain,
        control: TrackChainControl,
    }

    fn chain_harness() -> ChainHarness {
        let counters = Arc::new(PlaybackCounters::new());
        let mut first = track_feed(FIRST, &counters);
        let second = track_feed(SECOND, &counters);
        let (chain, control) = create_track_chain(
            first.feeder.take().expect("fresh feeder"),
            FIRST,
            source_format(),
        );
        ChainHarness {
            counters,
            first,
            second,
            chain,
            control,
        }
    }

    impl ChainHarness {
        fn begin(&mut self) {
            self.chain.begin_stream(RenderStreamDescription {
                render_format: render_format(),
                access_mode: DeviceAccessMode::Exclusive,
            });
        }

        fn render(&mut self, frame_count: usize) -> (Vec<u8>, RenderOutcome) {
            let mut destination = vec![0xAA; frame_count * render_format().bytes_per_frame()];
            let outcome = self.chain.render(&mut destination);
            (destination, outcome)
        }
    }

    fn packed(samples: &[i32]) -> Vec<u8> {
        let format = render_format();
        let mut bytes = vec![0; samples.len() * usize::from(format.container_bytes)];
        pack_samples_into_container(samples, 24, &format, &mut bytes);
        bytes
    }

    #[test]
    fn the_next_track_continues_in_the_same_buffer_without_a_gap() {
        let mut harness = chain_harness();
        let mut expected = harness.first.push_whole_track(3);
        expected.extend(harness.second.push_whole_track(2));
        let queued = harness.second.queued();
        assert!(harness.control.queue_next(queued));
        harness.begin();

        let (bytes, outcome) = harness.render(5);

        assert_eq!(outcome, RenderOutcome::Filled);
        assert_eq!(bytes, packed(&expected));
        assert_eq!(harness.counters.snapshot().stream_ends_reached, 0);
        assert_eq!(harness.counters.snapshot().current_track_id, SECOND.0);
        let retired = harness
            .control
            .take_retired()
            .expect("first feeder retired");
        assert!(retired.was_played && retired.track_id == FIRST);
        assert!(
            harness
                .second
                .tap_events()
                .iter()
                .any(|event| matches!(event, TapEvent::StreamBegan { .. })),
            "the next feeder began the stream before rendering"
        );
        assert!(
            harness
                .first
                .tap_events()
                .contains(&TapEvent::TrackEnded { track_id: FIRST })
        );
    }

    #[test]
    fn the_last_track_ends_the_stream_once() {
        let mut harness = chain_harness();
        let expected = harness.first.push_whole_track(3);
        harness.begin();

        let (bytes, outcome) = harness.render(5);

        assert_eq!(outcome, RenderOutcome::EndOfStream);
        let mut with_silence = packed(&expected);
        with_silence.resize(5 * render_format().bytes_per_frame(), 0);
        assert_eq!(bytes, with_silence);
        assert_eq!(harness.render(4).1, RenderOutcome::EndOfStream);
        assert_eq!(harness.counters.snapshot().stream_ends_reached, 1);
        assert!(harness.control.take_retired().is_none());
    }

    #[test]
    fn a_track_queued_after_the_end_was_counted_never_plays() {
        let mut harness = chain_harness();
        harness.first.push_whole_track(3);
        harness.second.push_whole_track(2);
        harness.begin();
        assert_eq!(harness.render(5).1, RenderOutcome::EndOfStream);

        let queued = harness.second.queued();
        assert!(harness.control.queue_next(queued));
        let (bytes, outcome) = harness.render(2);

        assert_eq!(outcome, RenderOutcome::EndOfStream);
        assert!(bytes.iter().all(|byte| *byte == 0));
        assert_eq!(harness.counters.snapshot().stream_ends_reached, 1);
        assert!(harness.control.take_retired().is_none());
    }

    #[test]
    fn a_track_taken_back_is_retired_unplayed() {
        let mut harness = chain_harness();
        harness.first.push_whole_track(3);
        harness.second.push_whole_track(2);
        let queued = harness.second.queued();
        assert!(harness.control.queue_next(queued));
        assert!(harness.control.cancel_next(SECOND));
        harness.begin();

        assert_eq!(harness.render(5).1, RenderOutcome::EndOfStream);

        let retired = harness.control.take_retired().expect("taken back");
        assert!(!retired.was_played && retired.track_id == SECOND);
        assert_eq!(harness.counters.snapshot().stream_ends_reached, 1);
    }

    #[test]
    fn a_track_taken_back_while_closed_frees_the_queue_for_another() {
        let mut harness = chain_harness();
        let queued = harness.second.queued();
        assert!(harness.control.queue_next(queued));
        assert!(harness.control.cancel_next(SECOND));
        let mut third = track_feed(TrackId(3), &harness.counters);
        assert!(
            !harness.control.can_queue_next(),
            "the queue still holds the track taken back"
        );

        harness.chain.service_while_closed();

        assert!(
            harness
                .control
                .take_retired()
                .is_some_and(|retired| !retired.was_played)
        );
        assert!(harness.control.can_queue_next());
        assert!(harness.control.queue_next(third.queued()));
    }

    /// The switch claimed the track first: the cancel loses and says so, so the engine keeps
    /// the pipeline that now plays.
    #[test]
    fn a_cancel_after_the_switch_loses() {
        let mut harness = chain_harness();
        harness.first.push_whole_track(3);
        harness.second.push_whole_track(2);
        let queued = harness.second.queued();
        assert!(harness.control.queue_next(queued));
        harness.begin();
        assert_eq!(harness.render(5).1, RenderOutcome::Filled);

        assert!(!harness.control.cancel_next(SECOND));
        assert!(
            harness
                .control
                .take_retired()
                .is_some_and(|retired| retired.was_played && retired.track_id == FIRST)
        );
    }

    #[test]
    fn an_underrun_in_the_current_track_never_switches() {
        let mut harness = chain_harness();
        harness.second.push_whole_track(2);
        let queued = harness.second.queued();
        assert!(harness.control.queue_next(queued));
        harness.begin();

        assert_eq!(harness.render(2).1, RenderOutcome::FilledWithUnderrun);

        assert!(harness.control.take_retired().is_none());
        assert_eq!(harness.counters.snapshot().stream_ends_reached, 0);
    }
}
