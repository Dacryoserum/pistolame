//! Gapless playback on the engine thread: preparing the track the UI announced as next,
//! queueing it in the current track chain when it has the same format, and following the chain
//! when it switched to it (`crate::track_chain`).
//!
//! The next track gets its own pipeline as soon as the current one is decoded to its end (the
//! rest of the current track is then in the ring, about `PLAYBACK_RING_DURATION` of audio), so
//! its file opens and its prefill fills while that tail plays. Once prefilled:
//! - same format as the stream's first track: its feeder is queued in the chain, which goes on
//!   with it in the same stream, without a gap;
//! - another format: it waits; the current track ends as usual (drain, `Stopped`), and the
//!   UI's `OpenFileAndPlay` of the same path starts from this prepared pipeline, so only the
//!   device reopen remains between the two tracks.

use std::path::{Path, PathBuf};

use super::EngineThread;
use crate::engine::EngineEvent;
use crate::engine::decode_thread::DecodeEvent;
use crate::engine::failure_mapping::failure_for_segment;
use crate::engine::pipeline::Pipeline;
use crate::feeder::DeviceFeeder;
use crate::signal_path::{ProcessingState, assess_signal_path};
use crate::track_chain::{QueuedTrack, TrackChainControl};
use crate::verification::VerifierCommand;

/// The track that follows the current one, prepared ahead.
pub(super) struct NextTrack {
    /// The file, as the UI announced it.
    pub(super) path: PathBuf,
    pub(super) pipeline: Pipeline,
    /// The pipeline's feeder, until it is queued in the chain. Stays here when the format
    /// differs from the chain's, for an `OpenFileAndPlay` of the same path.
    pub(super) feeder: Option<DeviceFeeder>,
}

/// Whether following a switch the chain made tells the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SwitchAnnouncement {
    /// `AdvancedToNextTrack`, `TrackLoaded` and the new signal path are emitted.
    ToTheUi,
    /// Nothing is emitted: a command (open, stop) replaces this playback, and the UI already
    /// moved its queue for it.
    Silent,
}

impl EngineThread {
    /// `EngineCommand::SetNextTrack`: a different track replaces the prepared one, unless the
    /// chain already switched to it (it then plays, and the UI hears of it at the next poll).
    pub(super) fn set_next_track(&mut self, path: Option<PathBuf>) {
        self.collect_track_chain_changes(SwitchAnnouncement::ToTheUi, false);
        if self.next_track_path != path {
            self.discard_next_track();
            self.next_track_path = path;
        }
        self.prepare_next_track();
    }

    /// Builds the next track's pipeline once the current track is decoded to its end. A next
    /// track that cannot be opened is left alone: the UI's `OpenFileAndPlay` reports why.
    pub(super) fn prepare_next_track(&mut self) {
        let is_current_decoded_to_end = self
            .pipeline
            .as_ref()
            .is_some_and(|pipeline| pipeline.is_decoded_to_end);
        if self.next_track.is_some() || !is_current_decoded_to_end {
            return;
        }
        let Some(path) = self.next_track_path.clone() else {
            return;
        };
        let Ok(decoder) = self.decoders.open(&path) else {
            return;
        };
        let track_id = self.allocate_track_id();
        if let Ok((pipeline, feeder)) = self.build_pipeline(track_id, decoder) {
            self.next_track = Some(NextTrack {
                path,
                pipeline,
                feeder: Some(feeder),
            });
        }
    }

    /// A decode event of the next track's pipeline.
    pub(super) fn handle_next_track_decode_event(&mut self, event: DecodeEvent) {
        let Some(next_track) = self.next_track.as_mut() else {
            return;
        };
        if next_track.pipeline.record_decode_event(event) {
            self.queue_next_track_in_chain();
        }
    }

    /// Queues the prepared next track in the chain once prefilled, when it has the chain's
    /// format. Retried at each poll while a track taken back still fills the chain's queue.
    pub(super) fn queue_next_track_in_chain(&mut self) {
        let (Some(next_track), Some(chain)) = (self.next_track.as_mut(), self.track_chain.as_mut())
        else {
            return;
        };
        if !next_track.pipeline.is_prefill_reached
            || next_track.pipeline.source.format != chain.chained_format()
            || !chain.can_queue_next()
        {
            return;
        }
        let Some(feeder) = next_track.feeder.take() else {
            return;
        };
        // Cannot fail after `can_queue_next`; were it to, the track would only lose its gapless
        // start (the UI opens it at the end of the current one).
        chain.queue_next(QueuedTrack {
            track_id: next_track.pipeline.track_id,
            feeder,
        });
    }

