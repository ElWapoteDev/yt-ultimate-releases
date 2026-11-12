# DJ

The AI DJ: what it measures about the songs it mixes, where that work runs, how it plans a beat-matched mix, and how
the engine plays it on the exact frame. The design, the research behind it and the stage plan are in `docs/AI_DJ.md`
(Spanish); the queue, the classic crossfade and when transitions happen are in `docs/systems/Playback.md`.

## Files

- `crates/yt-core/src/track_analysis.rs`: `TrackAnalysis` and its bounds.
- `crates/yt-core/src/dj_analysis.rs`, `crates/yt-core/src/migrations/0006_dj_analysis.sql`: the analysis cache.
- `crates/yt-core/src/source_rights.rs`: `MediaOrigin` and `SourceRights`.
- `crates/yt-mir/`: the analysis engine and the vocal measurement (its README owns the pipeline; `models/README.md`
  the models).
- `crates/yt-worker/`: the `ytu-worker` process, its protocol, client and governor (its README owns them).
- `crates/yt-media/src/streaming.rs`: `StreamingHandle::complete_file`, the downloaded stream the DJ analyzes.
- `crates/yt-app/src/dj_analysis.rs`: the analysis controller.
- `crates/yt-app/src/dj_analysis_ui.rs`, `crates/yt-app/src/dj_analysis_ui.css`: the developer view.
- `crates/yt-app/src/bundled.rs`: finds the worker, ONNX Runtime and the beat and vocal models beside the
  executable (Apple Silicon packages include the worker, ONNX Runtime and the beat model; Windows and Intel ones do
  not yet: `docs/RUST_PACKAGING.md`, «Modo DJ en los paquetes»).
- `crates/yt-mix/`: the planner, automation lanes, per-deck DSP and evaluation (its README owns them).
- `crates/yt-media/src/mix.rs`, `crates/yt-media/src/tempo.rs`, `crates/yt-media/src/crossfade.rs`
  (`Transition::Mix`, `Mix`), `crates/yt-media/src/playback.rs` (`place`, `install`, gates and speed ramps): the
  engine's side of a mix.
- `crates/yt-app/src/dj.rs`: `planned_exit`, `planned_mix_for`, `prepared_stream_file`, the mix trigger in
  `dj_tick`, the next mix the player shows (`publish_next_mix`, `NextMix`, `MixName`, `mix_name`), the scene of the
  blend being heard (`BlendScene`, published by `transition_for`), and the Spanish labels of the DJ settings.
- `crates/yt-app/src/dj_ui.rs`, `crates/yt-app/src/dj_ui.css`: what the player shows of the DJ (`NextMixChip`,
  `MixZone`, `BlendBadge`, `BlendSweep`, the «IA» tag) and the blend's show (`BlendBeam`, `BlendHalo`,
  `LeavingCover`, `BlendStage`, `BlendRings`, `BlendBanner`).
- `crates/yt-app/src/player.rs`: where the player bar and the seek bar place them.
- `crates/yt-app/src/now_playing.rs`, `crates/yt-app/src/now_playing_ui.css`: where the full-screen view places the
  blend's show (the view itself belongs to `docs/systems/Playback.md`).
- `crates/yt-app/src/dj_audio.rs`, `crates/yt-media/src/deck_stems.rs`: the vocal stems of a mix (moved to the
  incoming key when the plan says so) and the decks' stem stage, and the rendering of an AI fusion.
- `crates/yt-worker/src/fusion.rs`, `crates/yt-mix/src/fusion.rs`: the AI fusion's canvas and model run, and its
  plan (their READMEs own them).
- `crates/yt-core/src/settings.rs`: `DjStyle`, `MixLength`, `PlaybackPreferences::DJ_INTENSITY`, `dj_show_mix`.
- `crates/yt-app/src/settings_ui.rs`: the DJ settings (`StyleChoice`, `LengthChoice`, `SliderRow`, `SwitchRow`).

## Purpose

Every planned transition (beat-matched or a stem mashup) starts from the same facts about both songs:
where their beats and bars fall, their meter, tempo, key, loudness, the energy of every bar and the sections of
their structure. The DJ measures them once per song, in the
background, on the GPU, without heating the device or touching playback, and keeps them in SQLite.

