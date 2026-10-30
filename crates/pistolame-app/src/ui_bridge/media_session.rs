//! System media controls: what is shown and the media keys.

use super::*;
use crate::transport::PendingSeek;

impl UiBridge {
    /// Shows the selected track in the system media panel.
    pub(super) fn publish_media_track(&self) {
        let Some((album_index, track_index)) = self.selected_track.get() else {
            return;
        };
        let albums = self.albums.borrow();
        if let Some(album) = albums.get(album_index)
            && let Some(track) = album.tracks.get(track_index)
        {
            self.media_controls.borrow_mut().publish_track(
                &track.title,
                &track.artist,
                &album.title,
                Duration::from_secs(track.duration_seconds),
            );
        }
    }

    /// Position in the current track: the restored one, or what the engine has played.
    pub(super) fn position_in_track(&self) -> Duration {
        if let Some(seconds) = self.resume_position_seconds.get() {
            return Duration::from_secs(seconds);
        }
        let rate = u64::from(self.sample_rate_hz.get().max(1));
        let frames = self.shown_frames_in_track(self.engine.counters().frames_rendered_in_track);
        Duration::from_secs_f64(frames as f64 / rate as f64)
    }

    /// Jumps in the current track (a restored track not opened yet starts there on Play).
    pub(super) fn seek_to(&self, position: Duration) {
        if self.resume_position_seconds.get().is_some() {
            self.resume_position_seconds.set(Some(position.as_secs()));
        } else {
            self.engine.send(EngineCommand::SeekTo {
                position_in_track: position,
            });
            let rate = u64::from(self.sample_rate_hz.get().max(1));
            let target_frame_index = (position.as_secs_f64() * rate as f64) as u64;
            self.pending_seek
                .set(Some(PendingSeek::new(target_frame_index)));
            if let Some(window) = self.window.upgrade() {
                self.show_position(&window.global::<AppState>(), target_frame_index);
            }
        }
        self.publish_media_status_at(position);
    }

    /// Tells the system media panel whether the app plays, and where.
    pub(super) fn publish_media_status(&self) {
        self.publish_media_status_at(self.position_in_track());
    }

    pub(super) fn publish_media_status_at(&self, position: Duration) {
        use crate::media_controls::TransportStatus;
        let status = match self.state.get() {
            PlaybackState::Playing | PlaybackState::Opening => TransportStatus::Playing,
            PlaybackState::Paused => TransportStatus::Paused,
            PlaybackState::Stopped if self.selected_track.get().is_some() => {
                TransportStatus::Paused
            }
            PlaybackState::Stopped => TransportStatus::Stopped,
        };
        self.media_controls
            .borrow_mut()
            .publish_status(status, position);
    }

    /// Registers the media keys once the window exists, then applies what they ask.
    pub(super) fn handle_media_commands(&self) {
        use crate::media_controls::MediaCommand;
        if let Some(window) = self.window.upgrade() {
            let is_new = self.media_controls.borrow_mut().install_once(&window);
            if is_new {
                self.publish_media_track();
                self.publish_media_status();
            }
        }
        let commands = self.media_controls.borrow().take_commands();
        for command in commands {
            let is_active = matches!(
                self.state.get(),
                PlaybackState::Playing | PlaybackState::Opening
            );
            match command {
                MediaCommand::Play if !is_active => self.toggle_play(),
                MediaCommand::Pause if is_active => self.toggle_play(),
                MediaCommand::Play | MediaCommand::Pause => {}
                MediaCommand::Toggle => self.toggle_play(),
                MediaCommand::Next => self.adjacent_track(true),
                MediaCommand::Previous => self.adjacent_track(false),
                MediaCommand::Stop => self.engine.send(EngineCommand::Stop),
                MediaCommand::SeekTo(position) => self.seek_to(position),
                MediaCommand::SeekBy { is_forward, amount } => {
                    let current = self.position_in_track();
                    self.seek_to(if is_forward {
                        current + amount
                    } else {
                        current.saturating_sub(amount)
                    });
                }
            }
        }
    }
}
