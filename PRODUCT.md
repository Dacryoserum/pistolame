# Product

## Platform

Native desktop app (Windows first; macOS and Linux later). Rust, GPU-rendered UI, no web view.
UI language: French. Code and docs: English.

## Users

Audiophiles with a local library of lossless files (FLAC, mostly 16/44.1 to 24/192) and a
dedicated DAC (reference test device: FiiO K11 over WASAPI exclusive). They already know what
"bit-perfect", "24/96" or "DR12" mean. They have been burned by players that *claim* quality and
by downloads that turn out to be upsampled CDs or transcoded MP3s. They listen at a desk, often
in the evening, with the app open next to other windows for long sessions.

## Jobs to be done

1. **Play my music without it being altered**, and *see* that it is not: one glance tells whether
   the current path is bit-perfect, and one click shows why.
2. **Browse a large library by its covers**, quickly, the way a record shelf works.
3. **Know whether a file is what it says it is** (real hi-res, real 24-bit, not a lossy
   transcode, how much dynamic range, any clipping) before trusting or buying it again.
4. **Switch tracks and albums gaplessly** without thinking about sample rates or devices.

## Product principles

1. **Proof, not claims.** Every quality statement shown (badge, verdict, path) is computed from
   real state and opens onto its evidence. If processing alters the signal, the UI says so, in
   words, at the same place the "bit-perfect" badge sits.
2. **Music first, proof one click away.** Covers and titles lead; technical detail is present
   but quiet (small badges, a "Qualité" section at the end of an album page, a popover).
3. **Calm and dense like a pro tool.** Neutral surfaces, one accent, 13 px body, no decoration
   that does not carry state. Familiar layout: sidebar, transport toolbar with an LCD, content.
4. **Honest wording.** Verdicts say "Douteux", not "Faux", when the evidence is statistical;
   they always state the measured reason ("le spectre s'arrête à 22,05 kHz").
5. **Light and fast.** Instant navigation, virtualized grids and lists, no blocking work on the
   UI thread, no animation that makes the user wait.

## Tone (French UI copy)

Precise, calm, technical without jargon theatre. Short sentences, no exclamation marks, no
marketing words ("ultime", "incroyable"). Use the audiophile's vocabulary as-is (bit-perfect,
24/96, DR, MD5, WASAPI exclusif). French typography: decimal comma (44,1 kHz), no-break space
before ":" and inside « », true minus sign (−6,0 dB).

## Accessibility commitments

- WCAG 2.2 AA contrast in **both** themes: text ≥ 4.5:1, icons and control boundaries ≥ 3:1.
  Ratios are computed and listed in DESIGN.md, not eyeballed.
- Status is never conveyed by color alone: every badge has an icon **and** a word.
- Smallest text is 11 px; body is 13 px.
- Everything is operable from the keyboard (Space play/pause, Escape back/close, arrow
  navigation in grids and lists, visible 2 px focus ring).
- Screen-reader names for every icon-only control (French labels). Note: Slint exposes
  AccessKit; iced 0.14 has no accessibility tree. This weighs in the framework choice.
- Respect the OS "reduce animations" setting: only state-carrying motion remains.
