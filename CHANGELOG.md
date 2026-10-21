# Changelog

## 0.7.0

### Features
- gapless playback: the next track is prepared while the current one ends; when both share the same format (sample rate, bit depth, channels) it continues in the same device stream, with no silence and no reopen, bit-perfect across the join; each track keeps its own verification
- a next track in another format is decoded ahead, so only the device reopen remains between the two

## 0.6.7

### Features
- choose an artist's photo: click the photo on their page to search Deezer (their name is searched at once, any other name can be typed), see the artists found with their fan counts and pick one, or import an image file; a photo chosen by hand is never replaced by the automatic lookup

### Fixes
- "Renommer l'artiste": the help text is no longer cut, and "Réunir avec" sits under it instead of over the buttons

## 0.6.6

### Features
- "Retirer de la bibliothèque" in the "…" menu of a track, an album or a selection: the tracks leave the library (the files stay on disk) and are not read again until their folder is added again; "Annuler" brings them back
- rename an artist from their page (pencil next to "Aléatoire"), on all their albums; giving the name of another artist merges the two, and a spelling that differs only by case is offered in one click

### Fixes
- a folder or file added from inside a folder already in the library no longer becomes a separate library folder: once deleted from the disk, its tracks stayed shown and could not be played (and showed as a namesake album with "AAC" or "24/44,1" next to the artist)

## 0.6.5

### Performance
- covers are loaded only when they come on screen, at the screen's size, and kept on disk: memory at start-up about halved again (428 -> 224 MB on a 928-track library), start-up CPU halved
- library import several times faster: files read in parallel, written in batches; a first import of 928 tracks takes 0.35 s instead of 3.3 s, a rescan with nothing changed 8 ms instead of 63
- folder watching updates only the files and folders that changed instead of rescanning the whole library
- a folder's cover image is read once per folder instead of once per track

## 0.6.0

### Features
- the LCD unfolds into the current-track page: click anywhere on it (or Cmd/Ctrl+L) and it opens downward; leaving the page folds it back
- multiple selection in every track list (Cmd/Ctrl+click, Shift+click) with an action bar; double click plays, a single click selects (option "Lire d'un simple clic" in Réglages)
- right click opens a track's "…" menu; artists and albums are links in the Morceaux table, the album header and the current-track page
- queue: undo and redo (Cmd/Ctrl+Z, Cmd/Ctrl+Shift+Z), move tracks up and down, Delete, and a played history with "Lire à partir d'ici"
- "Annuler" after removing from the queue, a playlist or the favorites
- sleep timer in the queue panel: end of the track or 15 to 90 minutes, no fade (bit-perfect to the end)
- artist page: photo, "Tout lire", "Aléatoire", albums from the newest, most played tracks
- "Reprendre" on an album page continues where listening stopped
- keyboard help with ? or F1; the mouse wheel sets the volume
- the window, the Morceaux order and its Favoris filter are remembered
- Linux plays sound through ALSA (experimental): shared mode through PipeWire or PulseAudio, exclusive bit-perfect mode on a chosen card
- macOS: unified title bar, floating messages, consistent page headers

### Performance
- covers are decoded in the background at display size and kept on disk: about half the memory, a start several times lighter, no stall while the library loads
- the authenticity analysis runs only when the current-track page is open, not at every track change
- long playlists build only the rows on screen; the queue panel lists at most 100 tracks per section

## 0.5.0

### Features
- every common audio format: FLAC, ALAC, WAV, AIFF and CAF stay bit-perfect; MP3, AAC (M4A) and Ogg Vorbis play too, marked "Avec perte" and never shown as bit-perfect
- MP3 and Vorbis encoder delay and padding are removed, so tracks start without the encoder's silence
- the codec is shown on the LCD, in the file details and on album pages; the file picker and folder watching accept every supported format

## 0.4.1

### Fixes
- "Sortie du système" (new default): follow the operating system's output device, so headphones, AirPods or a DAC plugged in are used; no more silence when the remembered device is muted by the system
- unplugging the device in use stops playback instead of jumping to the speakers; no device bouncing in exclusive mode
- the interface stops refreshing values that did not change (lower CPU use), and the equalizer animates only while playing

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
