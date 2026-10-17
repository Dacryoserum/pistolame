# Pistolame 0.6.5

- **Lighter.** Covers load only when they appear on screen, at the size the screen draws
  them: about half the memory of 0.6.0 and a start twice as light.
- **Faster library.** Importing reads several files at once; a first import is about ten
  times faster, and checking an unchanged library is near instant.
- **Smarter folder watching.** Adding, removing or renaming music updates only what changed,
  not the whole library.

# Changelog

## 0.6.5

### Performance
- covers are loaded only when they come on screen, at the screen's size, and kept on disk: memory at start-up about halved again (428 -> 224 MB on a 928-track library), start-up CPU halved
- library import several times faster: files read in parallel, written in batches; a first import of 928 tracks takes 0.35 s instead of 3.3 s, a rescan with nothing changed 8 ms instead of 63
- folder watching updates only the files and folders that changed instead of rescanning the whole library
- a folder's cover image is read once per folder instead of once per track
