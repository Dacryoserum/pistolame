# Pistolame 0.7.0

- **Gapless playback.** Albums play without a gap: live records, concept albums and classical
  movements flow from one track to the next. When two tracks share the same format, the next
  one continues in the same device stream, bit-perfect across the join, and each track is still
  verified on its own. A change of format (for example 44,1 to 96 kHz) reopens the device, with
  the next track already decoded.

# Changelog

## 0.7.0

### Features
- gapless playback: the next track is prepared while the current one ends; when both share the same format (sample rate, bit depth, channels) it continues in the same device stream, with no silence and no reopen, bit-perfect across the join; each track keeps its own verification
- a next track in another format is decoded ahead, so only the device reopen remains between the two
