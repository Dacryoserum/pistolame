# Pistolame

A lossless audio player for audiophiles, for Windows, macOS and Linux. It aims to be light, fast, and
able to **prove** what it claims.

> **Status: early design stage.** Nothing is playable yet. The repository holds the product and
> design documents and some throwaway UI prototypes.

## Goals

- **Bit-perfect playback** with exclusive device access (WASAPI exclusive, CoreAudio hog mode, ALSA `hw:`),
  gapless transitions and automatic sample-rate switching.
- **Proof, not claims**: a bit-perfect badge computed from the real pipeline state, a live MD5 check
  during playback, and a visible signal path.
- **File authenticity report**: it detects fake 24-bit, upsampled "hi-res" and lossy transcodes,
  and measures dynamic range and clipping.
- **A calm, cover-led interface** with one brass accent and a native GPU-rendered UI (no web view).
- Formats: FLAC first, then the other lossless formats, then DSD/DoP.

## Tech

- Rust
- [Slint](https://slint.dev) for the UI (see [ADR 0001](docs/decisions/0001-ui-framework.md))

## Repository layout

```
PRODUCT.md        users, jobs to be done, product principles
DESIGN.md         visual system (tokens, type, components)
docs/decisions/   architecture decision records
docs/ideas.md     idea backlog
prototypes/       throwaway UI spikes (Slint, iced, GPUI)
```

To run a prototype:

```
cd prototypes
cargo run -p proto-slint-am
```

## License

[GPL-3.0](LICENSE). The prototypes bundle the [Inter](https://rsms.me/inter/) font (SIL OFL 1.1) and
[Phosphor](https://phosphoricons.com) icons (MIT). Their licenses are included next to them.