## Lifecycle

1. While DJ mode is on and something plays, the shared 250 ms poll (`dj_tick`) calls `request_dj_analyses`. It wants
   two songs: the playing one and `upcoming_track` (what `next(true)` would play, without making a shuffled pick).
   Requests for any other song are cancelled; turning DJ mode off cancels all of them.
2. A library song is read from its file. An online song is read from its own playback's download once
   `complete_file` reports it whole, so it is never downloaded twice; an online next song waits until it plays.
   Videos are never analyzed.
3. A blocking task derives the content key (`file:<size>:<modified ns>` or `stream:<bytes>`) and looks in the cache
   (`track_analysis`, per analyzer version). A hit is ready at once.
4. On a miss, `WorkerClient::analyze` runs the job in `ytu-worker` (started on demand, exits after three idle
   minutes). The validated result is stored with `store_track_analysis`.
5. Once neither wanted song is being analyzed, their vocals are measured (`measure_vocals`, HTDemucs on the GPU),
   the playing song first, one at a time; a new analysis stops a running measurement, which starts again after it.
   The levels are added to the analysis and stored under the same key; a measurement that fails is not retried this
   session. Installations without the vocal model (public packages until its license review; local builds may include it
   with `YTU_PACKAGE_VOCAL_MODEL`) skip this step.
6. `Deferred` (power or heat) and a restarting worker are asked again after a minute; `Unavailable` (an installation
   without the worker files, or a device that cannot run them) is logged once and stays for the session; other
   failures stay for that song.
7. The playing song's state is published in `DjAnalysis::shown` for the developer view, and the analyses feed the
   mix (below).

## Mixes (the professional and mashup styles)

1. With DJ mode on, a style that plans mixes (`DjStyle::plans_mixes`) and 1× speed, `dj_tick` picks the next song early (`peek_next`, so
   shuffle reserves it) and both songs are analyzed. An online next song starts downloading 90 s before the end of
   the playing one and is analyzed from its prepared stream (`prepared_stream_file`).
2. Once both analyses are ready, `planned_exit` calls `yt_mix::plan` with the pair's combined `SourceRights`, the
   user's length and intensity, an earliest exit 5 s ahead, the previous mix's technique and a new random seed
   (`RandomState`), so the same pair mixes differently each time it comes up. The planner scores every phrase line
   near the end of the playing song and draws the cue among the best, and lines the songs up on straight grids
   fitted to their beats, which the analyzer reports only to the nearest 20 ms (`crates/yt-mix/README.md`). The plan, or the
   fact that none suits the pair, is kept per pair and settings; a position past the planned exit plans once more
   from there, with a new seed. Vocals that arrive after the plan plan it again (`MixKey::vocals`), so the songs
   stop singing over each other.
3. One second before the exit, `dj_tick` calls `next(true)`; `transition_for` returns `Transition::Mix`, and the
   next song is installed through the usual local or online path. The engine places it (`place`): it waits silent at
   its entry and starts when the playing song's frame clock reaches the exit frame, then both decks follow the
   plan's lanes; the outgoing deck is released when the mix ends and a stretched song glides back to 1× over 4 s
   per percent of speed it was changed by (between 8 s and 40 s, which a mashup at its limit takes), too slowly to
   notice.
4. Anything not ready in time (no analysis, no plan, a stream still downloading, a late arm, another sample rate)
   falls back to the classic crossfade at the end of the song; the professional settings never leave silence.
   Blends never lean on silence: with the playing song's analysis, `dj_tick` counts the time left to where its music
   ends (`TrackAnalysis::audible_end`, before any trailing silence and most of a fade-out), not to the end of the
   file, and an automatic crossfade starts the next song where its sound starts (`audible_start`,
   `Crossfade::starting_at`); a mix starts it on a downbeat, which never lies in silence.
