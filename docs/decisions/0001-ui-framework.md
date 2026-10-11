# ADR 0001: UI framework, Slint 1.18.1 vs iced 0.14.0 vs egui/eframe 0.36.2

- **Status:** **Accepted** by the user on 2026-10-11 (proposed by the architect)
- **Date:** 2026-10-11 (egui added the same day)
- **Decision:** **Slint 1.18.1**, winit backend, femtovg renderer (default), `accessibility` on.
  Confidence: **medium-high (~70 %)**, unchanged after evaluating egui (runner-up, 3.85).
  A 2-day validation spike (section 7) must pass before `pistolame-app` is started.

## 1. Context

The Slint and iced mockups (`prototypes/proto-slint-am`, `prototypes/proto-iced-am`) implement
the same spec (`docs/plans/maquette-apple-music.md`) and look equally good to the user. egui
(with eframe) was added on the user's request, because Spotifast (a native Spotify client) is
built with it. **No egui mockup was built**; egui figures come from an empty-window probe
(one label, `scratchpad/egui-probe`), so they are a floor, not comparable to the mockups.
Measured on the user's Windows 10 machine (`prototypes/mesurer.ps1` method, release builds):

| | Slint 1.18.1 (mockup) | iced 0.14.0 (mockup) | egui 0.36.2 (empty window) |
|---|---|---|---|
| RAM (working set) | **149 MB** | 169 MB | glow: **91–108 MB**; wgpu: 217–259 MB |
| CPU while playing | **1.2 %** | 1.5 % | not measurable without a mockup (probe: ≤ 0.7 %) |
| CPU idle | | | 0 % (eframe sleeps on `ControlFlow::Wait`) |
| Window open | | | glow 61 ms; wgpu 73–226 ms |
| Release exe (stripped) | 16.7 MB | 15.7 MB | glow **7.0 MB**; wgpu 13.8 MB (no fonts/images yet) |
| Unique crates (Windows, normal deps) | 251 | 196 | glow **111**; wgpu 139 |
| winit version | 0.30.13 | 0.30.13 | 0.30.13 |

The UI must sit on top of the planned workspace (`AGENTS.md`): `pistolame-core`,
`pistolame-output` (real-time thread), `pistolame-library` (SQLite + watcher), and
`pistolame-app` talking to the engine only through messages. Capabilities below were checked
in the local crate sources (`~/.cargo/registry/src/index.crates.io-*/`) unless marked
otherwise.

## 2. Criteria and weights

| # | Criterion | Weight | Why |
|---|---|---|---|
| C1 | Fit with the backend (threads, 30–60 Hz push, async, custom pixels, 100k lists) | 25 | The product is the engine; the UI must not fight it. |
| C2 | Maintainability and ergonomics (the user edits the code) | 20 | Pillar 4 of `AGENTS.md`. |
| C3 | OS integration (tray, media keys, DnD, dialogs, windows, HiDPI, i18n, theme) | 15 | Desktop player table stakes. |
| C4 | Accessibility (screen readers) | 10 | Written commitment in `PRODUCT.md`. |
| C5 | Lightness and speed | 10 | Pillar 3. All three are "light"; differences are small. |
| C6 | Project health and API stability | 15 | Multi-year open source project. |
| C7 | Licensing | 5 | GPL-3.0 project; all three are compatible. |

## 3. Comparison

### C1. Backend fit (25)

