# Idea backlog

Ideas for later. Not scheduled; do not implement without a plan.

## Device-aware output indicator (user, 2026-10-11)

A discreet indicator for the active audio output that reflects the *type* of device:
an external DAC shows a small 3D model of a DAC, which animates on hover; other device
types (headphones, speakers, built-in, Bluetooth, HDMI…) get their own model.
Must stay subtle and light (no heavy 3D engine on the UI path); render cost and asset
licensing to study in a plan first.

## Port the Nyquist analysis engine (user, 2026-10-11)

Nyquist (github.com/Dacryoserum/nyquist, MIT, same author) already has a tested analysis engine
in `src-tauri/src/`: fake bit-depth detection (exact quantization grid), lossy-transcode
assessment (spectral cutoff, AAC MDCT grid, encoder tags, 4-state verdict), DR14, LUFS and true
peak (`ebur128`), bandwidth and stereo analysis, plus a regression corpus.
It becomes a `pistolame-analysis` crate for the file authenticity report, **only once playback is
solid and optimized**. Leave out its `rodio` player and the Tauri glue.
