//! The verification tap: device bytes and `TapEvent`s travelling from the real-time render
//! thread to the verifier thread (plan B6, B7.2).
//!
//! Two lock-free single-producer/single-consumer rings (`rtrb`): one of `u8` device bytes and
//! one of `TapEvent`s. For each `TapEvent::DeviceBytes`, its `byte_count` bytes are pushed
//! before the event, so the verifier always finds them when it pops the event.
//!
//! Pushing never blocks and never allocates. When the verifier lags, the writer keeps, in this
//! order, what has not reached the event ring yet:
//! 1. a small fixed-size FIFO of events;
//! 2. a held `StreamBegan` (sticky: never dropped; a newer one replaces it, and the overflow
//!    marker then follows);
//! 3. a held `TapOverflowed` marker (sticky, coalesced).
//!
//! Once 2 or 3 is held, the writer is "frozen": other events and device bytes are dropped,
//! which the held marker already covers, until everything held has gone out. A marker never
//! overwrites another event, so no loss ever depends on which track a marker names.
//!
//! The verifier can therefore trust that, between two events it receives, nothing was lost
//! unless a `TapOverflowed` sits between them, and that the latest `StreamBegan` always arrives.

use std::time::Duration;

use crate::playback_ring::{TrackId, frames_in_duration};
use crate::verification::TapEvent;

/// Capacity of the event ring. A render call pushes one `DeviceBytes` per block it touches,
/// plus rare segment, track and stream markers. With 10 ms periods and blocks of about 4096
/// frames this is several seconds of events; with very small blocks the event ring can fill
/// before the bytes ring. Either way a full ring only leads to the marked overflow path.
const TAP_EVENT_RING_CAPACITY: usize = 4096;

/// Events that can wait for room in the event ring. Only a few non-bytes events can arrive
/// while the verifier lags (stream, segment and track markers), so a handful of slots is
/// enough; beyond it they are dropped under the overflow marker.
const PENDING_EVENT_CAPACITY: usize = 8;

/// Creates the two lock-free rings of the verification tap. The bytes ring holds `duration` of
/// frames at `sample_rate_hz` and `max_bytes_per_frame`.
///
/// - `sample_rate_hz`: the source rate. Every device candidate keeps the source rate (B3.1),
///   and a file at another rate gets a new ring, feeder and tap from the engine (B5.7), so one
///   tap never sees a higher rate than the one it was sized for.
/// - `max_bytes_per_frame`: the render format is only known at `begin_stream`, so size it for
///   the widest container: 4 bytes (the largest `container_bytes` that `validate_pcm_layout`
///   accepts) times the channel count.
///
/// Allocates; call off the real-time thread.
pub fn create_verification_tap(
    sample_rate_hz: u32,
    max_bytes_per_frame: usize,
    duration: Duration,
) -> (VerificationTapWriter, VerificationTapReader) {
    let byte_capacity =
        frames_in_duration(sample_rate_hz, duration).saturating_mul(max_bytes_per_frame.max(1));
    tap_with_capacities(byte_capacity, TAP_EVENT_RING_CAPACITY)
}

/// Builds a tap with explicit ring capacities (bytes, events); `create_verification_tap` sizes
/// them, tests shrink them.
pub(crate) fn tap_with_capacities(
    byte_capacity: usize,
    event_capacity: usize,
) -> (VerificationTapWriter, VerificationTapReader) {
    let (byte_producer, byte_consumer) = rtrb::RingBuffer::new(byte_capacity);
    let (event_producer, event_consumer) = rtrb::RingBuffer::new(event_capacity);
    (
        VerificationTapWriter {
            bytes: byte_producer,
            events: event_producer,
            queued_events: PendingTapEvents::default(),
            held_stream_began: None,
            held_overflow: None,
            is_latest_sent_event_overflow: false,
        },
        VerificationTapReader {
            bytes: byte_consumer,
            events: event_consumer,
        },
    )
}

/// What happened to something pushed into the tap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TapPushOutcome {
    /// It is in the rings.
    Pushed,
    /// The event waits and will go out in order.
    Deferred,
    /// It was dropped; a `TapOverflowed` follows it in the event order.
    Dropped,
}

