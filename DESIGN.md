---
name: Pistolame
description: >
  Calm, artwork-led desktop music player in the spirit of a classic macOS music app
  (sidebar, transport toolbar with a central LCD, cover grid), with one burnished-brass accent
  and computed quality proof (bit-perfect badge, signal path, authenticity verdict).
# Source of truth for colors: prototypes/common/src/tokens.rs mirrors this table (0xRRGGBBAA).
# Change both together.
colors:
  dark:
    surface-content: "#1E1E1E"
    surface-sidebar: "#262626"
    surface-toolbar: "#262626"
    surface-lcd: "#2F2F2F"
    surface-raised: "#2C2C2C"
    surface-field: "#353535"
    border-lcd: "#3D3D3D"
    border-raised: "#3D3D3D"
    separator: "#383838"
    separator-chrome: "#141414"
    fill-control: "#333333"
    fill-control-hover: "#3D3D3D"
    fill-icon-hover: "#333333"
    fill-icon-pressed: "#3D3D3D"
    fill-row-hover: "#2A2A2A"
    fill-row-pressed: "#333333"
    fill-row-zebra: "#222222"
    fill-sidebar-hover: "#303030"
    fill-sidebar-selected: "#3A3A3A"
    rail: "#4A4A4A"
    rail-fill: "#A3A3A3"
    scrubber-thumb: "#EBEBEB"
    scrollbar-thumb: "#5A5A5A"
    text-primary: "#EBEBEB"
    text-secondary: "#A3A3A3"
    text-tertiary: "#6E6E6E"
    text-on-accent: "#1A1204"
    accent: "#E5B454"
    accent-hover: "#EDC26A"
    accent-pressed: "#C99A3E"
    focus-ring: "#E5B454"
    status-ok: "#4CC38A"
    status-ok-tint: "#21352B"
    status-caution: "#FF8A4C"
    status-caution-tint: "#3B2A21"
    shadow-artwork: "#00000059"
    shadow-popover: "#00000073"
  light:
    surface-content: "#FFFFFF"
    surface-sidebar: "#F2F2F2"
    surface-toolbar: "#F2F2F2"
    surface-lcd: "#EBEBEB"
    surface-raised: "#FFFFFF"
    surface-field: "#E3E3E3"
    border-lcd: "#DADADA"
    border-raised: "#DADADA"
    separator: "#E3E3E3"
    separator-chrome: "#D6D6D6"
    fill-control: "#EDEDED"
    fill-control-hover: "#E3E3E3"
    fill-icon-hover: "#E3E3E3"
    fill-icon-pressed: "#D9D9D9"
    fill-row-hover: "#F3F3F3"
    fill-row-pressed: "#E8E8E8"
    fill-row-zebra: "#F7F7F7"
    fill-sidebar-hover: "#E6E6E6"
    fill-sidebar-selected: "#DCDCDC"
    rail: "#C8C8C8"
    rail-fill: "#636368"
    scrubber-thumb: "#1D1D1F"
    scrollbar-thumb: "#C1C1C1"
    text-primary: "#1D1D1F"
    text-secondary: "#636368"
    text-tertiary: "#A1A1A6"
    text-on-accent: "#FFFFFF"
    accent: "#8A5F00"
    accent-hover: "#7A5400"
    accent-pressed: "#6B4A00"
    focus-ring: "#8A5F00"
    status-ok: "#157443"
    status-ok-tint: "#E3F3EA"
    status-caution: "#A8420E"
    status-caution-tint: "#FBEADF"
    shadow-artwork: "#0000001F"
    shadow-popover: "#00000029"
  both-themes:
    overlay-artwork-hover: "#00000040"
    overlay-button: "#000000A6"
    overlay-button-hover: "#000000D9"
    overlay-icon: "#FFFFFF"
    spectrogram-label: "#FFFFFFB3"
    spectrogram-cutoff: "#FF8A4C"
