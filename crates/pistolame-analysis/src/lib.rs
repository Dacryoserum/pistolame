//! Authenticity analysis of audio files: lossy-to-lossless transcodes, bit-depth padding,
//! limited bandwidth, dynamics (DR14, LUFS, true peak) and the spectrogram.
//!
//! Ported from Nyquist (https://github.com/Dacryoserum/nyquist, MIT License, Copyright (c)
//! 2026 Matthieu), the same author's analyzer; see `NYQUIST-LICENSE`. The verdict logic is
//! Nyquist's: four explainable states, never a flat yes/no, and no verdict without positive
//! evidence. Pistolame only reads the results; the analysis never touches playback.
//!
//! The modules are kept as close to Nyquist's as possible so fixes can flow both ways; that is
//! why some self-explanatory fields carry no doc comment (`missing_docs` is allowed here only).
#![allow(missing_docs)]

pub mod analysis;
pub mod bit_depth;
pub mod decode;
pub mod dynamic_range;
pub mod mdct_grid;
pub mod metadata;
pub mod sample_rate;
pub mod signal_analysis;
pub mod spectral;
pub mod stereo;
pub mod tags;
pub mod transcode_detect;
