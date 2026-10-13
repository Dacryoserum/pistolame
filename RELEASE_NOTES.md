# Pistolame 0.2.0

A complete library, and the player now fits into daily use.

- **Browse your music your way**: an Artists view (round covers, one click for an artist's
  albums), a Tracks view (the whole library in a table sorted by any column), and *Ajouts
  récents* with the albums added and played last.
- **Sorted album grid**: by title, artist then year, year, recently added or recently played.
  Titles sort naturally ("Season 2" before "Season 10"). Years and "Disque 1 / Disque 2"
  headings on album pages.
- **Search** finds tracks as well as albums (accents ignored).
- **Favorites**: "Ajouter aux favoris" from any track menu, a heart on favorite tracks, and a
  *Favoris* filter in the Tracks view. Track menus also offer "Aller à l'album" and "Aller à
  l'artiste", and "Afficher dans le Finder / l'Explorateur".
- **Media keys and Now Playing** on macOS, Windows and Linux (MPRIS).
- **Picks up where you left off**: the app reopens on the last track and queue, paused at the
  same position. It never starts playing on its own.
- **Keyboard shortcuts**, listed on the new *Réglages* page (output, library folders, rescan,
  shortcuts, updates).
- **Drop files or folders** on the window to add them, and library folders are watched: new
  music appears without restarting.
- **Playlists**: export to M3U.
- **Windows**: the shared-mode volume (dB slider, mute) now works.

Known limits: no gapless playback, authenticity analysis or spectrograms yet; Linux has no
audio backend. Windows builds are unsigned: SmartScreen may show "Unknown publisher".

# Changelog

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
