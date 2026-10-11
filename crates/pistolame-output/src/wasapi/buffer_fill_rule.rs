//! How many frames the render loop writes per buffer event, chosen once per stream (plan B5.6).
//! Source: the event-driven remarks of `IAudioClient::Initialize`:
//! <https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-initialize>

/// The per-mode fill rule. The render loop is the same for both modes apart from this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BufferFillRule {
    /// Exclusive event-driven: two buffers in ping-pong; "each time the client receives a
    /// buffer from the system ... the client must process the entire buffer".
    ExclusiveWholeBuffer,
    /// Shared event-driven (task T12): one buffer; "each time the thread awakens, it should
    /// call IAudioClient::GetCurrentPadding to determine how much data to write".
    #[allow(dead_code, reason = "selected by the shared-mode branch, task T12")]
    SharedFreeSpace,
}

impl BufferFillRule {
    /// Whether the loop must read `GetCurrentPadding` before writing.
    pub(crate) fn needs_current_padding(self) -> bool {
        self == BufferFillRule::SharedFreeSpace
    }

    /// Frames to write on this wake-up. `current_padding_frames` is ignored by the exclusive
    /// rule.
    pub(crate) fn frames_to_write(
        self,
        buffer_frame_count: u32,
        current_padding_frames: u32,
    ) -> u32 {
        match self {
            BufferFillRule::ExclusiveWholeBuffer => buffer_frame_count,
            BufferFillRule::SharedFreeSpace => {
                buffer_frame_count.saturating_sub(current_padding_frames)
            }
        }
    }

    /// Silent frames that must be written after a pause before the stream may stop, whatever
    /// `GetCurrentPadding` says. Microsoft documents `GetCurrentPadding` as "unnecessary" in
    /// event-driven exclusive mode and does not define its value there, so the exclusive rule
    /// requires two whole silent buffers: the second buffer event after the pause only fires
    /// once the device has moved past both ping-pong halves that held audio (B5.6 fallback).
    /// Shared mode has a single buffer whose padding is documented, so it needs no minimum.
    pub(crate) fn minimum_silent_frames_before_stop(self, buffer_frame_count: u32) -> u64 {
        match self {
            BufferFillRule::ExclusiveWholeBuffer => {
                EXCLUSIVE_PING_PONG_BUFFER_COUNT * u64::from(buffer_frame_count)
            }
            BufferFillRule::SharedFreeSpace => 0,
        }
    }

    /// Play-out criterion on close (B5.6): every audio frame queued before the pause has left
    /// the device buffer. Both conditions must hold: the per-mode minimum of silence has been
    /// written, and the queued frames are no more than the silence written since the pause.
    pub(crate) fn is_queued_audio_played_out(
        self,
        buffer_frame_count: u32,
        current_padding_frames: u32,
        silent_frames_written_since_play_out_began: u64,
    ) -> bool {
        silent_frames_written_since_play_out_began
            >= self.minimum_silent_frames_before_stop(buffer_frame_count)
            && u64::from(current_padding_frames) <= silent_frames_written_since_play_out_began
    }
}

/// Buffers in event-driven exclusive mode: "the client and the audio engine alternate"
/// between two buffers of the same size (`Initialize` remarks).
const EXCLUSIVE_PING_PONG_BUFFER_COUNT: u64 = 2;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusive_rule_always_fills_the_whole_buffer() {
        assert!(!BufferFillRule::ExclusiveWholeBuffer.needs_current_padding());
        assert_eq!(
            BufferFillRule::ExclusiveWholeBuffer.frames_to_write(960, 500),
            960
        );
    }

    #[test]
    fn shared_rule_fills_the_free_space() {
        assert!(BufferFillRule::SharedFreeSpace.needs_current_padding());
        assert_eq!(
            BufferFillRule::SharedFreeSpace.frames_to_write(960, 500),
            460
        );
        assert_eq!(BufferFillRule::SharedFreeSpace.frames_to_write(960, 960), 0);
        assert_eq!(
            BufferFillRule::SharedFreeSpace.frames_to_write(960, 1_000),
            0
        );
    }

    #[test]
    fn shared_play_out_completes_when_only_silence_is_queued() {
        let rule = BufferFillRule::SharedFreeSpace;
        assert_eq!(rule.minimum_silent_frames_before_stop(960), 0);
        assert!(!rule.is_queued_audio_played_out(960, 900, 460));
        assert!(rule.is_queued_audio_played_out(960, 900, 900));
        assert!(rule.is_queued_audio_played_out(960, 0, 0));
    }

    #[test]
    fn exclusive_play_out_needs_two_silent_buffers_whatever_the_padding() {
        let rule = BufferFillRule::ExclusiveWholeBuffer;
        assert_eq!(rule.minimum_silent_frames_before_stop(960), 1_920);
        // A driver reporting padding equal to one buffer: one silent buffer is not enough,
        // the second ping-pong half may still hold audio.
        assert!(!rule.is_queued_audio_played_out(960, 960, 960));
        // A driver reporting no padding at all.
        assert!(!rule.is_queued_audio_played_out(960, 0, 960));
        assert!(rule.is_queued_audio_played_out(960, 960, 1_920));
        assert!(rule.is_queued_audio_played_out(960, 0, 1_920));
        // Padding still above the silence written keeps waiting.
        assert!(!rule.is_queued_audio_played_out(960, 2_400, 1_920));
    }
}
