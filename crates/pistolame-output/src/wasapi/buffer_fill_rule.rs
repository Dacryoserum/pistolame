//! How many frames the render loop writes per buffer event, chosen once per stream (plan B5.6).
//! Source: the event-driven remarks of `IAudioClient::Initialize`:
//! <https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-initialize>

/// The per-mode fill rule. The render loop is the same for both modes apart from this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BufferFillRule {
    /// Exclusive event-driven: two buffers in ping-pong; "each time the client receives a
    /// buffer from the system ... the client must process the entire buffer".
    ExclusiveWholeBuffer,
    /// Shared event-driven: one buffer; "each time the thread awakens, it should call
    /// IAudioClient::GetCurrentPadding to determine how much data to write".
    SharedFreeSpace {
        /// The audio engine's default period (`GetDevicePeriod`), in frames of the stream's
        /// own rate, rounded up: the extra silence the play-out writes once the engine has read
        /// the last audio frame, so its converter and effects can flush their delay.
        engine_period_frames: u32,
    },
}

impl BufferFillRule {
    /// Whether the loop must read `GetCurrentPadding` before writing.
    pub(crate) fn needs_current_padding(self) -> bool {
        matches!(self, BufferFillRule::SharedFreeSpace { .. })
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
            BufferFillRule::SharedFreeSpace { .. } => {
                buffer_frame_count.saturating_sub(current_padding_frames)
            }
        }
    }

    /// Silent frames that must be written after a pause before the stream may stop, whatever
    /// `GetCurrentPadding` says.
    ///
    /// - Exclusive: Microsoft documents `GetCurrentPadding` as "unnecessary" in event-driven
    ///   exclusive mode and does not define its value there, so the rule requires two whole
    ///   silent buffers: the second buffer event after the pause only fires once the device has
    ///   moved past both ping-pong halves that held audio (B5.6 fallback).
    /// - Shared: the loop always fills the whole free space, so "queued frames no more than the
    ///   silence written" holds as soon as the audio engine has *read* the last audio frame. The
    ///   engine's sample rate converter and effects still hold part of it at that point, and
    ///   `Stop` plus `Reset` would cut that tail. A whole buffer of silence plus one more engine
    ///   period lets the engine process the last audio frame through its own pipeline first.
    pub(crate) fn minimum_silent_frames_before_stop(self, buffer_frame_count: u32) -> u64 {
        match self {
            BufferFillRule::ExclusiveWholeBuffer => {
                EXCLUSIVE_PING_PONG_BUFFER_COUNT * u64::from(buffer_frame_count)
            }
            BufferFillRule::SharedFreeSpace {
                engine_period_frames,
            } => u64::from(buffer_frame_count) + u64::from(engine_period_frames),
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
/// WASAPI's time unit: one REFERENCE_TIME tick is 100 ns.
const HUNDRED_NANOSECONDS_PER_SECOND: u64 = 10_000_000;

/// The number of frames at `sample_rate_hz` that cover `period_in_hundred_nanoseconds`,
/// rounded up so the play-out never waits less than one period. Saturates; a negative period
/// counts as zero.
pub(crate) fn frames_covering_period(
    period_in_hundred_nanoseconds: i64,
    sample_rate_hz: u32,
) -> u32 {
    let period = u64::try_from(period_in_hundred_nanoseconds).unwrap_or(0);
    let frames = period
        .saturating_mul(u64::from(sample_rate_hz))
        .div_ceil(HUNDRED_NANOSECONDS_PER_SECOND);
    u32::try_from(frames).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 10 ms at 96 kHz, the K11's default engine period at a common rate.
    const SHARED: BufferFillRule = BufferFillRule::SharedFreeSpace {
        engine_period_frames: 960,
    };

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
        assert!(SHARED.needs_current_padding());
        assert_eq!(SHARED.frames_to_write(960, 500), 460);
        assert_eq!(SHARED.frames_to_write(960, 960), 0);
        assert_eq!(SHARED.frames_to_write(960, 1_000), 0);
    }

    #[test]
    fn shared_play_out_waits_one_engine_period_after_the_last_audio_frame_is_read() {
        let buffer_frame_count = 1_056;
        assert_eq!(
            SHARED.minimum_silent_frames_before_stop(buffer_frame_count),
            1_056 + 960
        );
        // Audio still queued.
        assert!(!SHARED.is_queued_audio_played_out(buffer_frame_count, 900, 460));
        // The engine has read the last audio frame (only silence is queued), but its converter
        // may still hold it: not yet.
        assert!(!SHARED.is_queued_audio_played_out(buffer_frame_count, 1_056, 1_056));
        assert!(!SHARED.is_queued_audio_played_out(buffer_frame_count, 0, 1_056));
        // One engine period of silence later: done.
        assert!(SHARED.is_queued_audio_played_out(buffer_frame_count, 1_056, 2_016));
        assert!(SHARED.is_queued_audio_played_out(buffer_frame_count, 0, 2_016));
        // Padding above the silence written keeps waiting.
        assert!(!SHARED.is_queued_audio_played_out(buffer_frame_count, 2_500, 2_016));
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

    #[test]
    fn period_frames_round_up_at_the_stream_rate() {
        // 10 ms.
        assert_eq!(frames_covering_period(100_000, 44_100), 441);
        assert_eq!(frames_covering_period(100_000, 96_000), 960);
        // 3 ms at 44.1 kHz = 132.3 frames, rounded up.
        assert_eq!(frames_covering_period(30_000, 44_100), 133);
        assert_eq!(frames_covering_period(-1, 44_100), 0);
        assert_eq!(frames_covering_period(i64::MAX, 384_000), u32::MAX);
    }
}