/// A held overflow marker: the track of the first dropped item, if any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HeldOverflow {
    track_id: Option<TrackId>,
}

/// Small fixed-size FIFO of events waiting for room in the event ring. No allocation.
#[derive(Debug, Default)]
struct PendingTapEvents {
    events: [Option<TapEvent>; PENDING_EVENT_CAPACITY],
    length: usize,
}

impl PendingTapEvents {
    fn is_empty(&self) -> bool {
        self.length == 0
    }

    fn front(&self) -> Option<TapEvent> {
        self.events.first().copied().flatten()
    }

    fn pop_front(&mut self) {
        if self.length == 0 {
            return;
        }
        self.events.rotate_left(1);
        self.length -= 1;
        if let Some(freed_slot) = self.events.get_mut(self.length) {
            *freed_slot = None;
        }
    }

    /// Appends `event`; `false` if the queue is full.
    fn try_push_back(&mut self, event: TapEvent) -> bool {
        match self.events.get_mut(self.length) {
            Some(slot) => {
                *slot = Some(event);
                self.length += 1;
                true
            }
            None => false,
        }
    }
}

/// Render-thread side of the tap, owned by `DeviceFeeder`. Never blocks, never allocates.
pub struct VerificationTapWriter {
    bytes: rtrb::Producer<u8>,
    events: rtrb::Producer<TapEvent>,
    queued_events: PendingTapEvents,
    held_stream_began: Option<TapEvent>,
    held_overflow: Option<HeldOverflow>,
    /// The latest event that entered the event ring is a `TapOverflowed`, so a new drop before
    /// any other event needs no new marker.
    is_latest_sent_event_overflow: bool,
}

impl VerificationTapWriter {
    /// Pushes `event` after everything waiting. A `StreamBegan` is never dropped. Another event
    /// that cannot wait (queue full, or writer frozen) is dropped and a `TapOverflowed` naming
    /// that event's own track marks the loss.
    pub(crate) fn push_event(&mut self, event: TapEvent) -> TapPushOutcome {
        if self.flush_pending_events() && self.events.push(event).is_ok() {
            self.is_latest_sent_event_overflow = false;
            return TapPushOutcome::Pushed;
        }
        if !self.is_frozen() && self.queued_events.try_push_back(event) {
            return TapPushOutcome::Deferred;
        }
        if matches!(event, TapEvent::StreamBegan { .. }) {
            if self.held_stream_began.replace(event).is_some() {
                // The replaced stream start (maybe a shared-mode one) is lost: mark it. A
                // stream start belongs to no track.
                self.mark_overflow(None);
            }
            return TapPushOutcome::Deferred;
        }
        self.mark_overflow(track_of(&event));
        TapPushOutcome::Dropped
    }

    /// Copies `bytes` into the bytes ring, then pushes the `DeviceBytes` event describing them.
    /// All or nothing: if anything waiting cannot go out first, or either ring lacks room, the
    /// bytes are dropped and a `TapOverflowed` marks the loss.
    pub(crate) fn push_device_bytes(
        &mut self,
        bytes: &[u8],
        track_id: TrackId,
        first_frame_index: u64,
        frame_count: u32,
    ) -> TapPushOutcome {
        // Room for both the bytes and their event is checked before anything is committed.
        let has_room = self.flush_pending_events()
            && self.events.slots() >= 1
            && self.bytes.slots() >= bytes.len();
        if !has_room || self.bytes.push_entire_slice(bytes).is_err() {
            self.mark_overflow(Some(track_id));
            return TapPushOutcome::Dropped;
        }
        // Cannot fail, so no fallback: this writer is the only producer of the event ring, so
        // the free slot checked above is still free (the consumer can only add slots).
        let _ = self.events.push(TapEvent::DeviceBytes {
            track_id,
            first_frame_index,
            frame_count,
            byte_count: bytes.len(),
        });
        self.is_latest_sent_event_overflow = false;
        TapPushOutcome::Pushed
    }

