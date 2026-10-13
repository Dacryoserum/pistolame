# Pistolame 0.1.0

First public release of Pistolame, a native lossless player for audiophiles (Rust + Slint,
GPL-3.0). Bit-perfect playback you can verify.

## Platforms

- **Windows x64**: FLAC through WASAPI, shared mode (default) or exclusive mode.
- **macOS universal** (Apple Silicon and Intel, macOS 11+): FLAC through CoreAudio. Shared mode,
  or exclusive mode (hog mode at the file's sample rate, exact float conversion). Built-in Mac
  outputs only accept float samples, so they show "Exclusif", never "Bit-perfect".
- **Linux x64**: interface and library only, no audio output yet (ALSA is planned).

## What is new since the previews

- Sound on macOS (CoreAudio backend).
- Persistent library: albums are kept between launches and rescanned in the background.
- Edit an album's title, artist and cover inside the app (your files are never modified).
- Playlists with covers, and an "Up next" queue: play next or add to the queue from anywhere.
- Shared-mode volume in decibels (0 to -60 dB); disabled in exclusive mode (use your DAC).
- Icon-only quality badge in the central display; hover for details, click for the signal path.
- Automatic updates: a green dot appears when a new release exists; one click downloads,
  verifies the signature and restarts the app.

## Known limits

- Windows: the shared-mode volume slider does not change the level yet.
- No gapless playback, authenticity analysis or spectrograms yet. FLAC only, up to 24 bits.
- Hardware acceptance on the reference DAC and screen-reader testing are still pending.
- Windows builds are unsigned; the Mac app has an ad-hoc signature only (no notarization).
  macOS may ask for approval in System Settings > Privacy & Security on first launch.

Archives include licenses and SHA-256 checksums; `latest.json` carries the update signatures.

# Changelog

## 0.1.0

### Features
- add CoreAudio backend with exact float exclusive mode (67be04e)
- persist the library and let users choose album covers (a2facc4)
- album editing, shared-mode volume, playlists, play queue and self-update (ac6bdbb)

### Fixes
- build the macOS package (41b5634)


## 0.1.0-preview.2

### Fixes
- build the macOS universal package (the preview.1 release job failed at the lipo check)


## 0.1.0-preview.1

### Documentation
- add GPL-3.0 license (f0292d3)
- add project README (e821fd5)
- add Nyquist analysis port to idea backlog (20c1c4e)
- add shared-mode badge, exclusive switch and two-level check (6a3caba)


### Features
- add core contract types and device format negotiation order (e1bba2b)
- persist output mode and device in a settings file (649607b)
- assess whether the signal path is bit-perfect (67d0610)
- add PCM container packing and canonical MD5 byte layout (aef9eb7)
- add WASAPI exclusive-mode backend and format probe (097ec05)
- decode FLAC to integer PCM with symphonia (d7f7d40)
- add WASAPI shared-mode backend and exclusive fallback (3345043)
- add playback ring, device feeder and verification tap (291433b)
- verify transmission and file integrity on two levels (a5607c3)
- add playback engine and play real FLAC files (937a8fe)
- add quiet file playback (541df76)
- add album player and multiplatform preview (bb0892f)


### Fixes
- draw scrubber thumb above progress rail (498ac9b)


### Maintenance
- initialize repository with design docs and UI prototypes (cf881fc)
- stop tracking implementation plans (08cb03f)
- add cargo workspace skeleton and CI (bd54aa8)
- add generated FLAC fixtures for bit-exact tests (b2d6ead)
- add Slint validation spike (dd75da1)