typography:
  family-text: "Inter (static TTF: Regular 400, Medium 500, SemiBold 600)"
  family-display: "Inter Display (static TTF: Bold 700)"
  roles:            # size px / line box px / weight
    display:   { family: Inter Display, size: 26, box: 32, weight: 700 }
    title-2:   { family: Inter, size: 20, box: 26, weight: 500 }
    title-3:   { family: Inter, size: 17, box: 22, weight: 600 }
    popover-title: { family: Inter, size: 15, box: 20, weight: 600 }
    headline:  { family: Inter, size: 13, box: 16, weight: 600 }
    body-medium: { family: Inter, size: 13, box: 16, weight: 500 }
    body:      { family: Inter, size: 13, box: 16, weight: 400, multiline-line-height: 18 }
    callout:   { family: Inter, size: 12, box: 16, weight: 400, multiline-line-height: 17 }
    callout-medium: { family: Inter, size: 12, box: 16, weight: 500 }
    label:     { family: Inter, size: 11, box: 14, weight: 600 }
    caption:   { family: Inter, size: 11, box: 14, weight: 500 }
spacing: [2, 4, 6, 8, 12, 16, 20, 24, 28, 32, 40, 48]
rounded: { xs: 3, sm: 4, md: 6, lg: 8, popover: 10, pill: "height/2" }
motion:
  instant: 0ms
  quick: { duration: 120ms, easing: ease-out-quint }
  standard: { duration: 200ms, easing: ease-out-quint }
---

# Design system: Pistolame

## 1. Stance

The layout, density and calm of a classic macOS music app; the identity of an instrument.

- **Borrowed (structure, not assets):** left sidebar with a search field and grouped sources;
  a transport toolbar whose centre is an "LCD" showing the current track; an artwork grid;
  album pages with a large cover, title, artist in the accent color, two action buttons and a
  zebra track list. 13 px body, 28 px sidebar rows, 36 px track rows, neutral grays, color
  coming from the covers. No Apple logo, name, icon, font or screenshot is used.
- **Pistolame's own:** a single burnished-brass accent ("laiton"); the quality proof surfaced
  where the eye already is (a computed badge in the LCD, a "Qualité" section on album pages,
  a spectrogram that *shows* the cutoff); French copy with correct typography.
- **Mode:** Operate. The world lends type, palette, density and one signature move (the proof
  badge → signal path). Navigation and controls stay standard.
- **No translucency.** Native vibrancy/backdrop blur exists in neither Slint nor iced. Chrome
  (sidebar + toolbar) uses one solid tinted surface, one step lighter than content, framed by
  a 1 px `separator-chrome` line. This reads as the same L-shaped chrome without the blur.

## 2. Color

Neutral grays carry the structure; the accent is rare; status colors appear only with an icon
and a word. Dark is primary (evening listening at a desk, artwork glows on dark); light is a
composed theme, not an inversion.

**Accent — laiton (brass).** Dark `#E5B454`, light `#8A5F00`. Used for: sidebar icons (always,
not only when selected), the artist name on album pages, the primary "Lecture" button, the
now-playing bars in track lists, active shuffle/repeat icons, the back link, the focus ring.
Never for large fills or decoration. It is warm and hi-fi (backlit amplifier displays) and far
from the red of the app it is inspired by.

**Status.** `status-ok` (green) = verified / bit-perfect / authentic. `status-caution` (orange)
= doubtful authenticity, the processing step in a signal path. A "processed" (non bit-perfect)
signal is shown **neutral** (`fill-control` + `text-primary` + sliders icon + the word
"Traité"): it is a state the user chose (volume), not an error. Shared mode is shown the same
neutral way with its own icon and word ("Mode partagé"): the Windows mixer *may* alter the
signal, so bit-perfect is not claimed, but nothing is known to be wrong either.

**Contrast (WCAG 2.1 formula, computed).** Text needs 4.5:1; icons 3:1.

| Dark fg \ bg | content | sidebar | lcd | raised | field | zebra | row-hover | sidebar-sel | control |
|---|---|---|---|---|---|---|---|---|---|
| text-primary `#EBEBEB` | 14.0 | 12.7 | 11.2 | 11.7 | 10.3 | 13.3 | 12.0 | 9.5 | 10.6 |
| text-secondary `#A3A3A3` | 6.6 | 6.0 | 5.3 | 5.5 | 4.9 | 6.3 | 5.7 | 4.5 | 5.0 |
| accent `#E5B454` | 8.7 | 7.9 | 7.0 | 7.3 | 6.4 | 8.3 | 7.5 | 6.0 | 6.6 |
| status-ok `#4CC38A` | 7.5 | 6.8 | 6.0 | 6.3 | 5.5 | 7.2 | 6.5 | 5.1 | 5.7 |
| status-caution `#FF8A4C` | 7.1 | 6.5 | 5.7 | 6.0 | 5.3 | 6.8 | 6.1 | 4.9 | 5.4 |