| Need | Slint 1.18.1 | iced 0.14.0 | egui 0.36.2 |
|---|---|---|---|
| Engine → UI at 30–60 Hz | Components are `!Send`. `slint::invoke_from_event_loop(FnOnce + Send)` / `Weak::upgrade_in_event_loop` (`i-slint-core/api.rs:1227,1286`), or a `slint::Timer` at frame rate reading a lock-free snapshot (mockup). Property bindings re-evaluate only dependants. | `Subscription::run` / `run_with` (`iced_futures/subscription.rs:182`) or `window::frames()` polling a snapshot. Every message rebuilds the **whole** `view` tree (mitigation: `lazy`). | `egui::Context` is `Arc<RwLock<..>>`, `Clone + Send + Sync` (`context.rs:723`): an engine thread calls `ctx.request_repaint()` (`:1821`) or `request_repaint_after` (`:1872`); the UI reads a snapshot each frame. Immediate mode: every repaint re-runs layout **and** tessellation of the whole visible UI (no retained tree, no `lazy`). Fine for typical UIs, cost at 60 Hz on our screens unverified. |
| Async work | Threads / tokio + `invoke_from_event_loop`; `slint::spawn_local` (`slint/lib.rs:355`). Hand-written bridge. | First-class `Task::perform/run/sip` (`iced_runtime/task.rs:48-74`), `tokio` feature. Best of the three. | None built in. Threads + channel + `request_repaint`; polled each frame. As simple as Slint's bridge, less structured than iced. |
| Custom pixel buffer (spectrogram) | `Image::from_rgba8(SharedPixelBuffer)`; full re-upload per update. | `image::Handle::from_rgba` new texture per call; `shader` widget for partial updates. | `TextureHandle::set_partial` (`epaint/texture_handle.rs:78`) uploads only the new column: the cheapest live spectrogram of the three. |
| Custom GPU (3D indicator, `docs/ideas.md`) | `set_rendering_notifier` underlay, or wgpu 30 behind **`unstable-wgpu-30`**. | `shader` widget, wgpu 27, stable. | `egui_wgpu::Callback` / `CallbackTrait` (`egui-wgpu/renderer.rs:29,88`, wgpu 30) in any painter rect, stable API; `egui_glow` callback with the glow backend. On par with iced. |
| 100k-track list, sort/filter | `ListView` virtualizes; custom `Model::row_data` only for visible rows → page from SQLite. | No virtualized list; hand-virtualized grid in the mockup. | `ScrollArea::show_rows` (`scroll_area.rs:983`) and `egui_extras::TableBuilder` `rows`/`heterogeneous_rows` (`table.rs:1027,1112`) call back only for visible rows → page from SQLite. Sorting/filtering is our code (no model adapters). |
| Cover grid, off-thread decoding | Decode to `SharedPixelBuffer` on a worker, wrap in `Image` on UI thread. | Decode on a worker, `Handle::from_rgba` is `Send`. | `egui_extras` loaders (URI-keyed, cached, `forget_image` `context.rs:3764`) or own `TextureHandle` cache; texture eviction is manual. Grid virtualization by hand via `show_viewport` (`:1020`). |

**Verdict C1:** egui is the best backend fit on paper: thread-safe context, virtualized rows,
partial texture updates and a stable wgpu callback. Its cost is re-running the whole visible UI
every frame while playback animates. Slint 4/5, iced 4/5, egui 4.5/5.

### C2. Maintainability and ergonomics (20)

