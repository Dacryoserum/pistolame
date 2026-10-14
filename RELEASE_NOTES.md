# Pistolame 0.4.0

Getting around, the way you expect.

- **Back takes you back.** Open an album from Artistes, Morceaux, a search, Ajouts récents, a
  playlist or the current-track page: Back returns there, with the same filter, search and
  scroll position. Chains unwind step by step (album → artist → album, Back, Back), and Forward
  re-applies what you left.
- **Shortcuts**: Cmd+[ / Cmd+] (Ctrl elsewhere), Alt+← / Alt+→, and the side buttons of your
  mouse.
- **Escape** closes what is open first (menu, album editor, queue panel…), then goes back.
- **The sidebar always shows where you are**, and Pistolame reopens on your last library section.
- **Add music from the Albums page** with the new + button. The version now shows, small, in the
  sidebar corner.
- The unfinished Authenticité and Spectrogrammes entries leave the sidebar; the analysis stays on
  the current-track page.

Known limits: no gapless playback yet; Linux has no audio backend. Windows builds are unsigned.

# Changelog

## 0.4.0

### Features
- navigation history: Back returns to the page you came from, with its filter, search and scroll; Forward re-applies it
- back and forward with Cmd/Ctrl+[ ], Alt+arrows and the mouse side buttons
- Escape closes the topmost panel first, then goes back
- the sidebar always highlights where you are; reopen on the last library section
- add music from the Albums page (+), version shown in the sidebar corner

### Changes
- the unfinished Authenticité and Spectrogrammes entries leave the sidebar (the analysis stays on the current-track page)

## 0.3.1

### Fixes
- updates no longer give up after five minutes on a slow connection: a download stops only when it stalls, resumes where it left off, and shows its progress
- no console window flashes on Windows while downloading updates or artist photos

## 0.3.0

### Features
- authenticity analysis ported from Nyquist: transcode verdict with evidence, padded bit depth, limited bandwidth, dynamics, loudness and spectrogram
- redesigned current-track page: file information, then signal path, authenticity and spectrogram
- several artists per album, with track artists suggested
- artist photos from Deezer, cached in the library (can be turned off)

## 0.2.0

### Features
- sort the album grid, show years and disc headings
- settings page with library folders, rescan and reveal in file manager
- export playlists as M3U
- search tracks as well as albums
- artists view, tracks view and recently added page
- resume the last session, paused at its position
- media keys and system now-playing controls
- keyboard shortcuts
- favorite tracks, go to album and go to artist
- watch library folders; drop files and folders on the window

### Fixes
- apply the shared-mode volume on Windows (618d4c8)
- clear a packaging folder left by a cached build

## 0.1.1

### Features
- add the app icon on every platform
- add a per-user Windows installer


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