    /// Tears the prepared next track down, taking it back from the chain if it was queued
    /// there (its feeder is then retired unplayed and dropped when collected). Keeps it when
    /// the chain already claimed it: it is playing, and the next collect makes it current.
    pub(super) fn discard_next_track(&mut self) {
        let Some(next_track) = self.next_track.as_ref() else {
            return;
        };
        let is_queued_in_chain = next_track.feeder.is_none();
        let is_taken_back = !is_queued_in_chain
            || self
                .track_chain
                .as_ref()
                .is_none_or(|chain| chain.cancel_next(next_track.pipeline.track_id));
        if !is_taken_back {
            return;
        }
        let Some(mut next_track) = self.next_track.take() else {
            return;
        };
        next_track.pipeline.render_source = next_track
            .feeder
            .take()
            .map(|feeder| Box::new(feeder) as Box<dyn crate::output_contract::RenderSource>);
        let outcome = next_track.pipeline.tear_down(false);
        self.forward_pending_verification_updates();
        self.report_panicked_threads(outcome.panicked_threads);
    }

    /// The prepared next track, if it is `path` and its feeder is still here (not queued in a
    /// chain), to start it at once.
    pub(super) fn take_prepared_next_track(&mut self, path: &Path) -> Option<NextTrack> {
        let is_prepared_here = self
            .next_track
            .as_ref()
            .is_some_and(|next_track| next_track.path == path && next_track.feeder.is_some());
        if is_prepared_here {
            self.next_track.take()
        } else {
            None
        }
    }

    /// Starts a prepared next track as a new current track (its own chain and stream).
    pub(super) fn start_prepared_track(&mut self, next_track: NextTrack) {
        let NextTrack {
            path,
            pipeline,
            feeder,
        } = next_track;
        let Some(feeder) = feeder else {
            return;
        };
        self.emit(EngineEvent::TrackLoaded {
            track_id: pipeline.track_id,
            path,
            source: pipeline.source.clone(),
        });
        self.install_current_pipeline(pipeline, feeder);
    }

    /// Takes back the feeders the chain is done with. A played one means the chain went on
    /// with the next track: it becomes the current track. Exact once the stream is closed;
    /// while it plays, a later poll sees a switch made after this call.
    /// `was_queued_audio_discarded`: the stream just closed without playing out the device
    /// queue, which may still have held the finished track's last frames.
    pub(super) fn collect_track_chain_changes(
        &mut self,
        announcement: SwitchAnnouncement,
        was_queued_audio_discarded: bool,
    ) {
        while let Some(retired) = self
            .track_chain
            .as_mut()
            .and_then(TrackChainControl::take_retired)
        {
            // A feeder taken back belongs to a pipeline already torn down: it is dropped. A
            // played one is always the current track's (the chain finishes tracks in order).
            let is_current_track_finished = self
                .pipeline
                .as_ref()
                .is_some_and(|pipeline| pipeline.track_id == retired.track_id);
            if retired.was_played && is_current_track_finished {
                self.follow_chain_to_next_track(
                    retired.feeder,
                    announcement,
                    was_queued_audio_discarded,
                );
            }
        }
    }

    /// The chain finished the current track and plays the next one: the next pipeline becomes
    /// current (same stream, same counters baselines), the finished one is torn down as a
    /// track that ended, and the UI is told unless `announcement` is `Silent`. When its last
    /// frames may have been dropped from the device queue, its verifier is told first, so it
    /// never keeps a `Verified` over audio that was not played.
    fn follow_chain_to_next_track(
        &mut self,
        finished_feeder: DeviceFeeder,
        announcement: SwitchAnnouncement,
        was_queued_audio_discarded: bool,
    ) {
        let Some(next_track) = self.next_track.take() else {
            return;
        };
        let Some(mut finished) = self.pipeline.replace(next_track.pipeline) else {
            return;
        };
        let Some(current) = self.pipeline.as_mut() else {
            return;
        };
        current.render_source = finished.render_source.take();
        current.has_opened_stream = finished.has_opened_stream;
        current.stream_ends_baseline = finished.stream_ends_baseline;
        current.format_boundaries_baseline = finished.format_boundaries_baseline;
        let track_id = current.track_id;
        let source = current.source.clone();
        let stream_ends_baseline = current.stream_ends_baseline;
        finished.render_source = Some(Box::new(finished_feeder));
        if was_queued_audio_discarded {
            finished.service_render_source();
            finished.send_to_verifier(VerifierCommand::QueuedAudioLost {
                track_id: finished.track_id,
            });
        }
        let outcome = finished.tear_down(true);
        self.forward_pending_verification_updates();
        self.report_panicked_threads(outcome.panicked_threads);
        self.followed_switch_count += 1;
        self.next_track_path = None;
        self.last_opened_path = Some(next_track.path.clone());
        if announcement == SwitchAnnouncement::Silent {
            return;
        }
        if let Some(failure) = outcome.segment_failure {
            let (kind, detail) = failure_for_segment(&failure);
            self.emit_failure(kind, detail);
        }
        self.emit(EngineEvent::AdvancedToNextTrack {
            path: next_track.path.clone(),
            stream_ends_baseline,
        });
        self.emit(EngineEvent::TrackLoaded {
            track_id,
            path: next_track.path,
            source: source.clone(),
        });
        if let Some(stream) = self.stream.as_ref() {
            let assessment = assess_signal_path(
                &source.format,
                &source.codec,
                stream.negotiated(),
                &ProcessingState::default(),
            );
            self.emit(EngineEvent::SignalPathChanged {
                assessment: Some(assessment),
            });
        }
    }
}
