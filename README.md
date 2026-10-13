# Pistolame

A native lossless audio player built with Rust and Slint. Plays on Windows (WASAPI) and macOS
(CoreAudio); the Linux audio backend is planned.

## Download

Get the Windows x64, macOS universal (Apple Silicon + Intel, macOS 11+) or Linux x64 archive from
[Releases](https://github.com/Dacryoserum/pistolame/releases) and check its SHA-256 file.

- **Windows**: FLAC playback through WASAPI, shared (default) or exclusive mode. Use the
  `…-setup.exe` installer (per user, no administrator rights) or the portable ZIP. Unsigned build.
- **macOS**: FLAC playback through CoreAudio, shared or exclusive mode (hog mode at the file's
  sample rate). Ad-hoc signed, not notarized: macOS may ask for approval in System Settings >
  Privacy & Security on first launch. Move the app to Applications so it can update itself.
- **Linux**: interface and library only; the ALSA backend is not implemented yet. Targets
  Ubuntu 24.04 or newer with fontconfig, xkbcommon and a working graphics stack.

Installed copies check GitHub for new releases and update themselves after one click; every
download is verified against an ed25519 signature before anything is replaced.

## What it does

- **Library**: import FLAC files or folders (or drop them on the window); albums are
  remembered between launches and the folders are watched, so new music shows up on its own
  (files that vanish with an unplugged drive are kept). Browse by album (sortable, natural
  order), artist or track (a sortable table), see what was added or played recently, search
  albums and tracks, and mark favorites. Edit an album's title, artist and cover in the app;
  your music files are never modified.
- **Playback**: play, pause, seek, previous/next, shuffle and repeat, from the window, the
  keyboard or the media keys (and the system's *Now Playing* panel). Playlists (with covers,
  M3U export) and an *Up next* queue: "Play next" or "Add to queue" from any track, album or
  playlist. The app reopens on the last track, paused where you left it.
- **Output**: choose the device and the mode. Exclusive mode holds the device at the file's
  format; the volume is then set on your DAC. Shared mode offers a volume in decibels (0 to
  -60 dB) applied before the system mixer.
- **Proof**: the quality badge describes the real audio path (Bit-perfect, Exclusif, Mode
  partagé); hover it for details, click it for the signal path and two independent checks:
  decoded samples versus the device buffer, and decoded samples versus the FLAC's stored MD5.
  These are software checks at the device-buffer boundary, not a measurement of the DAC's output.

## Run from source

Install the Rust toolchain specified in `rust-toolchain.toml` and the platform build tools.

```powershell
cargo run --release -p pistolame-app
```

From an empty library, add files or a folder (or drop them on the window), open an album, then
choose a track. Keyboard shortcuts (Cmd on macOS, Ctrl elsewhere) are listed on the Réglages
page: Space toggles playback, Cmd/Ctrl+arrows change track or volume, Cmd/Ctrl+F searches,
Cmd/Ctrl+1 to 4 switch views, Escape goes back. Click the quality badge or output icon to see
verification details. Seek by releasing the pointer on the position bar.

Views whose backend is not implemented (authenticity, spectrograms) show an unavailable state;
authenticity reports never use simulated results.

Click the title in the central display to open the current track page. It follows playback,
shows artwork, metadata and the two real verification results, and returns to the previous view
with Back or Escape. The quality badge still opens the signal-path popover.

## Limits of 0.2.0

No gapless playback, authenticity analysis or spectrograms yet; FLAC only, up to 24 significant
bits. Hardware acceptance on the reference DAC and screen-reader testing are still pending.

## Development

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Hardware tests and the graphical snapshot test are ignored by default; they must be run
explicitly. The snapshot test does not play audio.

The crates separate audio processing (`pistolame-core`), platform output
(`pistolame-output`), metadata (`pistolame-library`) and UI (`pistolame-app`).
`prototypes/` is a separate workspace of throwaway design experiments.

See `PRODUCT.md`, `DESIGN.md` and `docs/decisions/0001-ui-framework.md` for the product,
visual system and UI choice.

## License

[GPL-3.0](LICENSE). The app and prototypes bundle
[Inter](https://rsms.me/inter/) (SIL OFL 1.1) and
[Phosphor](https://phosphoricons.com) icons (MIT).
Their licenses are included beside the assets.
