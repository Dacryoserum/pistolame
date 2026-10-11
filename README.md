# Pistolame

A native lossless audio player built with Rust and Slint. Windows is the first supported
playback platform; macOS and Linux audio backends are planned.

## Development preview

Download the Windows x64, macOS universal (Apple Silicon + Intel) or Linux x64 archive from
[Releases](https://github.com/Dacryoserum/pistolame/releases). Mac and Linux preview the interface
and FLAC import only: their audio backends are not implemented. Mac requires macOS 11 or later;
Linux builds target Ubuntu 24.04 or newer with fontconfig, xkbcommon and a working graphics stack.

The Mac app is ad-hoc signed, without Apple Developer signing or notarization. Windows is
unsigned. Consult the release notes for platform limits. Verify archive SHA256 files after download.
Project handoff and local plans are synchronized in the private
[pistolame-docs](https://github.com/Dacryoserum/pistolame-docs) companion repository.

## Current player

- Import FLAC files or folders into an album grid with titles, artists and square covers.
  Embedded covers and JPEG/PNG files beside the music are supported.
- Open an album and choose a track. Play, pause, stop, seek, and select the previous or next
  track with the transport controls.
- Select an output device and choose shared mode (default) or WASAPI exclusive mode.
  Pausing releases the device. The output preference and selected device are remembered.
- Inspect the signal path and two independent checks: decoded samples versus the device
  buffer in exclusive mode, and decoded samples versus the FLAC's stored MD5 when present.

The badge describes the active audio path. A shared-mode path cannot claim bit-perfect
transmission. The verification checks are software checks at the device-buffer boundary,
not a physical measurement of the DAC's analog output.

## Run from source

Install the Rust toolchain specified in `rust-toolchain.toml` and the platform build tools.

```powershell
cargo run --release -p pistolame-app
```

From an empty library, add files or a folder, open an album, then choose a track. Once populated, use Ctrl+O or Ctrl+Shift+O to add more music. Space toggles playback; Escape
closes the signal-path panel or returns to the album grid. Ctrl+T switches themes; Ctrl+O imports files, Ctrl+Shift+O imports a folder, and Ctrl+. stops playback. Click the quality badge or output
icon to see verification details. Seek by releasing the pointer on the position bar.

Adjust listening volume on the DAC. The toolbar slider is disabled because software gain and endpoint volume control are not implemented.

The three navigation groups, album grid, album detail and two themes follow the visual prototype.
Views whose backend is not implemented show an unavailable state; authenticity reports never use simulated results.

Click the title in the central display to open the current track page. It follows playback,
shows artwork, metadata and the two real verification results, and returns to the previous view
with Back or Escape. The quality badge still opens the signal-path popover.

## First-version limits

The imported album collection is kept for the current session only. Persistent indexing,
gapless playback, playlists, authenticity analysis,
spectrograms, other audio formats, and CoreAudio/ALSA outputs are not implemented yet.
Album playback advances automatically, with shuffle and album repeat. Transitions reopen the stream and are not gapless. FLAC playback supports up to 24 significant bits.

Hardware acceptance of the integrated desktop player is required before publishing v0.1.0.
Windows preview executables are unsigned.

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
