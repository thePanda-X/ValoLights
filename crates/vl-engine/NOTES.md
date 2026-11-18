# vl-engine notes

Analysis of a recording: `imgproc` (the image operations the detectors need), `ocr`, `layout`, `names`,
`detectors::{killfeed, topbar, hud}`, `tracker`, `identity`, `rounds`, `analyze`, `audio`, `evaluate`.
Binaries: `analyze <video> <out_dir> [--alias NAME]...`, `evaluate [--labels DIR] [--only NAME]`.
Dev examples (`examples/`): `ocr_line`, `kf_dump`, `resize_check`.

## Exact image operations

The detector thresholds were tuned on OpenCV 5.0 output, so the pixel operations reproduce it bit for bit
(checked against reference outputs; the evaluation fixtures give identical events):

- `BGR2GRAY`: 15-bit fixed point `(3735 B + 19235 G + 9798 R + 2^14) >> 15` (OpenCV 4.x uses other constants).
  Verified for all 2^24 colours. HSV (8-bit, H 0..180) also verified for all 2^24 colours.
- `resize` 8-bit linear and INTER_AREA *upscaling*: fixed-point horizontal pass, then OpenCV's vectorised vertical
  step `((S0>>4)*b0>>16) + ((S1>>4)*b1>>16) + 2 >> 2` for every width; row weights are not reset at the borders
  (only column weights are). Bit-exact on random tests.
- `resize` 8-bit cubic: float pipeline with weights evaluated from a double-precision position (no 2048
  quantisation). Matches OpenCV to the pixel in ~95% of random cases; the rest differ in single pixels at
  rounding ties. OCR inputs (crop scaled to height 48) were bit-exact in every case checked.
- INTER_AREA downscale: OpenCV's area table in float; bit-exact.
- Float32 means use pairwise summation where they feed a comparison (row opacity ranking).
- OCR confidences are rounded to 5 decimals from the exact binary value.

## OCR

- ONNX Runtime 1.31 (ort's static build) with RapidOCR's PP-OCRv6 small models. Recognition runs on a pool of up
  to 6 single-threaded sessions so the per-frame workers and the name OCR can run in parallel; the thread count
  doesn't change results.
- `read_block` (the DB text detector, used for round-end banners) returns axis-aligned boxes of connected
  components rather than rotated rectangles; fine for horizontal banner text.
- `engine_version` in events.json is the crate version (`vl_core::VERSION`).

## Speed

Frames are decoded on the calling thread and analysed on worker threads; results are consumed in frame order,
so the tracker sees exactly the sequential stream. Name OCR and `refine_time` (one ffmpeg seek per candidate)
run in parallel too.

Frame reads with `hwaccel` keep the decoded frames on the GPU (`-hwaccel_output_format cuda`): `fps` drops the
unsampled frames there and only the kept ones are downloaded (`hwdownload`) for crop / scale / BGR. Same pixels
as downloading every frame (checked), but the 6 fps scan of a 60 fps video no longer copies 10x the frames it
uses. If the GPU can't decode a video (e.g. ProRes) the reader falls back to the plain GPU decode, then the CPU.
Refine and round-banner reads use the GPU too. Full-match 1080p60 AV1 VOD (33 min): 527 s -> 225 s
(scan 338 -> 137 s, refine 123 -> 34 s), same 216 kills.

Debug aid: `VL_DUMP_CANDS=<file.json>` writes the kill candidates (texts, scores, colours) before identity
resolution.

## Sound pass (`audio.rs`)

YAMNet (Google, AudioSet, Apache 2.0; ONNX export with the log-mel frontend inside, `assets/audio/yamnet.onnx`
fetched by `tools/fetch_binaries.ps1`) on 16 kHz mono from ffmpeg, streamed in ~1 minute chunks aligned to
its 0.48 s frames. Kept per frame (0..100): laughter (6 classes), shouting (5), cheering (2), speech (4)
and loudness. About 6 s for a 33 minute VOD on the CPU. Laughter mixed under game sound scores low
(peaks ~0.25-0.35), so Smart cut's thresholds are modest.