| Light fg \ bg | content | sidebar | lcd | raised | field | zebra | row-hover | sidebar-sel | control |
|---|---|---|---|---|---|---|---|---|---|
| text-primary `#1D1D1F` | 16.8 | 15.0 | 14.1 | 16.8 | 13.1 | 15.7 | 15.2 | 12.3 | 14.4 |
| text-secondary `#636368` | 6.0 | 5.3 | 5.0 | 6.0 | 4.7 | 5.6 | 5.4 | *4.4* | 5.1 |
| accent `#8A5F00` | 5.6 | 5.0 | 4.7 | 5.6 | *4.4* | 5.3 | 5.1 | *4.1* | 4.8 |
| status-ok `#157443` | 5.8 | 5.2 | 4.9 | 5.8 | 4.5 | 5.4 | 5.2 | *4.2* | 5.0 |
| status-caution `#A8420E` | 6.1 | 5.4 | 5.1 | 6.1 | 4.7 | 5.7 | 5.5 | *4.4* | 5.2 |

Italic cells are below 4.5 and are therefore **icon-only** pairs (≥ 3:1): no text of that color
is ever placed on that background. Other pairs: on-accent text 9.7 (dark) / 5.6 (light),
hover 11.1 / 6.8, pressed 7.2 / 8.1; badge text on its tint: ok 5.9 / 5.1, caution 5.8 / 5.2.
`text-tertiary` is for disabled controls and decorative empty-state icons only, never for
information.

## 3. Typography

**Inter** (SIL OFL 1.1) replaces the platform face, which cannot be redistributed. Static TTF
files from the official release, pinned:
`https://github.com/rsms/inter/releases/download/v4.1/Inter-4.1.zip`, entries
`extras/ttf/Inter-Regular.ttf`, `extras/ttf/Inter-Medium.ttf`, `extras/ttf/Inter-SemiBold.ttf`,
`extras/ttf/InterDisplay-Bold.ttf`, and `LICENSE.txt`. Their typographic family names
(name ID 16) are `Inter` (weights 400/500/600) and `Inter Display` (700); both Slint
(fontique) and iced (fontdb) resolve families by name ID 16, so `family "Inter" + weight`
selects the right file. Static files, not the variable font: weight axes are not reliably
applied by either renderer.

Optical split as on the reference platform: **Inter Display** only for the 26 px titles,
**Inter** everywhere else. One family, four files, ~1.6 MB.

| Role | Face | Size / line box | Use |
|---|---|---|---|
| display | Inter Display Bold | 26 / 32 | page title, album title |
| title-2 | Inter Medium | 20 / 26 | album artist (accent) |
| title-3 | Inter SemiBold | 17 / 22 | section title "Qualité" |
| popover-title | Inter SemiBold | 15 / 20 | popover heading |
| headline | Inter SemiBold | 13 / 16 | sub-headings, button labels |
| body-medium | Inter Medium | 13 / 16 | tile title, LCD title, values, playing track |
| body | Inter Regular | 13 / 16 (18 multi-line) | track titles, sidebar items, summaries |
| callout | Inter Regular | 12 / 16 (17 multi-line) | LCD artist line, footers, descriptions |
| callout-medium | Inter Medium | 12 / 16 | album meta line |
| label | Inter SemiBold | 11 / 14 | sidebar section headers, badges |
| caption | Inter Medium | 11 / 14 | format per track, time labels, data labels |

Rules: no letter-spacing (iced has none, and the roles do not need it); no uppercase labels;
sentence case everywhere. Single-line text sits in a fixed-height box and is vertically
centered (removes line-height differences between frameworks). Inter's digits are
proportional and neither framework exposes OpenType `tnum`: time labels sit in fixed-width,
end-aligned boxes so they do not shift the layout.

## 4. Space, radii, elevation

- **Spacing scale** (px): 2, 4, 6, 8, 12, 16, 20, 24, 28, 32, 40, 48. Content gutters 32;
  grid gaps 24; groups separated by 28–40, items inside a group by 2–12.
- **Radii:** 3 (tiny), 4 (LCD thumbnail), 6 (artwork in grid, LCD, rows, sidebar items, icon
  buttons), 7 (search field), 8 (album-page artwork), 10 (popover), pill = height/2 (action
  buttons, badges).
