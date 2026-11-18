# vl-app architecture

The `valolights` binary: GUI (eframe 0.37 + glow, egui), background worker, CLI, self-test, UI check.

```
src/
  main.rs        entry: GUI | --worker job.json | cli ... | --selftest | --ui-check [dir] | --read-only
  app.rs         MainWindow: top bar, navigation, jobs, dialogs, drag & drop, close
  state.rs       AppState: open project, edits, events, transcript, save, undo/redo
  pages/mod.rs   the page contract: Page trait, PageCtx, Shell, Action, JobNotice, STEPS
  pages/*.rs     library (Clips), trim, layout, subtitles, export
  dialogs.rs     Dialog values: confirm, message, fail (+details), who-are-you, settings, first run, vocabulary
  widgets/ui.rs  widget kit (buttons, toggle, slider, segmented, panel, choice list, inputs, figure, crosshair…)
  widgets/player.rs  libmpv player (render API -> our FBO/texture), edl_url, lavfi graph
  widgets/layout_canvas.rs, crop_editor.rs  Layout: place layers on the canvas; choose a layer's crop
  widgets/mpv.rs     libmpv-2.dll binding (libloading)
  theme.rs       colour tokens, fonts, egui Visuals, spacing S1..S8
  icons.rs       lucide SVGs recoloured and rasterised with resvg, cached textures
  jobs.rs        Job: worker subprocess, JSON lines, progress/ETA, cancel = kill the process tree
  worker.rs      --worker: analyze / render / proxy / transcribe / smart (Smart cut sound pass)
  speech.rs      how the speech helper (vl-speech.exe) is started
  backend.rs     the ONLY place that calls vl-engine / vl-render (thin adapters)
  update.rs      update check / signature / download / install (UI-free); update_ui.rs: dialog + top bar
  cli.rs, selftest.rs, uicheck.rs, sys.rs
```

## Frame loop

`MainWindow::frame_ui` runs once per frame:

1. poll analysis jobs and the clip job (transcribe / proxy / render) → library progress, `Page::on_job`, dialogs
2. drain `AppState` events → toast + `Page::on_state` for **every** page
3. shortcuts (Ctrl+Z / Ctrl+Y / Ctrl+Shift+Z / Ctrl+PgUp / Ctrl+PgDown / Ctrl+1..5), file drag & drop
4. top bar, then the visible page's `ui()`, then the newest dialog (modal)
5. run the `Action`s pages and dialogs pushed; drain state events again; `st.tick()` (debounced save,
   undo grouping) and `request_repaint_after` for the next deadline

Nothing repaints on a timer: egui repaints on input, the player's mpv callbacks, job output (the job
reader thread calls `ctx.request_repaint()`), and the deadlines above.

## The page contract (`pages/mod.rs`)

```rust
pub trait Page {
    fn on_state(&mut self, ev: &StateEvent, st: &mut AppState, shell: &mut Shell) {}  // state changes
    fn on_job(&mut self, n: &JobNotice, st: &mut AppState, shell: &mut Shell) {}       // set_progress + result
    fn on_show(&mut self, st: &mut AppState, shell: &mut Shell) {}
    fn on_hide(&mut self, st: &mut AppState, shell: &mut Shell) {}
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut PageCtx);                              // draw, every frame
    fn stop_players(&mut self) {}   // the clip is closed / removed
    fn shutdown(&mut self) {}       // window closing (GL current): free players
}
pub struct PageCtx<'a> { pub st: &'a mut AppState, pub shell: &'a mut Shell, pub frame: &'a mut eframe::Frame }
```

* A page owns its widget state and its own `Player`. Construct it with `XPage::new(&shell)` (the shell
  carries the GL handle players need). Pages are fields of `MainWindow` (`trim`, `layout`, `subs`, `export`).
