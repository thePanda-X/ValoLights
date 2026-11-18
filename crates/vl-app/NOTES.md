# vl-app notes

Design notes per area. The overall structure is in [ARCHITECTURE.md](ARCHITECTURE.md).

## General

* `backend.rs` is the only place that calls vl-engine / vl-render; pages and dialogs go through it.
* Analyses run one at a time from a queue; transcription, preview and export jobs run one at a time for the
  open clip.
* Message boxes are in-window modals (egui), title shown as the heading; button order is Windows'
  ([Confirm][Cancel], right-aligned); Enter = default button, Esc = cancel.
* Duplicate files in "Add video": each duplicate is asked about separately after the others were added.
* `cli analyze --alias`: the config cache can't be changed in memory, so the aliases are applied after the
  analysis (`finalize_identity(ev, aliases)` + marking matching players `me_hint = "alias"`).
* First-run GPU line: "NVIDIA driver present" (System32\nvcuda.dll). The self-test asks the speech helper itself
  whether it can use CUDA.
* Minimum window size 1100x700.

## Updates (`update.rs`, `update_ui.rs`)

* `update.rs` is the release side and is UI-free: manifest types, the Ed25519 signature check, version
  comparison, download with SHA-256 check, `start_install` (Windows: the Inno Setup installer with
  `/SILENT /RELAUNCH=1 /DIR=<app root>`), plus the release tooling behind `cli release-keygen` and
  `cli release-manifest`.
* `update_ui.rs` runs the check / download on a thread and shares a `Status` with the top bar ("Update to x.y.z")
  and the update dialog. The launch check is skipped in read-only / QA modes and for source checkouts
  (`update::is_dev_build`). See docs/releasing.md.

## QA aids

* `--read-only` / `VALOLIGHTS_READONLY=1`: GUI that never writes projects or settings.
* `--ui-check [page] [dir]`: scripted GUI runs that inject input, save PNGs of the app's own frames (egui
  viewport screenshots, never the desktop), work on `data/projects/rustqa_*` copies and hash the user's data
  before and after. Reports go to `scratch/vl-app*/`.
  * base (`uicheck.rs`): clips list, context menu, open by click, player, undo grouping / labels / Ctrl+Z /
    Ctrl+Y and disk write, settings, who-are-you, fail and vocabulary dialogs, every tab, opening a project
    without edits.json read-only, transcription job progress + cancel + full run.
  * trim, layout, subs, export: the pages below.
* `--shot <project> <page> <out.png> [height] [crop:<layer>]`: one screenshot of a page.
* `--perf`: playback and export timings (read-only).

## Export page (`pages/export.rs`)

* "Replace video" / "Replace file" questions are app dialogs whose confirm button pushes
  `Action::ExportConfirmed(tag)` ("render" or the editor file extension); the main window hands it back to
  `ExportPage::confirmed`.
* `MainWindow::poll_job` passes render notices for another clip to the Export page too (status "Saved" /
  "Cancelled" / "Render failed", no error dialog), so a render ends cleanly even when another clip is open.
* `cancel_job` waits up to 2 s for the killed worker to exit, so stopping an export when opening another clip
  asks only once.
* A partial file the encoder still holds is deleted on a retry thread.
* Check: `valolights --ui-check export` (`src/uicheck_export.rs`): texts, editor files against reference exports,
  real 720p and 1080p renders.

## Trim page (`pages/trim.rs`, `widgets/timeline.rs`, `widgets/music_lane.rs`)

* Timeline and lane are plain structs drawn with the egui painter; what the user did comes out of
  `take_events()` lists (`TlEvent`, `LaneEvent`).
* State events are dispatched after the page's `ui`, so a message after an edit is queued (`say_later`) and shown
  at the start of the next frame, after `edits_changed` ran.
* Shortcuts are read at the start of the page's `ui` (consumed, so a focused slider never also gets the arrows) and
  only when no modal is open and no text field has focus. Kill-list switches never take focus.
* The film strip PNG (ffmpeg tile, 160 cells) is cut into textures of at most 8192 px (the 16:9 strip is ~19000 px
  wide, over the GPU limit). The strip's rounded ends use per-corner rounding on the end cells.
* Read-only mode (QA): previews (ASS, music wav, a missing film strip) go to `data/cache/tmp/trim/<id>` instead of
  the project's cache.
* Player loads pass `has_audio` from the source (a silent source must not get a graph naming an audio track).
* Smart cut: picking it the first time starts the `smart` worker job (sound pass); progress and Cancel show under
  the quick-cut list, and the Highlights list shows each moment's reasons.
* Check: `valolights --ui-check trim` (`src/uicheck_trim.rs`). Works on `rustqa_trim_*` copies, deleted afterwards.

## Subtitles page (`pages/subtitles.rs`, `widgets/caption_track.rs`)

Check: `valolights --ui-check subs` (`src/uicheck_subs.rs`, includes a real transcription + Listen again;
`VL_SUBS_NO_SPEECH=1` skips the speech helper).

* Saves go through `st.save_transcript()` (+ `end_step()` for discrete actions); the page skips the
  `TranscriptChanged` events of its own saves.
* In read-only mode the preview `.ass` / music wav go to `data/cache/tmp` instead of the project's cache.
* The time fields are a TextEdit-based spin (Enter / focus-out applies, Up/Down/wheel 0.05 s); Enter / F2 / Up /
  Down act on the list only while it "has focus" (last clicked, no widget focused); the playhead used for
  [ ] S N is the page's own (set on seek and from player positions); a scrub release seeks once more to the
  release point, and a seek that didn't land is re-sent once after 350 ms (mpv sometimes loses the last of a
  burst of exact seeks); after a (re)load the page re-seeks to refresh the paused first frame.

## Layout page (`pages/layout.rs`, `widgets/layout_canvas.rs`, `widgets/crop_editor.rs`)

Check: `valolights --ui-check layout` (`src/uicheck_layout.rs`, on a `rustqa_layout_*` copy, incl. a real
preview render, Cancel, playback and the crop editor).

* User actions are collected while drawing and applied afterwards; discrete actions are one undo step
  (begin_step / touch / end_step), slider drags group via `touch`.
* Arrow keys are handled page-wide while the page shows; a focused slider keeps them (Shift + arrow = a page step
  of 10), and arrows never move egui's keyboard focus here.
* In read-only mode the still frame / drawn kill rows go to the temp folder instead of the project cache.
* The subtitle sample on the canvas uses Inter SemiBold; images are drawn with mipmapped linear filtering; a
  finished preview is loaded into the player on the page's next frame (paused when the page is hidden).