    /// Moves everything waiting into the event ring, in order (queued events, held
    /// `StreamBegan`, held marker), while there is room. Returns whether nothing waits now.
    pub(crate) fn flush_pending_events(&mut self) -> bool {
        while let Some(event) = self.queued_events.front() {
            if self.events.push(event).is_err() {
                return false;
            }
            self.queued_events.pop_front();
            self.is_latest_sent_event_overflow = false;
        }
        if let Some(stream_began) = self.held_stream_began {
            if self.events.push(stream_began).is_err() {
                return false;
            }
            self.held_stream_began = None;
            self.is_latest_sent_event_overflow = false;
        }
        if let Some(held) = self.held_overflow {
            let marker = TapEvent::TapOverflowed {
                track_id: held.track_id,
            };
            if self.events.push(marker).is_err() {
                return false;
            }
            self.held_overflow = None;
            self.is_latest_sent_event_overflow = true;
        }
        true
    }

    /// Something waits in the held slots: later events must not overtake it.
    fn is_frozen(&self) -> bool {
        self.held_stream_began.is_some() || self.held_overflow.is_some()
    }

    fn is_nothing_waiting(&self) -> bool {
        self.queued_events.is_empty() && !self.is_frozen()
    }

    /// Records that something was lost (`track_id`: the track it belonged to, if any): a
    /// `TapOverflowed` enters the event order after everything already in it, unless the
    /// latest entry already is one. Also used by the feeder when it loses frames itself.
    pub(crate) fn mark_overflow(&mut self, track_id: Option<TrackId>) {
        if self.is_nothing_waiting() {
            if self.is_latest_sent_event_overflow {
                return;
            }
            let marker = TapEvent::TapOverflowed { track_id };
            if self.events.push(marker).is_ok() {
                self.is_latest_sent_event_overflow = true;
                return;
            }
        }
        if self.held_overflow.is_none() {
            self.held_overflow = Some(HeldOverflow { track_id });
        }
    }
}

/// The track an event belongs to; `None` for a stream start or an overflow marker without one.
fn track_of(event: &TapEvent) -> Option<TrackId> {
    match *event {
        TapEvent::StreamBegan { .. } => None,
        TapEvent::SegmentStarted { track_id, .. }
        | TapEvent::DeviceBytes { track_id, .. }
        | TapEvent::TrackEnded { track_id } => Some(track_id),
        TapEvent::TapOverflowed { track_id } => track_id,
    }
}

/// Why `VerificationTapReader::pop_device_bytes` read nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{requested_bytes} device bytes requested but only {available_bytes} are in the tap")]
pub struct TapBytesMissing {
    /// Bytes the caller asked for.
    pub requested_bytes: usize,
    /// Bytes currently in the ring.
    pub available_bytes: usize,
}

/// Verifier-thread side of the tap.
pub struct VerificationTapReader {
    bytes: rtrb::Consumer<u8>,
    events: rtrb::Consumer<TapEvent>,
}

impl VerificationTapReader {
    /// The oldest event not yet read, if any.
    pub fn pop_event(&mut self) -> Option<TapEvent> {
        self.events.pop().ok()
    }