* A page **never** calls the main window. It pushes an `Action` (`cx.shell.push(..)`): `WhoAreYou` (Trim's Change), `Transcribe { whole, only }` (Subtitles), `Preview` (Layout),
  `Render { out, options }` (Export), `CancelJob`, `Go(step)`, `Toast(msg)`, `Dialog(..)`, `Settings`.
* Job feedback comes back through `on_job` (`JobNotice { kind, progress: Some(p) | None, msg, result }`),
  progress and the result: transcribe → Subtitles,
  proxy → Layout (`result = {"out": ".../preview.mp4"}` → load it in the page's player), render → Export.
  `cx.shell.job` tells whether a job runs right now (`shell.job_running("render")`).
* Refresh the page's widgets from `st` in `on_state` (ProjectChanged / EditsChanged / EventsChanged /
  TranscriptChanged / Restored). Widgets bound directly to `st.ed` fields are fine too – egui redraws
  from state every frame, so most pages need very little in `on_state`.

Example (a page body that edits the kill feed rows):

```rust
fn ui(&mut self, ui: &mut egui::Ui, cx: &mut PageCtx) {
    side_panel(ui, 320.0, |p| {
        p.section(Some("Kill feed"), |ui| {
            let mut rows = cx.st.ed.killfeed.rows as f64;
            if ui.add(Slider::new(&mut rows, 1.0, 6.0).step(1.0)).changed() {
                cx.st.ed.killfeed.rows = rows.round() as i32;
                cx.st.touch();                      // save in 400 ms + undo step when changes pause
            }
            if ui.add(Button::primary("Render preview").icon("film")).clicked() {
                cx.shell.push(Action::Preview);
            }
        });
    });
}
```

## State, saving, undo (`state.rs`)

`AppState` holds `h` (ProjectHandle), `ev` (Events), `ed` (Edits), `tr` (Transcript). Mutate in place,
then tell the state what happened:

| you changed | call | effect |
|---|---|---|
| `st.ed.*` | `st.touch()` | `EditsChanged`, save in 400 ms, undo step once changes pause 600 ms |
| `st.ev` (kills) | `st.save_events()` | writes events.json, `EventsChanged`, grouped undo step |
| `st.tr` | `st.save_transcript()` | writes transcript.json, `TranscriptChanged`, grouped undo step |
| parts | `st.set_parts(parts, record)` | sorted, saved; `record` = its own undo step |
| quick cut | `st.apply_quick_cut(name, record)` | parts from `backend::quick_cut` |
| "me" | `st.set_me(ids)` | keeps a hand-trimmed cut (`cut_is_custom`), else rebuilds the quick cut |

Discrete actions (add, delete, split…) are wrapped: `st.begin_step(); …mutate…; st.touch(); st.end_step();`
so they become one step immediately. Continuous edits (slider drags, typing) just `touch()` and are grouped
(600 ms). Labels are derived from what changed ("Undo caption edit", "Undo trim change",
"Undo beat sync"…); `undo()`/`redo()` emit `Restored("Undid …")`, shown in the top bar. History limit 200.
`st.say(msg)` → top-bar toast. `st.kept_duration()` includes transition overlaps. `st.read_only` (QA
mode) turns every write into a no-op.

## Jobs (`jobs.rs`, `worker.rs`, `speech.rs`)

`Job::start(kind, spec, waker)` writes `data/cache/tmp/job_xxxx.json`, starts this exe with
`--worker <spec>` (no console window), reads JSON lines on a thread; `poll()` returns
`Progress / Finished(result) / Failed(msg | "Cancelled")`; `cancel()` runs `taskkill /PID <pid> /T /F`.
Analyses are queued and run one at a time, one transcribe/render/proxy job at a
time for the open clip. The worker's `transcribe` kind starts the speech helper `vl-speech.exe` (crate
vl-speech, whisper.cpp) next to this exe with the same spec (`speech::transcribe_command`; `VALOLIGHTS_ROOT`
always set) and relays its lines; if a GPU run dies without an answer it runs once more with `--cpu`.

## Player (`widgets/player.rs`)

