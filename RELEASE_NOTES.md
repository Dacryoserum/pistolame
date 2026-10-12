# Pistolame 0.1.0-preview.2

Development preview of the native Rust/Slint player. Hardware acceptance of the integrated
player is still pending; this is not the completed M1 release.

- Windows x64: FLAC playback through WASAPI shared or exclusive mode.
- macOS universal (Apple Silicon and Intel, macOS 11+): interface, metadata and artwork import;
  **no audio output yet**. CoreAudio is not implemented.
- Linux x64 (built on Ubuntu 24.04): interface and import; **no audio output yet**.
  ALSA is not implemented. Requires a desktop session, fontconfig, xkbcommon and graphics drivers.

The approved album grid and album detail use square artwork, Inter type and brass accents in
both themes. Playback includes seeking, previous/next, shuffle and album repeat on Windows.
Click the central title to open the current track page; Back/Escape restores the previous view.
Software verification distinguishes transmission to the device buffer from FLAC file integrity.

Known limits: session-only library, inactive volume slider (use the DAC), no gapless playback,
playlists, authenticity analysis or spectrogram computation. Several sidebar views are placeholders.
Narrator and the full K11 hardware acceptance checklist have not been validated.

Archives include licenses and SHA256 checksums. Windows is unsigned. The Mac bundle has only
an ad-hoc signature, with no Apple Developer signature or notarization. macOS may require
explicit approval in System Settings > Privacy & Security after an attempted launch.

To resume development, clone the source and the private `Dacryoserum/pistolame-docs` repository.
Follow its Mac setup and handoff guide. The binary archives are for previewing the application;
source and project memory are versioned separately.

# Changelog

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