5. When both songs' audio files can be read (a library file or a complete online download, the files the analysis
   read) and the installation has the vocal model, the plan may play the songs' vocals apart
   (`MixRequest::separation`): held apart when they would clash, or the outgoing vocals riding over the incoming
   instrumental. The mashup style («Mashup con IA (experimental)») prefers a `Technique::Mashup` wherever both
   songs' vocals were measured and their tempos meet within 10 %: the incoming instrumental comes in under the
   playing song with its own vocals muted (so the incoming song may sing from its first bar), the instrumentals
   blend, the outgoing vocals ride over the incoming instrumental and the incoming vocals take over on a phrase
   line, with no loops or repeats. Where the keys clash but a semitone or two fits them (`yt_mix::vocal_shift`), the
   outgoing vocals ride moved to the incoming key (`MixPlan::vocal_shift`): after separating the stems,
   `separate_both` has `Stem::moved` transpose the outgoing one from the switch on, on a blocking thread, and the
   stem stage plays the moved vocals from that frame (`crates/yt-mix/README.md`, `crates/yt-media/README.md`).
   Between library songs, when the installation has the fusion
   model (`sa3-generate` and Stable Audio 3 Medium, `docs/RUST_PACKAGING.md`), the same style may instead plan a
   `Technique::Fusion` (with «Fusión IA en cada cambio», `dj_always_fuse`, first for every pair whatever its tempos
   and keys): both songs blend beat-matched over 4, 8 or 16 bars and `ytu-worker` has the generative model
   render that blend again as one piece (`render_fusion`, into a new file under `dj/fusions`, read into memory,
   removed, and added with `Mix::with_rendering`); the engine crossfades into the rendering over the beat before the
   exit and out of it into the incoming song over the beat after it. With this style the next song starts preparing
   180 s before the end instead of 90 s, a fusion being rendered goes before vocal measurements (they give way and
   start again afterwards), and vocals that arrive do not replan a fusion. Library and YouTube songs fuse alike (YouTube by
   the owner's choice of 12 November 2026, §7.3); the rendering is never kept. Such a plan waits (`planned_exit` returns `None`) while `separate_stems` has `ytu-worker` separate
   both windows (`MixPlan::stem_frames`, at most 120 s each) from those files into new files under `dj/stems` in the profile, reads
   them into memory, removes them and adds them to the mix (`Mix::with_stems`). Stems that fail, or are not ready
   half a second before the arm, mark the pair: it is planned again without them for the rest of the playback. The
   same holds for a fusion's rendering.
6. Before the mix, after `dj_tick`, the poll publishes the planned mix (`publish_next_mix` into
   `AppState::next_mix`, only when it changed): a chip beside the song's title names its technique and the second
   where it starts («Mashup · 3:12», «Preparando Mashup · 3:12» while its stems are separated), and with «Mostrar la
   mezcla en la barra» (`dj_show_mix`, on by default) a striped zone on the progress bar covers the seconds both
   songs will sound together, with a tick where the mix starts. An «IA» tag marks the mixes the vocal model shaped
   (its vocals measured either song, or the mix plays them apart).
7. While two songs sound together, the poll copies the engine's `Blend` into `AppState::blend`: the player's badge
   names the planned mix that is playing (`AppState::mixing_as`, set when it is armed: «Mashup», «Doble drop», …,
   with its «IA» tag), «Mezclando» for another mix and «Fundiendo» for a crossfade, with a level meter and a small
   crossfader whose knob follows the blend's progress, and a light sweeps along the progress bar, whose head glows.
   The chip and the zone read published signals only; nothing is computed per frame.
8. The blend's show. `transition_for` publishes the blend's `BlendScene` (`AppState::blend_scene`: the song that
   leaves, how long both songs sound together, and the seconds per beat of beat-matched songs, from the outgoing
   song's tempo), cleared when the blend ends. From the moment the songs sound together (`Blend::progress` above 0),
   the player bar shows a glow and a beam along its top edge that a spark crosses every four beats (`BlendBeam`), a
   halo around the cover whose light turns once every four beats while a ring leaves it on each beat (`BlendHalo`),
   and the leaving song's cover sliding away behind the playing one (`LeavingCover`); the full-screen view shows rays
   that turn behind everything once every eight bars and a glow on the screen's edges that flashes on the beat
   (`BlendStage`), rings that leave the cover on every beat and a wider one on every fourth (`BlendRings`), the
   leaving cover sliding off the playing one, and a banner with the blend's name and a crossfader that travels from
   the leaving song's title to the playing one's (`BlendBanner`). Each group of the show (`Show`) sets
   `--dj-energy`, which swells into the middle of the blend (`energy`), and between beat-matched songs `--dj-beat`
   and `--dj-phase`: the beat's length and, as a negative delay, how far into a beat the blend was when the group
   mounted (`beat_timing`, taken once with `use_hook` so the pulses keep their place), because a mix starts on a
   downbeat. Songs that only crossfade, or fade without matching tempos, share no beat: their show breathes every
   2.4 s instead. Only `transform` and `opacity` animate; pausing pauses the show, and reduced motion leaves still
   lights without rings, sparks or rays.

## Settings

Ajustes → Reproducción, with DJ mode on: «Clásico», «Profesional» or «Mashup con IA (experimental)»; for every
style but the classic one the mix length («Corta», «Media», «Larga»: up to 4, 8 or 16 bars), «Intensidad» (Suave,
Tranquila, Equilibrada, Animada, Enérgica) and «Mostrar la mezcla en la barra». Settings saved with the retired
«Híbrido» or «Generativo» styles read as the mashup one; their creativity level is ignored. Mashups work between
library and online songs. The crossfade length and curve stay: they are the classic style and the professional
fallback. When the fusion model is installed and the mashup style is chosen, the settings add the switch «Fusión IA
en cada cambio» (off by default) and say «Las fusiones usan
Stable Audio 3 en tu equipo · Powered by Stability AI», as its license asks (`dj::FUSION_ATTRIBUTION`). Values
from the WebView are parsed and checked against `PlaybackPreferences`' bounds in Rust (`SliderRow`) and validated
again by `yt-core`.

## Developer view

Settings, with developer mode on, shows «Análisis del DJ»: tempo, stability, meter, key, integrated loudness, true
peak, beat, bar and section counts, whether the vocals were measured, where it was computed (GPU, CPU or the cache),
and strips with the low, mid and high energy of every bar and, once measured, the vocal level over the song (at most
96 columns; longer rows show the loudest value of each group). When nothing can be shown
it says why: DJ mode off, nothing playing, a video, an online song still downloading, in progress, paused by the
governor, unavailable, or failed with its technical reason.

## Invariants

- At most two analyses are wanted at a time and the worker runs one job at a time.
- Nothing is analyzed while paused, with DJ mode off, or for a song that is neither playing nor next.
- In-memory state is bounded to 32 songs; the cache is bounded by `DJ_ANALYSIS_LIMIT`.
- A result for a song that stopped being wanted is discarded unless it already finished, in which case it is still
  stored: analyses are reusable by content.

## Trust boundary

- The worker is a separate, untrusted process: every reply is validated (`TrackAnalysis::validate`, analyzer and
  protocol versions) before it reaches the cache, and `yt-core` validates again when storing and when reading.
- Only confined paths reach the worker: a library track's own file or a download the application created.
- The worker checks `SourceRights` itself; online audio is never kept as derived audio. Measuring vocals separates
  them in the worker's memory and keeps only their levels, so it counts as analysis. Stems for playback need
  `SourceRights::separate`, which the library and YouTube grant (YouTube by the owner's choice of 12 November 2026,
  `docs/AI_DJ.md` §7.3, only while a transition plays): the worker refuses them for any origin without it, they live
  in memory for one mix, and their temporary files are removed as soon as they are read (and the folder is cleared
  before the next mix).
- The planner gets the pair's rights (`SourceRights::with`) and plans nothing a source forbids. Stems and mashups
  need `separate`, and fusions `generate`, which the library and YouTube grant (YouTube only while a transition
  plays); the worker checks them again before it works. A fusion's rendering lives in memory for one mix; its file is removed
  as soon as it is read.
- A plan is validated by `yt-mix` when it is made and again by `yt-media` (`Mix::new`) before the audio thread sees
  it.

## Personal data

Analyses of online media reveal what was played: «Borrar historial» in Settings → Privacidad deletes their rows
(`ListeningErasure::dj_analyses`) and `forget_dj_analyses` clears the session's state. Library analyses describe the
files and stay, like their audio features. Logs never name a song or a path.

## Performance

- The UI thread only compares two ids per poll tick and spawns at most two tasks per song; file metadata, SQLite and
  the worker run off it.
- The worker runs at low priority under the governor (`crates/yt-worker/README.md`); the application process never
  loads ONNX Runtime. Vocals cost about 3.7 s of GPU per minute of audio, after the analyses the mix needs.
- A fusion runs `sa3-generate` in its own process, on the GPU, under the worker's governor: about 13 s for an 8-bar
  fusion on an M4 Pro, 3.6 GB of memory at its peak, returned when it exits.
- Planning runs on the UI thread once per pair: at most 24 cues per technique are built and evaluated, 45 µs at the
  median and under a millisecond at most on real analyses (`crates/yt-mix/examples/plan_probe.rs`). Moving a
  mashup's vocals transposes at most the stem's window once, on a blocking thread, before the mix is armed; the
  stem stage then reads one more buffer. The blend's show costs nothing per frame in Rust: the poll changes two
  inline values four times a second and the compositor animates a dozen layers by `transform` and `opacity`; its
  largest layer, the full-screen rays, is 110 % of the window's longer side. The audio thread only compares a frame
  counter while a deck waits, and during a mix runs at most five biquads per channel and the reverb, with parameters
  updated every 64 frames; nothing allocates or locks there. At 1× a song no longer passes through SoundTouch, which
  saves its cost on every song.

## Tests

`crates/yt-app/src/dj.rs` (a planned mix replaces the automatic crossfade and publishes its scene with the outgoing
beat, a crossfade's scene has no beat and a cut none at all, a pair without a mix crossfades and is not
planned again, Spanish labels, a fusion's name and «IA» tag), `crates/yt-app/src/dj_ui.rs` (the badge names the blend or the planned mix and moves its knob, the chip says
what mix comes and when, the «IA» tag, the zone stays inside the bar, the show's timing puts its pulses on the beat
only for beat-matched songs that already sound together, its energy swells into the middle, the player bar's and the
full-screen view's shows render with both songs named and nothing before the songs meet),
`crates/yt-app/src/player.rs` (a blend lights the bar and shows the leaving cover),
`crates/yt-app/src/now_playing.rs` (a blend shows the banner, the rings and the leaving cover; nothing without one),
`crates/yt-app/src/settings_ui.rs` (the DJ
settings render from their bounds and hide with the classic style), `crates/yt-media/src/mix.rs` (pass-through, the gate opens on the exact exit frame, a
forced gate, the outgoing lanes, a seek ends the mix), `crates/yt-media/src/tempo.rs` (exact pass-through at 1×,
the stretch keeps beats within 5 ms, transposed audio changes pitch and keeps its place within 10 ms), the ignored
real-device test in `crates/yt-media/src/playback.rs` (`an_armed_mix_…`, muted; and the glide back to the song's
tempo lasting 4 s per percent), `crates/yt-mix` (its README), `crates/yt-app/src/dj_analysis.rs` (content keys, which
audio can be read now, requests following the current and next songs, a new analysis stopping vocal measurements,
status comparison), `crates/yt-app/src/dj.rs` (vocals that arrive later plan the mix again; a mix whose stems cannot
be had is planned again without them), `crates/yt-media/src/deck_stems.rs` (stem bounds, pass-through, gains inside
the window, the mix clock and seeks, moved vocals replacing the song's own from their frame without a click),
`crates/yt-media/src/crossfade.rs` (a mix takes only stems that cover its frames, with the outgoing vocals moved
exactly as it plans, and a fusion only a rendering of its length), `crates/yt-media/src/mix.rs` (a later gate starts its lanes at
their own time), `crates/yt-worker` (the fusion canvas and the real model run, its README), `crates/yt-app/src/dj_analysis_ui.rs` (Spanish labels, energy strips, the rendered
panel), `crates/yt-core/src/dj_analysis.rs` and `source_rights.rs`, `crates/yt-worker/tests/worker.rs`, and the
`complete_file` check in the `yt-media` streaming test.