| | Slint (1 682 lines `.slint` + 461 Rust) | iced (2 328 lines Rust) | egui (no mockup) |
|---|---|---|---|
| Model | Declarative DSL + Rust controller; state in two places. | Elm: one `App`, one `Message`, `update`, `view`. | Plain Rust functions that draw and handle input in the same place; app state is a normal struct. No messages, no bindings: the simplest to read for small screens. |
| Readability | `.slint` reads like the design spec. | Builder chains, hand layout math. | Imperative nested closures (`ui.horizontal(...)`). Single-pass layout: centring or right-aligning content of unknown size needs `Sides`, manual sizing or a discarded frame (`request_discard`), a known egui quirk. |
| Look control (`DESIGN.md`) | Mockup matches spec. | Mockup matches spec minus ellipsis and animations. | Unverified (no mockup). Available: custom fonts (`add_font`), harfrust shaping with GPOS kerning and skrifa/vello_cpu rasterizing since 0.34–0.35, per-text `line_height` and `extra_letter_spacing` (`widget_text.rs:160,174`), truncation with "…" (`Label::truncate`), rounded rects, `Shadow` (offset/blur/spread), two-stop `gradient_rect` only (`shape.rs:306`), `animate_bool_with_time_and_easing` / `animate_value_with_time`, `Popup`, smooth scrolling. Styling is global `Style`/`Visuals`: default widgets look "egui" (tight spacing, its own focus/selection visuals). Reaching the spec means writing our own buttons, rows, sidebar items, LCD, popovers with `ui.painter()` + `ui.interact`. Spotifast shows it is possible but built a dedicated layer for it (`fastframe-text/-fonts/-icons/-theme/-scroll` crates, [Cargo.toml](https://github.com/crmne/spotifast/blob/main/Cargo.toml)). |
| Tooling | VS Code LSP, **live preview**, testing backend. | `hot` reload, time-travel debugger, `tester`. | rust-analyzer only; no live preview. `egui_kittest` (AccessKit-based UI tests), `inspection` feature + `egui_mcp` (0.35). Hot reload unverified. |
| Refactoring safety | Manual across Rust/`.slint`. | Full rustc. | Full rustc. |

**Verdict C2:** egui is the easiest *code* to read and change, but the Apple Music-like look is
a pile of custom painted widgets the user would own, with no live preview to tweak them. Slint
4/5, iced 3.5/5, egui 3.5/5.

### C3. OS integration (15)

| Need | Slint | iced | egui / eframe |
|---|---|---|---|
| Tray icon | **Built-in** `SystemTrayIcon` (1.17). | `tray-icon` crate. | None; `tray-icon` crate (Spotifast uses its own `fastframe-tray`). |
| Media keys / SMTC / MPRIS | `souvlaki` with HWND behind `raw-window-handle-06`. | `souvlaki` with HWND from `window::run`. | `souvlaki` with HWND: `Frame` implements `HasWindowHandle` (`eframe/epi.rs:701`), `Frame::winit_window()` (`:766`). |
| OS file/folder drop | Needs **`unstable-winit-030`** hook. | Built-in `FileDropped`. | Built-in `RawInput::dropped_files` / `hovered_files` (`raw_input.rs:64,81`). |
| File dialogs | `rfd` | `rfd` | `rfd` |
| Second window (mini-player) | Second `Window` component. | `iced::daemon` + `window::open`. | Viewports: `show_viewport_deferred` / `_immediate` (`context.rs:4062,4116`). |
| Menus | `MenuBar`, `ContextMenuArea`, native on macOS. | `muda` by hand. | egui-drawn menus / context menus; native menu bar via `muda` by hand. |
| i18n | Built-in `@tr()`, bundled translations. | None. | None (Spotifast: own `fastframe-i18n`). |
| Light/dark | Tokens + `color-scheme`. | `theme()` closure. | `Visuals` + `ctx.theme()` follows the OS. |

All third-party crates (`souvlaki`, `rfd`, `tray-icon`, `muda`) depend on raw-window-handle 0.6,
not on winit; all three frameworks pin winit 0.30.13, so no version conflict.

**Verdict C3:** Slint 4.5/5, iced 3/5, egui 3/5 (file drop and multi-window built in, nothing
else).

### C4. Accessibility (10)

| Slint | iced | egui |
|---|---|---|
| AccessKit on by default, live regions, landmarks (1.17–1.18). | **None** upstream ([#552](https://github.com/iced-rs/iced/issues/552)). | AccessKit **always on** since 0.34 (`accesskit 0.24`, `accesskit_winit 0.32.2` in the tree); 0.34–0.35 exposed scroll bars, splitters, pressed state ([releases](https://github.com/emilk/egui/releases)). Built-in widgets get roles/labels; every **custom-painted** widget (most of ours, see C2) must call `Response::widget_info` (`response.rs:869`) itself. Spotifast enables it. NVDA/Narrator quality unverified. |

Slint 5/5, iced 1/5, egui 4/5.

### C5. Lightness and speed (10)

- Slint −20 MB RAM and −0.3 pt CPU vs iced (mockups).
- egui with **glow** is the lightest base measured (91–108 MB, 7 MB exe, 111 crates, 61 ms
  to window, 0 % idle). With **wgpu** the empty window already uses 217–259 MB: pick glow
  (Spotifast does) unless the 3D idea needs wgpu.
- egui repaints only on input, `request_repaint` or a running animation (eframe
  `ControlFlow::Wait`/`WaitUntil`, `native/run.rs:122,252`). While a track plays and the LCD
  progresses, it re-lays out and redraws the whole window at the chosen rate; Slint re-evaluates
  only the properties that changed. Cost on a full UI unverified.

Slint 4.5/5, iced 4/5, egui 4/5.

### C6. Project health and stability (15)

| | Slint | iced | egui |
|---|---|---|---|
| Versioning | 1.x semver since 2023. | 0.x, every release breaking. | 0.x, every minor breaking: 0.34 replaced `App::update` with `logic`/`ui`, merged panels; 0.35 removed all deprecated items; 0.36 more removals, MSRV 1.95. Deprecations do precede removals. |
| Cadence | ~2 months. | ~yearly (0.14 Dec 2025). | 0.33 Oct 2025, 0.34 Mar 2026, 0.35 Jun, 0.36 Aug + patches ([releases](https://github.com/emilk/egui/releases)): ~3 breaking upgrades a year. |
| Maintainers | SixtyFPS GmbH, paid staff. | One dominant maintainer. | Emil Ernerfeldt (3 098 commits) and lucasmerlin (218); Ernerfeldt is co-founder/CTO of Rerun, which builds its viewer on egui and raised a $17M seed in March 2025 ([CB Insights](https://www.cbinsights.com/company/rerun/financials)). Funded, but steered by Rerun's needs. |
| Production users | LibrePCB 2.0, embedded. | Kraken Desktop, COSMIC (fork), Halloy. | Rerun viewer, [Spotifast](https://github.com/crmne/spotifast) (7.1k stars, egui 0.36 + glow), many tools. |
| Popularity | 24.1k stars | 31.7k stars | 31.2k stars, ~1 040 open issues+PRs |
| Docs | Full, versioned. | API docs + partial book. | API docs + live web demo; no book. |

Slint 4.5/5, iced 3/5, egui 3.5/5.

### C7. Licensing (5)

- **iced:** MIT. **egui/eframe:** `MIT OR Apache-2.0` (`egui-0.36.2/Cargo.toml:44`). Both
  GPL-compatible, no obligations beyond the notice.
- **Slint:** `GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0`.
  GPLv3 is compatible; "**only**" means a "GPL-3.0-or-later" Pistolame binary is effectively
  GPLv3. Royalty-free 2.0 needs `AboutSlint` or a badge. Recommendation: declare
  `GPL-3.0-or-later` for our code, rely on Slint's GPLv3 grant, show `AboutSlint` in About.

Slint 4/5, iced 5/5, egui 5/5.

## 4. Score

| Criterion | Weight | Slint | iced | egui |
|---|---|---|---|---|
| C1 Backend fit | 25 | 4 | 4 | 4.5 |
| C2 Maintainability | 20 | 4 | 3.5 | 3.5 |
| C3 OS integration | 15 | 4.5 | 3 | 3 |
| C4 Accessibility | 10 | 5 | 1 | 4 |
| C5 Lightness | 10 | 4.5 | 4 | 4 |
| C6 Health/stability | 15 | 4.5 | 3 | 3.5 |
| C7 Licensing | 5 | 4 | 5 | 5 |
| **Weighted (/5)** | | **4.30** | **3.35** | **3.85** |

Without accessibility (C4 weight moved to C1): Slint 4.20, egui 3.90, iced 3.65.

## 5. Risks and mitigations

### If Slint (recommended)

| Risk | Mitigation |
|---|---|
| OS file drop needs `unstable-winit-030` hook. | One module `pistolame-app/src/platform/winit_hooks.rs`; pin `slint = "=1.18.x"`; watch the changelog. |
| Custom GPU (3D indicator) needs `unstable-wgpu-30` or an OpenGL underlay. | Treat it as optional; pre-rendered sprite sheet animated as `Image` frames. |
| Two languages, two places holding state, setter boilerplate. | Domain state in Rust only; `.slint` holds view-local state; themes in `.slint`; one `ui_bridge` module. |
| `!Send` UI types vs engine threads. | Lock-free snapshot read by a `slint::Timer`; discrete events via `invoke_from_event_loop`. |
| Full texture re-upload for live spectrogram. | ≤ 30 Hz pushes; spike measures 60 Hz. |
| Vendor steers toward embedded/commercial. | GPLv3 grant is irrevocable for released versions. |

### If iced

| Risk | Mitigation |
|---|---|
| No accessibility. | Wait for #552 or vendor a fork (heavy). |
| Breaking API every release, single maintainer. | Pin; budget an upgrade per release. |
| Hand-made virtualization, no ellipsis, no tray/menus/i18n. | Own widgets; `tray-icon`, `muda`, `fluent`. |
| Full `view` rebuild at 60 Hz. | `lazy`; push at 30 Hz. |

### If egui

| Risk | Mitigation |
|---|---|
| Default look is "egui"; the `DESIGN.md` look needs custom painted widgets (unverified effort). | A `pistolame-app/src/widgets/` layer (button, row, sidebar item, LCD, popover) built once from `DESIGN.md` tokens; a 2-day mockup of the album page before committing. |
| ~3 breaking releases a year. | Pin `=0.36.x`; budget half a day per upgrade; keep egui calls inside the widget layer. |
| Whole-UI relayout each frame while playing. | Animate only what moves (`request_repaint_after` at 30 Hz for the LCD); profile with `puffin`. |
| Custom widgets silently invisible to screen readers. | Rule: every widget calls `widget_info`; `egui_kittest` test asserting AccessKit labels. |
| No tray, menus, i18n. | `tray-icon`, `muda`, `fluent` (as Spotifast did with its own crates). |
| wgpu backend heavy (217–259 MB empty). | glow backend; wgpu only if the 3D idea ships. |

## 6. Recommendation

**Choose Slint 1.18.1. Evaluating egui does not change the recommendation.** Deciding factors:

1. **Accessibility** is a written commitment: Slint ships it fully; egui ships AccessKit too
   (a real advantage over iced) but our custom-painted widgets would each need manual labels.
2. **Stability for a multi-year codebase:** Slint 1.x semver vs egui's ~3 breaking releases a
   year and iced's 0.x with a bus factor of one.
3. **Built-in desktop features:** tray, menus, translations, virtualized `ListView` + `Model`.
   egui has virtualized rows and file drop, but tray, menus and i18n are ours to assemble.
4. **Editing the look:** `.slint` + live preview fits the user's wish to tweak the UI; with
   egui the Apple Music-like look is custom painting code with no preview, and it is the one
   option whose look is unproven here (no mockup).

egui is the clear runner-up (3.85): best backend fit (thread-safe context, `show_rows`,
`set_partial`, stable wgpu callback), lightest base with glow, single language, and Spotifast
proves a polished music client is possible with it, at the price of a dedicated widget layer.

## 7. Validation spike before `pistolame-app` (2 days, `prototypes/spike-slint-backend`)

Pass criteria (all on Windows 10, release build):
1. `ListView` over a custom `Model` with 100 000 rows, sort and filter in < 100 ms, scroll at
   display rate, RAM growth < 30 MB.
2. Spectrogram `SharedPixelBuffer` 1024×256 updated at 60 Hz from a worker: CPU < 5 %.
3. OS file/folder drop received through `on_winit_window_event` (`DroppedFile`).
4. `souvlaki` SMTC: play/pause/next from media keys with the HWND from `window_handle()`.
5. `SystemTrayIcon` with a menu, and a second mini-player window sharing state.

## 8. What would make us reconsider

- Any spike criterion fails without a reasonable workaround (egui is then the fallback).
- iced merges AccessKit upstream **and** ships a stable release.
- egui reaches 1.0 or a slower breaking cadence, **and** an egui album-page mockup matches
  `DESIGN.md` at Slint's RAM/CPU (glow backend).
- Slint drops or restricts the GPLv3 / royalty-free desktop options.
- The 3D device indicator (or other live GPU visuals) becomes core while Slint's wgpu interop
  is still `unstable-*` (iced and egui both have stable wgpu hooks).
- The user, after editing the mockups, finds a single-language model clearly easier.

## Unverified

- 60 Hz full-texture upload cost in Slint femtovg (spike item 2).
- Skia renderer binary size delta.
- iced master accessibility beyond issue #552.
- souvlaki behaviour with each framework (expected identical).
- egui: CPU/RAM of a full Pistolame UI (only an empty-window probe was measured, single run);
  effort and fidelity of the `DESIGN.md` look; NVDA/Narrator quality; hot reload support;
  lucasmerlin's affiliation with Rerun.