```rust
let mut player = Player::new(shell.gl.as_ref(), "layout");
player.load(&edl_url(&src, &[(a, b), (c, d)]), Load { start: 0.0, pause: true, compose: Some(graph), ..Default::default() });
player.show(ui, cx.frame);            // video + controls bar, fills the available rect
for e in player.take_events() { if let PlayerEvent::Position(t) = e { self.playhead = t } }
player.seek(t); player.toggle(); player.set_subs(Some(ass_path)); player.snapshot(png_path);
```

API: `load(url, Load { start, subs, pause, compose, has_audio, music, game_volume, duck })`, `seek`,
`toggle`, `play`, `pause`, `stop`, `pos`, `duration`, `set_subs`, `volume`. mpv runs with `vo=libmpv`,
`hwdec=auto-safe`, `keep-open`, no OSC, **no Lua scripts** (all `load-*`/`ytdl`/`config` = no); file
lists via `change-list`. mpv's update callback wakes egui; a frame is rendered into the player's own
FBO only when mpv has a new one. Call `show()` every frame the player is visible; `shutdown()` on exit.

## Theme and widget kit

Colours: `theme::c::{DEEP, BG, CARD, RAISED, RAISED_HI, HAIR, HAIR_STRONG, TEXT, SECONDARY, MUTED,
DISABLED, ACCENT, ACCENT_HOVER, ON_ACCENT, ACCENT_SOFT, WIN, LOSS}`. Spacing
`S1..S8` (4 px grid). Text roles `Role::{Title, Section, Secondary, Muted, Mono, Figure, Timecode, Pill,
Brand, Body}` → `ui::label(ui, text, Role::Muted)`. Fonts: Inter (variable; families for 400/500/600
via `theme::ui_font(size, Weight::Medium)`), mono = Cascadia Mono / Consolas (`theme::mono_font`).

| widget | call |
|---|---|
| buttons | `ui.add(Button::primary("Export").icon("download"))`, `Button::new(..)`, `Button::ghost(..)`, `.icon_right(..)`, `.enabled(b)`, `.tooltip(..)` |
| icon button | `ui.add(IconButton::new("scissors", "Split  (S)").checked(on))` |
| toggle (every checkbox) | `ui.add(Toggle::new(&mut on).text("Bleep the audio")).changed()` |
| slider (wheel only when focused) | `ui.add(Slider::new(&mut v, 0.0, 1.0).step(0.05))` |
| segmented | `segmented(ui, "fmt", &[("16:9".to_string(), "16:9"), …], &mut fmt, 0).changed()` |
| side panel | `side_panel(ui, 320.0, |p| { p.section(Some("Title"), |ui| { … }); })` |
| choice list | `choice_list(ui, "cuts", &rows, Some(&current), 36.0) -> Option<K>` |
| inputs | `text_input`, `text_area`, `spin(ui, &mut v, lo, hi, decimals, " s", w)`, `combo(ui, id, &mut k, &opts, w)`, `swatch` |
| misc | `figure`, `headshot_mark` / `paint_crosshair`, `pill`, `progress_bar`, `hline`, `section`, `hint`, `elided`, `paint_switch` |
| icons | `icons::show(ui, "music", c::MUTED, 16.0)`, `icons::paint_sized(painter, rect, name, color, size)` |

## Dialogs

`cx.shell.dialog(Dialog::confirm(title, text, "Remove", default_yes, vec![Action::…]))` – the actions run
when the user confirms (a dialog is data, not a blocking call). Also `Dialog::message`, `Dialog::fail(title,
text, detail)` (details behind "Show details"), `Dialog::who_are_you(&st)`, `Dialog::vocabulary()`.

## Checks

* `cargo test -p vl-app` – state/undo, graphs, EDL, formats.
* `valolights --ui-check [dir]` – drives the real window with injected input, saves the app's own frames
  (egui viewport screenshots) to PNG and writes `scratch/vl-app/uicheck_report.txt`. Every project
  except a `data/projects/rustqa_*` copy is read-only; user data is hashed before/after.
  Run it with `VALOLIGHTS_ROOT` pointing at an empty root to check the empty Clips page.
