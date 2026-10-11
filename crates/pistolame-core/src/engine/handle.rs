//! `EngineHandle`: what the UI owns. Starting it spawns the engine thread; afterwards the UI
//! only sends `EngineCommand`s, reads `EngineEvent`s and the lock-free counters.

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, unbounded};

use super::engine_thread::{EngineThread, EngineThreadSetup};
use crate::decoder::DecoderRegistry;
use crate::engine::{
    EngineCommand, EngineEvent, EngineStartOptions, PlaybackCounters, PlaybackCountersSnapshot,
};
use crate::output_contract::AudioOutput;
use crate::timing::{PLAYBACK_RING_DURATION, START_PREFILL_DURATION, VERIFICATION_TAP_DURATION};

/// Name of the engine thread, as seen in debuggers and crash reports.
const ENGINE_THREAD_NAME: &str = "pistolame-engine";

/// Sizes the engine uses; `Default` takes the `timing.rs` constants.
#[derive(Clone, Copy, Debug)]
pub struct EngineTuning {
    /// Capacity of each pipeline's playback ring.
    pub playback_ring_duration: Duration,
    /// Audio buffered before a stream opens (or the whole track if shorter).
    pub start_prefill_duration: Duration,
    /// Capacity of each pipeline's verification tap, in device bytes.
    pub verification_tap_duration: Duration,
}

impl Default for EngineTuning {
    fn default() -> Self {
        Self {
            playback_ring_duration: PLAYBACK_RING_DURATION,
            start_prefill_duration: START_PREFILL_DURATION,
            verification_tap_duration: VERIFICATION_TAP_DURATION,
        }
    }
}

/// Handle the UI owns. Dropping it shuts the engine down, like `shutdown`.
pub struct EngineHandle {
    commands: Sender<EngineCommand>,
    events: Receiver<EngineEvent>,
    counters: Arc<PlaybackCounters>,
    thread: Option<JoinHandle<()>>,
}

impl EngineHandle {
    /// Spawns the engine thread on `output`, with the `timing.rs` sizes. It lists the devices
    /// and selects one at once (B8).
    ///
    /// # Errors
    /// The thread could not be spawned.
    pub fn start(
        output: Box<dyn AudioOutput>,
        decoders: DecoderRegistry,
        options: EngineStartOptions,
    ) -> std::io::Result<Self> {
        Self::spawn(output, decoders, options, EngineTuning::default())
    }

    /// Feature `test-support` only: same, with test sizes (B10.3).
    ///
    /// # Errors
    /// The thread could not be spawned.
    #[cfg(any(test, feature = "test-support"))]
    pub fn start_with_tuning(
        output: Box<dyn AudioOutput>,
        decoders: DecoderRegistry,
        options: EngineStartOptions,
        tuning: EngineTuning,
    ) -> std::io::Result<Self> {
        Self::spawn(output, decoders, options, tuning)
    }

    fn spawn(
        output: Box<dyn AudioOutput>,
        decoders: DecoderRegistry,
        options: EngineStartOptions,
        tuning: EngineTuning,
    ) -> std::io::Result<Self> {
        let (command_sender, command_receiver) = unbounded();
        let (event_sender, event_receiver) = unbounded();
        let counters = Arc::new(PlaybackCounters::new());
        let setup = EngineThreadSetup {
            output,
            decoders,
            options,
            tuning,
            counters: Arc::clone(&counters),
            commands: command_receiver,
            events: event_sender,
        };
        let thread = std::thread::Builder::new()
            .name(ENGINE_THREAD_NAME.to_owned())
            .spawn(move || EngineThread::new(setup).run())?;
        Ok(Self {
            commands: command_sender,
            events: event_receiver,
            counters,
            thread: Some(thread),
        })
    }

    /// Queues a command. Never blocks (unbounded channel); a command sent after the engine
    /// stopped is dropped.
    pub fn send(&self, command: EngineCommand) {
        let _ = self.commands.send(command);
    }

    /// Every event received so far, without waiting.
    pub fn try_receive_events(&self) -> impl Iterator<Item = EngineEvent> + '_ {
        self.events.try_iter()
    }

    /// The next event, waiting at most `timeout`. For headless tools and tests; the UI polls
    /// `try_receive_events` on its timer instead.
    pub fn receive_event_timeout(&self, timeout: Duration) -> Option<EngineEvent> {
        self.events.recv_timeout(timeout).ok()
    }

    /// Current values of the lock-free counters (cumulative over the engine run).
    pub fn counters(&self) -> PlaybackCountersSnapshot {
        self.counters.snapshot()
    }

    /// Sends `Shutdown` and waits for the engine thread: the stream is closed and the pipeline
    /// torn down when it returns.
    pub fn shutdown(mut self) {
        self.stop_engine_thread();
    }

    fn stop_engine_thread(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = self.commands.send(EngineCommand::Shutdown);
            let _ = thread.join();
        }
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        self.stop_engine_thread();
    }
}
