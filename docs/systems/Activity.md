# Activity

The activity system tells the user what the application is doing and what happened. Long work registers a task while
it runs; every outcome becomes a notice with a tone. Notices appear as toasts and stay in a bounded history that the
activity panel shows next to the running tasks.

## Files

- `crates/yt-app/src/activity.rs`: model and controller. `Tone`, `Notice`, `Task`, `Stop`, `Activity` (the signals),
  `TaskHandle`, and the `AppState` methods that every feature calls.
- `crates/yt-app/src/activity_ui.rs`: components. `Toasts` (root), `ActivityButton` (player bar), `ActivityPanel`
  (side panel next to the queue), with `ToastCard`, `TaskRow`, `NoticeRow` and `ProgressRing`.
- `crates/yt-app/src/activity_ui.css`: the feature stylesheet, included once by `Toasts`.

Integration points owned by other systems: `crates/yt-app/src/updates.rs` exposes `activity_task()` and
`cancel_active()`; `crates/yt-app/src/downloads_ui.rs` exposes `active()` and `phase()`, which map a download row to its
running state; `crates/yt-app/src/state.rs` drives `Activity::expire` from the shared 250 ms poll.

## Public API

```rust
state.notify(text);                       // Tone::Info
state.notify_success(text);
state.notify_warning(text);               // invalid input, partial results
state.notify_error(text);                 // failures
state.announce(tone, text, Some(Page::Downloads)); // adds a «Ver» action
let task = state
    .begin_task("scissors", "Separando «Canción»")
    .page(Page::Stems)
    .stop(Stop::Job { key: job_id, label: "Cancelar" });
task.progress(42.0, "Separando voz");      // None-like progress until the first call
drop(task);                                // the task leaves the panel
```

Choosing a tone: `Success` for completed work and confirmed changes, `Info` for neutral confirmations and started work,
`Warning` for rejected input or work that finished with problems, `Error` for failures. Messages are Spanish, name the
item between «» when there is one, and say what happened, not how.

Use `begin_task` for work that can last longer than about a second (imports, scans, exports, processing, cleanup,
video preparation). A playlist batch announces itself once; its items do not toast individually, and a final notice
reports the complete playlist.

`Stop` says what the task's stop button does: `Job { key, label }` cancels the token registered under `key` in
`AppState::cancellations` (downloads pause, processing cancels), `Update` cancels the updater's check or download, and
`Playback` cancels the media being opened (`AppState::cancel_remote`).

Producers today: `state.rs` (`import_paths`, `scan_library`, `clean_temporary_files`, `export_playlist`),
`downloads.rs` (queue, completion, pause, failure, finished playlist), `processing.rs` (stems, chipmunk, quality,
variations), `remote.rs` and `video.rs` (video preparation), `updates.rs` (check, download, install) and every
module that reports an outcome through `notify_*`.

## Lifecycle

- `Activity::use_new` creates the signals with the application's lifetime (`use_app_signal`). A playback restore
  error at startup becomes the first notice.
- `announce` logs the notice (`native::record_log`, prefixed with the tone) and records it. While the panel is closed
  the notice also becomes a toast and raises the unread count; while it is open the notice only joins the history.
- Toasts expire after 4.5 s (info, success), 7 s (warning) or 10 s (error). `AppState::start_polling` calls
  `Activity::expire` on its 250 ms tick, so the feature has no timer of its own. Hovering the stack pauses expiry; leaving
  it grants 2 s.
- `TaskHandle` removes its task on drop, so a task never outlives the work that registered it, whatever path the work
  takes (success, error, cancellation, early return).
- Opening the panel clears the toasts and the unread count. Escape and the queue button close it.

## Invariants

- The history keeps the newest `NOTICE_HISTORY` (60) notices, newest first; at most `VISIBLE_TOASTS` (3) toasts show,
  oldest dropped first.
- A notice identical in tone and text to one still on screen refreshes it (`repeats` grows) instead of stacking.
- Running tasks shown to the user are the union of the registry, active downloads (`downloads_ui::active`) and the
  updater (`updates::activity_task`); downloads and the updater stay their own source of truth.
- `overall_progress` is the mean of the running tasks only when every task reports progress; otherwise the ring spins.
- `TaskHandle` writes the tasks signal only when the visible task changes.

## Trust boundary

Notice and task text is rendered through RSX text nodes and is escaped. Errors from the network or plugins may appear
in notices; signed URLs are never put in them, and `record_log` redacts URLs before writing. Stopping a task only
cancels a token the application registered (`AppState::cancellations`) or the updater's own token.

## Performance

- One shared poll tick (`Activity::expire`) peeks the toast list and writes only when a toast expires.
- Producers throttle progress before it reaches the signal: processing forwards whole-percent or wording changes,
  downloads reuse their existing row updates.
- `ActivityButton` is its own component, so the player bar's 250 ms position updates never re-render it.
- Toast entry animates `transform` and `opacity`; the indeterminate bar animates `transform`. Reduced motion is honored
  by the global rules in `crates/yt-app/assets/app.css`.

## Tests

- `activity::tests`: bounded history and toast stack, repeat folding, tone lifetimes, overall progress, and a
  VirtualDom test that a task ends with its handle and that opening the panel marks notices read.
- `activity_ui::tests`: SSR of an error toast with the alert role and the unread dot, the running task with its
  progress in the button title, and the panel sections after opening it.
