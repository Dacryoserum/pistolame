//! The audio engine: the messages between the UI and the engine thread, the lock-free counters
//! the real-time thread writes, the `EngineHandle` the UI owns, and the threads behind it (plan
//! B2, B3.7, B5.7).
//!
//! Threads: the engine thread (`engine_thread.rs`) owns the `AudioOutput`, the open stream and
//! the state machine below; each opened file gets a pipeline (`pipeline.rs`) with its own
//! decode thread (`decode_thread.rs`) and verifier thread; the backend owns the real-time
//! render thread, which runs the pipeline's `DeviceFeeder`.
//!
//! Playback states (`PlaybackState`). `Paused` holds no stream: the device is released, the
//! ring, the decoder and the feeder's position are kept.
//!
//! ```text
//!                      OpenFileAndPlay (from any state: the old stream closes with Discard
//!                      and the old pipeline is torn down) / Play in Stopped (last file again)
//!                                          |
//!                                          v
//!        +--------- Pause --------- [ Opening ] <--------- SeekTo (close Discard,
//!        |                           |      ^               new generation)
//!        v                           |      |                    |
//!   [ Paused ] -- Play, prefill  ----+      |                    |
//!     |  ^        not reached yet           |                    |
//!     |  |                                  |                    |
//!     |  |              PrefillReached of the current generation:
//!     |  |              open the stream, play()
//!     |  |                                  |                    |
//!     |  |                                  v                    |
//!     |  +---- Pause (close PlayOut) --- [ Playing ] ------------+
//!     |                                   |   ^
//!     +-- Play, prefill reached: ---------+   +-- SetOutputModePreference / SelectOutputDevice:
//!         reopen, play()                            close PlayOut, reopen, play()
//!                                                   (also when the system default output
//!                                                   moves under "Sortie du système", as long
//!                                                   as the device in use is still present)
//!
//!   SeekTo in Paused or Opening: new generation, same state.
//!   To [ Stopped ], tearing the pipeline down:
//!     Stop or Shutdown (any state);
//!     poll in Playing: status() Failed (close Discard, refresh the device list);
//!     poll in Playing: format_boundaries_reached grew (close Discard, Internal failure);
//!     poll in Playing: stream_ends_reached grew (close PlayOut; the track ended, so no
//!       TrackStopped; a kept decode error is reported now);
//!     a first open that fails, or a resume that finds no device (a resume that fails for
//!       another reason stays Paused).
//! ```

pub mod counters;
mod decode_thread;
mod engine_thread;
mod failure_mapping;
mod handle;
pub mod messages;
mod pipeline;

pub use counters::*;
pub use handle::*;
pub use messages::*;