    /// Reads exactly `destination.len()` device bytes. After popping a `DeviceBytes` event,
    /// read its `byte_count` bytes (laid out in the `render_format` of the latest
    /// `StreamBegan`): they were pushed before the event. Reads nothing if fewer are available
    /// (never expected; a sign of a protocol bug).
    pub fn pop_device_bytes(&mut self, destination: &mut [u8]) -> Result<(), TapBytesMissing> {
        self.bytes
            .pop_entire_slice(destination)
            .map_err(|_| TapBytesMissing {
                requested_bytes: destination.len(),
                available_bytes: self.bytes.slots(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{ChannelMask, DeviceSampleFormat, ValidBitsAlignment};
    use crate::output_contract::DeviceAccessMode;

    const TRACK: TrackId = TrackId(3);
    const OTHER_TRACK: TrackId = TrackId(4);

    fn ended(track_number: u64) -> TapEvent {
        TapEvent::TrackEnded {
            track_id: TrackId(track_number),
        }
    }

    fn overflowed(track_id: TrackId) -> TapEvent {
        TapEvent::TapOverflowed {
            track_id: Some(track_id),
        }
    }

    fn stream_began(container_bytes: u8) -> TapEvent {
        TapEvent::StreamBegan {
            render_format: DeviceSampleFormat {
                sample_rate_hz: 1_000,
                container_bytes,
                valid_bits_per_sample: container_bytes * 8,
                alignment: ValidBitsAlignment::MostSignificant,
                channel_count: 2,
                channel_mask: ChannelMask(0b11),
            },
            access_mode: DeviceAccessMode::Exclusive,
        }
    }

    /// Lets the verifier read one event at a time, the writer flushing in between.
    fn receive_all(
        writer: &mut VerificationTapWriter,
        reader: &mut VerificationTapReader,
    ) -> Vec<TapEvent> {
        let mut received = Vec::new();
        for _ in 0..64 {
            received.extend(reader.pop_event());
            writer.flush_pending_events();
        }
        received
    }

    #[test]
    fn byte_capacity_follows_rate_frame_size_and_duration() {
        // 10 ms at 1 kHz = 10 frames of 8 bytes.
        let (mut writer, _reader) = create_verification_tap(1_000, 8, Duration::from_millis(10));
        assert_eq!(
            writer.push_device_bytes(&[0; 80], TRACK, 0, 10),
            TapPushOutcome::Pushed
        );
        assert_eq!(
            writer.push_device_bytes(&[0; 8], TRACK, 10, 1),
            TapPushOutcome::Dropped
        );
    }

    #[test]
    fn device_bytes_carry_their_byte_count_and_are_readable_after_their_event() {
        let (mut writer, mut reader) = tap_with_capacities(16, 4);
        assert_eq!(
            writer.push_device_bytes(&[1, 2, 3, 4], TRACK, 9, 2),
            TapPushOutcome::Pushed
        );
        assert_eq!(
            reader.pop_event(),
            Some(TapEvent::DeviceBytes {
                track_id: TRACK,
                first_frame_index: 9,
                frame_count: 2,
                byte_count: 4
            })
        );
        let mut bytes = [0u8; 4];
        reader.pop_device_bytes(&mut bytes).expect("bytes pushed");
        assert_eq!(bytes, [1, 2, 3, 4]);
        assert_eq!(
            reader.pop_device_bytes(&mut bytes),
            Err(TapBytesMissing {
                requested_bytes: 4,
                available_bytes: 0
            })
        );
    }

    #[test]
    fn overflow_is_sticky_and_signaled_once_per_episode() {
        let (mut writer, mut reader) = tap_with_capacities(4, 8);
        assert_eq!(
            writer.push_device_bytes(&[0; 4], TRACK, 0, 1),
            TapPushOutcome::Pushed
        );
        // Bytes ring full: two drops, one marker.
        for frame_index in 1..3 {
            assert_eq!(
                writer.push_device_bytes(&[0; 4], TRACK, frame_index, 1),
                TapPushOutcome::Dropped
            );
        }
        let mut bytes = [0u8; 4];
        reader.pop_device_bytes(&mut bytes).expect("first bytes");
        assert_eq!(
            writer.push_device_bytes(&[0; 4], TRACK, 3, 1),
            TapPushOutcome::Pushed
        );
        let events = receive_all(&mut writer, &mut reader);
        assert_eq!(events.len(), 3);
        assert_eq!(events[1], overflowed(TRACK));
        assert!(matches!(
            events[2],
            TapEvent::DeviceBytes {
                first_frame_index: 3,
                ..
            }
        ));
    }

    #[test]
    fn events_wait_in_order_while_the_event_ring_is_full() {
        let (mut writer, mut reader) = tap_with_capacities(64, 1);
        assert_eq!(writer.push_event(ended(1)), TapPushOutcome::Pushed);
        assert_eq!(writer.push_event(ended(2)), TapPushOutcome::Deferred);
        assert_eq!(writer.push_event(ended(3)), TapPushOutcome::Deferred);
        // No room for the bytes' event: dropped, marker after the waiting events.
        assert_eq!(
            writer.push_device_bytes(&[0; 2], TRACK, 0, 1),
            TapPushOutcome::Dropped
        );
        assert_eq!(
            receive_all(&mut writer, &mut reader),
            vec![ended(1), ended(2), ended(3), overflowed(TRACK)]
        );
    }

    /// Two tracks lose data while the queue is full: no event is overwritten, and one marker
    /// (which taints every open segment) follows everything kept.
    #[test]
    fn a_full_queue_never_overwrites_an_event_with_a_marker() {
        let (mut writer, mut reader) = tap_with_capacities(64, 1);
        assert_eq!(writer.push_event(ended(0)), TapPushOutcome::Pushed);
        for track_number in 1..=PENDING_EVENT_CAPACITY as u64 {
            assert_eq!(
                writer.push_event(ended(track_number)),
                TapPushOutcome::Deferred
            );
        }
        assert_eq!(
            writer.push_device_bytes(&[0; 2], TRACK, 0, 1),
            TapPushOutcome::Dropped
        );
        assert_eq!(
            writer.push_device_bytes(&[0; 2], OTHER_TRACK, 0, 1),
            TapPushOutcome::Dropped
        );
        assert_eq!(writer.push_event(ended(99)), TapPushOutcome::Dropped);
        let expected: Vec<TapEvent> = (0..=PENDING_EVENT_CAPACITY as u64)
            .map(ended)
            .chain([overflowed(TRACK)])
            .collect();
        assert_eq!(receive_all(&mut writer, &mut reader), expected);
    }

    #[test]
    fn the_latest_stream_began_is_never_lost() {
        let (mut writer, mut reader) = tap_with_capacities(64, 1);
        assert_eq!(writer.push_event(ended(0)), TapPushOutcome::Pushed);
        for track_number in 1..=PENDING_EVENT_CAPACITY as u64 {
            writer.push_event(ended(track_number));
        }
        // Queue full: the stream start is held, never dropped.
        assert_eq!(writer.push_event(stream_began(3)), TapPushOutcome::Deferred);
        // A newer one replaces it, and the loss of the older one is marked.
        assert_eq!(writer.push_event(stream_began(4)), TapPushOutcome::Deferred);
        // Frozen: a later ordinary event is dropped under the same marker.
        assert_eq!(writer.push_event(ended(50)), TapPushOutcome::Dropped);
        let expected: Vec<TapEvent> = (0..=PENDING_EVENT_CAPACITY as u64)
            .map(ended)
            // The replaced stream start belongs to no track.
            .chain([stream_began(4), TapEvent::TapOverflowed { track_id: None }])
            .collect();
        assert_eq!(receive_all(&mut writer, &mut reader), expected);
        // Once everything went out, events flow again.
        assert_eq!(writer.push_event(ended(60)), TapPushOutcome::Pushed);
    }

    #[test]
    fn a_dropped_event_is_marked_with_its_own_track() {
        let (mut writer, mut reader) = tap_with_capacities(64, 1);
        assert_eq!(writer.push_event(ended(0)), TapPushOutcome::Pushed);
        for track_number in 1..=PENDING_EVENT_CAPACITY as u64 {
            writer.push_event(ended(track_number));
        }
        assert_eq!(writer.push_event(ended(77)), TapPushOutcome::Dropped);
        let received = receive_all(&mut writer, &mut reader);
        assert_eq!(received.last(), Some(&overflowed(TrackId(77))));
    }

    #[test]
    fn device_bytes_never_leave_orphan_bytes_when_the_event_ring_is_full() {
        let (mut writer, mut reader) = tap_with_capacities(64, 1);
        assert_eq!(writer.push_event(ended(0)), TapPushOutcome::Pushed);
        assert_eq!(
            writer.push_device_bytes(&[9; 4], TRACK, 0, 1),
            TapPushOutcome::Dropped
        );
        assert!(
            reader.pop_device_bytes(&mut [0u8; 1]).is_err(),
            "no byte was committed without its event"
        );
        assert_eq!(
            receive_all(&mut writer, &mut reader),
            vec![ended(0), overflowed(TRACK)]
        );
    }
}