- **Elevation** (declared once per element):
  - level 0: everything flat; regions separated by 1 px lines, not shadows.
  - level 1, artwork: shadow `0 2 8` in `shadow-artwork`. No border.
  - level 2, popover: shadow `0 8 24` in `shadow-popover` + 1 px `border-raised` (the border is
    what defines the edge on dark; kept in light for consistency).

## 5. Motion

Motion explains state; nothing else moves.

| Token | Value | Use |
|---|---|---|
| instant | 0 ms | hover and pressed fills, navigation between views, play/pause glyph swap |
| quick | 120 ms ease-out-quint | LCD time labels and scrubber thumb appearing on hover |
| standard | 200 ms ease-out-quint | popover open (opacity 0→1, y −4→0); close is instant |
| continuous | per frame | progress fill, now-playing bars (stop when paused) |

`ease-out-quint` ≈ cubic-bezier(0.22, 1, 0.36, 1); both Slint (`ease-out-quint`) and iced
(`animation::Easing::EaseOutQuint`) have it by name. No bounce, no stagger, no page-load
choreography. With "reduce animations" on, quick/standard become instant; progress and bars
keep updating (they are state).

## 6. Iconography

**Phosphor Icons** core v2.0.8 (MIT). Regular weight for UI, Fill weight for transport and
status, like the reference app mixes outline and filled symbols. All files are single-color
paths with `fill="currentColor"`, viewBox 256: tint them (Slint `colorize`, iced
`svg::Style::color`). Raw URL pattern:
`https://raw.githubusercontent.com/phosphor-icons/core/v2.0.8/assets/<weight>/<name>.svg`,
license `https://raw.githubusercontent.com/phosphor-icons/core/v2.0.8/LICENSE`.

| File | Weight | Use |
|---|---|---|
| magnifying-glass | regular | search field |
| clock-counter-clockwise | regular | Ajouts récents |
| microphone-stage | regular | Artistes |
| vinyl-record | regular | Albums |
| music-note | regular | Morceaux |
| seal-check | regular | Authenticité (sidebar) |
| waveform | regular | Spectrogrammes |
| speaker-hifi | regular | Sortie audio (sidebar), output button |
| playlist | regular | playlists |
| shuffle | regular | shuffle toggle, "Aléatoire" |
| repeat | regular | repeat toggle |
| skip-back-fill | fill | previous |
| play-fill | fill | play, "Lecture", tile overlay |
| pause-fill | fill | pause |
| skip-forward-fill | fill | next |
| speaker-low | regular | volume min |
| speaker-high | regular | volume max |
| caret-left | regular | back |
| seal-check-fill | fill | Bit-perfect, Authentique |
| warning-fill | fill | Douteux |
| sliders-horizontal | regular | Traité (processed signal) |
| intersect | regular | Mode partagé (signal goes through the system mixer) |
| waveform | regular | Exclusif (held exclusively, exact float delivery) |

Sizes: 14 (inline in fields/buttons), 16 (sidebar, toggles), 18 (prev/next, output), 22 (play).

## 7. Components

Exact geometry lives in `docs/plans/maquette-apple-music.md`; this is the contract.

- **Sidebar item** — 220×28, radius 6, icon 16 in `accent`, label body `text-primary`.
  Hover `fill-sidebar-hover`, selected `fill-sidebar-selected`. Section header: label role,
  `text-secondary`, 24 high.
- **Search field** — 216×28, radius 7, `surface-field`, magnifying-glass 14 `text-secondary`,
  placeholder "Rechercher" `text-secondary`, focused: 2 px `focus-ring` border.
- **Icon button (toolbar)** — 28/32/36 square, radius 6, glyph `text-primary` (toggles
  `text-secondary` off, `accent` on). Hover `fill-icon-hover`, pressed `fill-icon-pressed`.
- **LCD** — 44 high, 360–560 wide, radius 6, `surface-lcd` + 1 px `border-lcd`. Left: 36 px
  cover. Centre: title (body-medium) over "Artiste — Album" (callout, secondary), centered,
  elided; under them a 3 px progress rail (`rail` / `rail-fill`). On hover: elapsed and
  remaining times appear at both ends of line 2 and a 9 px thumb on the rail (drag to seek).
  Right: the quality badge.
