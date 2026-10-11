//! One playback pipeline per opened file (plan B2, B5.7): its playback ring, verification tap,
//! feeder, decode thread and verifier thread, created together when the file opens and torn
//! down together, in the B5.7 order, when it stops, ends or another file opens.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};

use super::decode_thread::{
    DecodeCommand, DecodeEvent, DecodeThreadSetup, SegmentFailure, spawn_decode_thread,
};
use super::handle::EngineTuning;
use crate::decoder::{AudioDecoder, SourceStreamInfo};
use crate::engine::PlaybackCounters;
use crate::feeder::DeviceFeeder;
use crate::output_contract::RenderSource;
use crate::playback_ring::{TrackId, create_playback_ring};
use crate::timing::TEARDOWN_FLUSH_TIMEOUT;
use crate::verification::{
    VerificationUpdate, VerifiedTrack, VerifierCommand, create_verification_tap, spawn_verifier,
};

/// Widest container `validate_pcm_layout` accepts: the tap is sized for it, since the render
/// format is only known when a stream begins (`create_verification_tap` docs).
const WIDEST_CONTAINER_BYTES: usize = 4;
/// Pause between two `service_while_closed` calls while the verifier drains the tap (B6).
const SERVICE_RETRY_SLEEP: Duration = Duration::from_millis(1);

/// The engine's handles on one opened file's threads and shared state.
pub(crate) struct Pipeline {
    pub(crate) track_id: TrackId,
    pub(crate) source: SourceStreamInfo,
    flush_generation: Arc<AtomicU64>,
    /// The feeder while no stream holds it (before the first open, while paused, between a
    /// close and a reopen). `None` while a stream is open.
    pub(crate) render_source: Option<Box<dyn RenderSource>>,
    decode_commands: Sender<DecodeCommand>,
    pub(crate) decode_events: Receiver<DecodeEvent>,
    decode_thread: Option<JoinHandle<()>>,
    verifier_commands: Sender<VerifierCommand>,
    verifier_thread: Option<JoinHandle<()>>,
    /// `PrefillReached` arrived for the current generation.
    pub(crate) is_prefill_reached: bool,
    /// A stream was opened for this pipeline at least once, so a later open failure is a
    /// failed resume (B8), not a failed start.
    pub(crate) has_opened_stream: bool,
    /// Why the current generation ended early, reported at the track end.
    pub(crate) kept_segment_failure: Option<SegmentFailure>,
    /// `stream_ends_reached` and `format_boundaries_reached` when the current stream opened;
    /// growth above them during `Playing` is this pipeline's.
    pub(crate) stream_ends_baseline: u64,
    pub(crate) format_boundaries_baseline: u64,
}

/// What a teardown found that the engine must still report.
#[derive(Debug, Default)]
pub(crate) struct TearDownOutcome {
    /// Why the current generation ended early, from an event read before the teardown or
    /// still waiting in the decode events channel (a short track can end before the engine
    /// reads it).
    pub(crate) segment_failure: Option<SegmentFailure>,
    /// Names of the pipeline threads that panicked instead of returning.
    pub(crate) panicked_threads: Vec<&'static str>,
}

/// What a pipeline is built from.
pub(crate) struct PipelineSetup<'setup> {
    pub(crate) track_id: TrackId,
    pub(crate) decoder: Box<dyn AudioDecoder>,
    pub(crate) tuning: &'setup EngineTuning,
    pub(crate) counters: &'setup Arc<PlaybackCounters>,
    pub(crate) verification_updates: &'setup Sender<VerificationUpdate>,
}

impl Pipeline {
    /// Creates the ring, the tap, the feeder, then spawns the verifier and the decode thread.
    pub(crate) fn build(setup: PipelineSetup<'_>) -> std::io::Result<Pipeline> {
        let source = setup.decoder.stream_info().clone();
        let format = source.format;
        let (ring_writer, ring_reader) =
            create_playback_ring(&format, setup.tuning.playback_ring_duration);
        let (tap_writer, tap_reader) = create_verification_tap(
            format.sample_rate_hz,
            WIDEST_CONTAINER_BYTES * usize::from(format.channel_count),
            setup.tuning.verification_tap_duration,
        );
        let flush_generation = Arc::new(AtomicU64::new(0));
        let is_playback_queue_finished = Arc::new(AtomicBool::new(false));
        let feeder = DeviceFeeder::new(
            ring_reader,
            tap_writer,
            Arc::clone(&flush_generation),
            Arc::clone(&is_playback_queue_finished),
            Arc::clone(setup.counters),
        );
        let (verifier_commands, verifier_command_receiver) = unbounded();
        let verifier_thread = spawn_verifier(
            VerifiedTrack {
                track_id: setup.track_id,
                source_format: format,
                expected_md5: source.embedded_md5,
                expected_total_frames: source.total_frames,
            },
            tap_reader,
            verifier_command_receiver,
            setup.verification_updates.clone(),
        )?;
        let (decode_commands, decode_command_receiver) = unbounded();
        let (decode_event_sender, decode_events) = unbounded();
        let decode_thread = spawn_decode_thread(DecodeThreadSetup {
            decoder: setup.decoder,
            track_id: setup.track_id,
            ring_writer,
            is_playback_queue_finished,
            start_prefill_duration: setup.tuning.start_prefill_duration,
            commands: decode_command_receiver,
            events: decode_event_sender,
            verifier_commands: verifier_commands.clone(),
        });
        let decode_thread = match decode_thread {
            Ok(thread) => thread,
            Err(error) => {
                // The verifier exits on `Shutdown`; nothing else was started.
                let _ = verifier_commands.send(VerifierCommand::Shutdown);
                let _ = verifier_thread.join();
                return Err(error);
            }
        };
        Ok(Pipeline {
            track_id: setup.track_id,
            source,
            flush_generation,
            render_source: Some(Box::new(feeder)),
            decode_commands,
            decode_events,
            decode_thread: Some(decode_thread),
            verifier_commands,
            verifier_thread: Some(verifier_thread),
            is_prefill_reached: false,
            has_opened_stream: false,
            kept_segment_failure: None,
            stream_ends_baseline: 0,
            format_boundaries_baseline: 0,
        })
    }

