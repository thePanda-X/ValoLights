# Changelog

All notable changes to ValoLights are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.2.0] - 2026-11-18

First public release.

### Highlights

- Finds your kills in Valorant recordings and replay extracts by reading the kill feed (by name, so spectated
  teammates' kills aren't counted as yours), plus rounds, spike plants and round results.
- Quick cuts: kills only, first to last kill, best moments, whole rounds, kills with plants and round ends, and
  **Smart cut**, which also listens for laughter, shouting and hype in voice chat.
- Trim timeline with film strip, per-kill toggles, split / delete / add parts and undo for everything.
- Layout editor for 16:9, 9:16, 1:1 and 4:5 with live HUD crops (minimap, health and ammo, score bar), your
  webcam, the kill feed and subtitles; adjustable crops.
- Voice-chat subtitles transcribed on your PC (whisper.cpp, GPU or CPU), caption editor, censoring with bleeps,
  styles and "words at a time".
- Music: background or placed by hand, or synced so the drop lands on your first kill and cuts land on beats.
- Super smooth motion: optical-flow motion blur (fast, any PC) or AI frame interpolation (RIFE).
- Export to MP4 (NVENC when available) and to an EDL for DaVinci Resolve, an XML (FCP 7) for Premiere Pro, or JSON.
- Windows installer with automatic updates.