- **Quality badge** — pill, 20 high, padding 8, gap 4, icon 12 + label role text.
  *Bit-perfect · 24 bits / 96 kHz*: `status-ok-tint` / `status-ok` / seal-check-fill.
  *Traité · 24 bits / 96 kHz*: `fill-control` / `text-primary` / sliders-horizontal.
  *Mode partagé · 24 bits / 96 kHz*: `fill-control` / `text-primary` / intersect. The default output mode;
  the popover says why ("Le système peut convertir, rééchantillonner ou mixer le signal ; le
  bit-perfect n'est pas garanti") and shows the system mix format (e.g. 176,4 kHz, flottant).
  *Exclusif · 24 bits / 96 kHz*: `fill-control` / `text-primary` / waveform. The device is held
  exclusively at the source rate but only accepts float samples (macOS built-in outputs): the
  samples are converted to 32-bit float exactly, and what the system does next cannot be
  verified, so it is never "Bit-perfect".
  *Authentique*: ok colors, seal-check-fill. *Douteux*: `status-caution-tint` /
  `status-caution` / warning-fill. In the LCD the badge is a button that opens the signal path.
- **Exclusive mode switch** — a small labelled toggle "Mode exclusif" next to the LCD badge
  (off by default, remembered). The first time it is turned on, an inline note says once: "En
  mode exclusif, les autres sons de ce périphérique sont coupés pendant la lecture."
- **Album tile** — square artwork (radius 0 — square like a record sleeve, decided 2026-10-11; level 1), 8 gap, title body-medium, artist body
  `text-secondary`. Hover: 25 % black veil + 32 px round play button bottom-left.
- **Track row** — 36 high, radius 6, zebra on odd rows, hover `fill-row-hover`, pressed
  `fill-row-pressed`. Number (secondary, end-aligned) → title (body) → format (caption,
  secondary) → duration (body, secondary, end-aligned). Playing row: three animated accent
  bars replace the number; title becomes body-medium.
- **Pill buttons** — 112×32, radius 16, icon 14 + headline label. Primary "Lecture":
  `accent` / `text-on-accent` (hover `accent-hover`, pressed `accent-pressed`). Secondary
  "Aléatoire": `fill-control` / `accent` (hover `fill-control-hover`, pressed
  `fill-icon-pressed`). One primary per screen.
- **Signal path** — vertical list of 5 steps (Source, Décodage, Volume, Sortie, DAC), 32 px
  rows: 8 px node dot (`status-ok` when the step leaves samples untouched, `status-caution`
  when it changes them) joined by 1 px `separator` connectors; label secondary, value
  body-medium primary.
- **Popover** — 380 wide, radius 10, `surface-raised`, level 2, padding 16. Opens under the
  LCD, right edges aligned. Closes on Escape, outside click, or its trigger.

## 8. How the proof is surfaced (the signature move)

The reference app shows a "Lossless" badge you have to believe. Pistolame's badge is a
*measurement you can open*:

1. **Always visible, computed.** The LCD badge is derived from pipeline state
   (`is_bit_perfect(volume)` in the mock). Pull the volume below 100 % and it turns into
   "Traité · 24/96" immediately: the claim retracts itself.
2. **One click to the evidence.** The badge (and the output button) opens the signal path:
   each step with what it does to the samples, the one that alters them marked in caution
   color, plus the live check, on two lines with their own status:
   - **Transmission vérifiée**: the bytes delivered to the device equal the decoded PCM
     (exclusive mode only; in shared mode the line says it does not apply).
   - **Fichier intègre**: the decoded PCM matches the file's own MD5 (only when the file has
     one; otherwise the line says the file carries no checksum).
3. **Files are judged, not trusted.** Album pages end with "Qualité": signal path for this
   album on the current output, and an authenticity verdict with its measured reasons
   (effective bits, spectral cutoff, DR, clipping, MD5) and a spectrogram where a fake hi-res
   file visibly stops at 22,05 kHz, the cutoff drawn as a labelled line.

## 9. Do / Don't

Do: let covers carry color; keep one primary action per screen; pair every status color with
an icon and a word; compute every badge; write French with decimal commas and no-break spaces.

Don't: gradients on text or chrome; glass/blur imitations; colored side stripes on rows or
cards; cards around every group (use spacing and 1 px rules); uppercase tracked eyebrows;
monospace "tech" costume (Inter for numbers too); emoji or Unicode glyphs as icons; glow
halos; a second accent; claims without a reason ("Qualité studio !").