    pub(crate) fn current_generation(&self) -> u64 {
        self.flush_generation.load(Ordering::Acquire)
    }

    /// Seek: a new generation (Release, so the feeder drops every older block), stale blocks
    /// dropped from the held feeder, then the decode thread told to decode from `target`.
    pub(crate) fn seek(&mut self, target_frame_index: u64) {
        let flush_generation = self.flush_generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.is_prefill_reached = false;
        self.kept_segment_failure = None;
        self.service_render_source();
        let _ = self.decode_commands.send(DecodeCommand::Seek {
            target_frame_index,
            flush_generation,
        });
    }

    /// Tells the verifier. A send error means the verifier already exited: nothing to tell.
    pub(crate) fn send_to_verifier(&self, command: VerifierCommand) {
        let _ = self.verifier_commands.send(command);
    }

    /// One non-blocking `service_while_closed` call on the held feeder, if any (engine poll).
    pub(crate) fn service_render_source_once(&mut self) {
        if let Some(render_source) = self.render_source.as_mut() {
            render_source.service_while_closed();
        }
    }

    /// `service_while_closed` until it returns `true`, 1 ms apart, at most
    /// `TEARDOWN_FLUSH_TIMEOUT` while the verifier drains the tap. Past it the waiting events
    /// are given up: the verifier then concludes the open segment as stopped (B7.2).
    pub(crate) fn service_render_source(&mut self) {
        let Some(render_source) = self.render_source.as_mut() else {
            return;
        };
        let started_at = Instant::now();
        while !render_source.service_while_closed() {
            if started_at.elapsed() >= TEARDOWN_FLUSH_TIMEOUT {
                return;
            }
            std::thread::sleep(SERVICE_RETRY_SLEEP);
        }
    }

    /// B5.7 order: stop and join the decode thread (its `Abandoned` report is then in the
    /// verifier's channel), service the feeder, `TrackStopped` unless the track ended,
    /// `Shutdown`, join the verifier (its final status is then in the updates channel). The
    /// stream must already be closed.
    pub(crate) fn tear_down(mut self, has_track_ended: bool) -> TearDownOutcome {
        let mut outcome = TearDownOutcome::default();
        let _ = self.decode_commands.send(DecodeCommand::Stop);
        if let Some(thread) = self.decode_thread.take()
            && thread.join().is_err()
        {
            outcome.panicked_threads.push("decode");
        }
        outcome.segment_failure = self.take_segment_failure();
        self.service_render_source();
        if !has_track_ended {
            self.send_to_verifier(VerifierCommand::TrackStopped {
                track_id: self.track_id,
            });
        }
        self.send_to_verifier(VerifierCommand::Shutdown);
        if let Some(thread) = self.verifier_thread.take()
            && thread.join().is_err()
        {
            outcome.panicked_threads.push("verifier");
        }
        outcome
    }

    /// The kept failure, or one of the current generation still unread in the decode events
    /// (the decode thread is joined, so nothing more can arrive).
    fn take_segment_failure(&mut self) -> Option<SegmentFailure> {
        let current_generation = self.current_generation();
        let unread_failure = self.decode_events.try_iter().find_map(|event| match event {
            DecodeEvent::SegmentEnded {
                flush_generation,
                error,
            } if flush_generation == current_generation => error,
            _ => None,
        });
        self.kept_segment_failure.take().or(unread_failure)
    }
}

impl Drop for Pipeline {
    /// A pipeline dropped without `tear_down` (engine thread unwinding) still stops its threads:
    /// the closed channels make both of them return.
    fn drop(&mut self) {
        let _ = self.decode_commands.send(DecodeCommand::Stop);
        let _ = self.verifier_commands.send(VerifierCommand::Shutdown);
    }
}
