# vl-render notes

Highlights, the edit list and everything that turns an edit into files: filter graphs (graph.txt, cmd.txt),
subtitles (subs.ass), editor exports (EDL / XML / JSON), the music analysis cache and preview wav, and the
final render. These outputs are deterministic; tests compare them byte for byte with reference files.

## Decisions

- `numfmt` holds the formatting and rounding rules the outputs use: round half to even, truncation, floor
  division, shortest round-trip float text, JSON string escaping (ASCII only), Windows command-line quoting,
  percentile/median, and text-mode writes (`\n` -> `\r\n` on Windows).
- serde_json has the `float_roundtrip` feature turned on. Without it serde_json reads some floats 1 ulp
  off (for example `0.37962962962962965`), so JSON written back out (export_json) would drift.
  Feature unification turns it on for the whole workspace build.
- Resizing uses Pillow-style fixed-point two-pass resampling (`overlays::pil_resize`) with premultiplied
  alpha and bicubic/Lanczos kernels, so the resized kill feed row PNGs are stable pixel for pixel.
- Text is drawn with `ab_glyph`, using Inter-Variable (opsz 14, wght 700) and Anton. Advances are rounded
  to whole pixels and the "lm" anchor follows Pillow's rule (FreeType size metrics rounded outwards).
- Shapes overwrite pixels with no antialiasing. Text blends into all four channels. Pasted portraits are
  alpha-composited.
- `render_project(h, out, progress, proxy, options, cancel)` accepts the Export page options (`resolution`,
  `fps`, `encoder`, `quality`, `keep_work`) plus two for tooling: `work_dir` (use this work folder) and
  `dry_run` (write graph.txt/cmd.txt but don't encode). Setting `cancel` kills ffmpeg within about 100 ms.
  In super smooth, cancel takes effect after the current RIFE chunk (one second of video), or after the
  current frame with the optical-flow blur.
- Super smooth "Faster" (`flow.rs` + `vecblur.rs`) replaces the AI in-between frames with a vector motion
  blur: DIS-style optical flow (patch inverse search on a pyramid, an exhaustive integer search on the
  coarsest level, neighbour propagation, weighted densification, 3x3 median) at a quarter of the gameplay
  resolution, a per-pixel confidence (does the flow land on the same colour; softened, so the blur fades
  in and out instead of switching in blocks) that keeps the crosshair, HUD bits and disocclusions sharp,
  and a tent-weighted gather along each pixel's motion in linear light. Faster measures one flow per frame
  pair and uses it reversed for the other direction; custom "High detail" measures both at half resolution
  and samples both neighbouring frames (also used whenever the output frame rate is above the source's).
  Measured on a Ryzen 5 5600X, 1080p60 gameplay, seconds per second of video: Faster 2.4, flow High detail
  8, Fast (RIFE x4, RTX GPU) 10.5, HD subtle (RIFE x8) 20. `examples/smooth_bench.rs` times the presets
  on any part of a video. Thread work goes through rayon's pool: per-call thread spawning cost more than
  the flow itself.
- Overlay layers (score bar, health and ammo, minimap, webcam) store their crop in `LayerSpec.src`
  (fractions of the game area); the Layout page edits it and the box height follows the crop's shape.
  The webcam is cropped from the whole frame (a facecam can sit in a recording's borders, outside the
  game area): `compose_lines` splits the input before the game-area crop only when a webcam is visible, so
  other graphs are unchanged. Layouts saved before the webcam existed get it, hidden, from
  `complete_layout` (also inside `effective_layout`).
- `music::analyze_cached_in(path, dir)` lets tests use their own cache folder. The default is
  data/cache/music.
- `highlights::quick_cut` returns `Result`: an unknown cut name is an error.

## Smart cut (`smart.rs`)

The "Smart cut" quick cut = the kill highlights ("Best moments" scoring) plus what the clip sounds like
(`Events.audio`, from the sound pass in `vl_engine::audio`) and funny / hype phrases in the voice-chat
transcript. Reactions are runs of frames with laughter, shouting / cheering, or a loud *voice* (loudness
above the surrounding minute by at least the clip's 99th percentile, with speech, for 2+ frames — streamers
talk constantly and every gunfight is loud, so a fixed bar fired every few seconds). A reaction during or
up to 5 s after a kill highlight adds to its score and stretches it to keep the reaction; strong reactions
away from kills are moments of their own. The best moments up to the "Best moments" length are kept in clip
order, each with its reasons. `examples/smart_preview.rs <project_dir>` prints what it would keep (read-only).
Without the sound pass it is the kill highlights.

## Dev tools

`cargo build --release -p vl-render --examples`, then
`vlr render <project_dir> <out.mp4> [--proxy] [--options '{json}'] [--cancel-after s]`, `vlr analyze <song> [cache_dir]`,
`vlr preview-graph <project>`, `vlr preview-wav <project> <dir>`, `vlr export <project> <dir>`, `vlr overlays <dir>`.
`smooth_bench` and `smart_preview` are described above.
