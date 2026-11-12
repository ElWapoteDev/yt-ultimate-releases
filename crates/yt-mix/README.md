# yt-mix

The DJ's transitions as pure functions of both songs' analyses (`yt_core::TrackAnalysis`): where a mix starts, which
technique it uses, what each deck does over its seconds, the per-deck DSP that plays it, and a first evaluation that
rejects mixes whose beats drift apart or whose basslines play together. Songs are lined up on straight beat grids
fitted to their detected beats (`grid.rs`), which the analyzer reports only to the nearest 20 ms. No files, network or
clock; the only
randomness is the request's seed, so the same request always gives the same plan. The design and the research it
follows are in [`docs/AI_DJ.md`](../../docs/AI_DJ.md) (§5.5–5.7).

## API

- `plan(&MixRequest) -> Option<MixPlan>`. The request carries both analyses, `MixLength`, the intensity
  (`PlaybackPreferences::DJ_INTENSITY`), the pair's `SourceRights` (`with`), the earliest exit frame, the previous
  mix's `Technique`, a `seed`, `separation` (stems can be had), `fusing` (a fusion can be rendered) and the
  `DjStyle`. The application passes a new seed for every transition, so the same pair mixes
  differently each time. `None` means the classic crossfade is better: the sources forbid mixing, no cue is left
  after the earliest exit, or no candidate passes the evaluation.
- `MixPlan`: the `Technique`, both sample rates, `exit_frame` (a detected downbeat, unless the song has no beat
  grid) and `entry_frame` (the incoming downbeat, moved by the few milliseconds that make the songs' fitted grids
  meet), `speed` of the incoming song during the overlap, `overlap` in seconds, both decks' `Lanes`, the
  `MixReport`, and `vocal_shift` (`VocalShift { semitones, from }`: a mashup's outgoing vocals sound moved by that
  many semitones from `from` seconds after the exit; `vocal_shift_frame()` gives the outgoing frame). `validate()`
  checks every bound, among them that moved vocals never sound with their own instrumental; `lead()` and `end()`
  give the seconds before and after the exit in which the lanes change (`end` includes a reverb ringing out).
- `Technique`: `Relaxed`, `Rolling`, `DoubleDrop` (beat-matched hand-overs), `FilterSweep` (clashing keys), `Fade`
  (songs that cannot be beat-matched), `Mashup` (a stem mashup) and `Fusion` (an AI fusion), the mashup style's
  techniques. Every technique
  overlaps the songs and none repeats or loops audio: the owner rejected loop rolls, echo repeats and synthesized
  bridges because they sounded like abrupt cuts (2026-11-12).
- `Curve` (piecewise linear over seconds since the exit, at most 16 points; it holds its end values), `Effects`
  (whether a deck carries a reverb) and `Lanes` (gain, three EQ cuts, high-pass and low-pass in octaves above 20 Hz,
  the reverb send, and the gains of its separated vocals and of the rest) with `at(seconds) -> DeckParams` and
  `uses_stems()`; the stem gains are played by the engine's stem stage, not by `DeckProcessor`.
- `DeckProcessor::new(channels, rate, effects)`, `set(&DeckParams)`, `process(&mut frame)`: a three-band DJ EQ (low
  shelf 200 Hz, mid 1 kHz, high shelf 4 kHz, cuts only, down to a −30 dB kill), Butterworth high-pass and low-pass
  from the RBJ cookbook and a Freeverb-style reverb (four combs and two all-passes per side).
- `camelot(key)` and `key_distance(a, b)`: positions and steps on the Camelot wheel (`COMPATIBLE_KEYS` = 1).
  `transposed(key, semitones)` moves a key, and `vocal_shift(vocals, instrumental)` gives the fewest semitones
  (within `MAX_VOCAL_SHIFT`, 2; the closest key first, then downwards) that make a song's vocals fit another song's
  key: 0 when they fit already, `None` when no such move does.
- `MixReport { beat_error_ms, bass_overlap_seconds, loudness_step_lu }` and `passes(beat_seconds)`.

## How a plan is chosen

1. **Tempo.** The incoming speed that matches the beat periods (1:1, or half and double time), closest to 1 and
   within `MAX_STRETCH` (±5 %, where 86 % of real DJ tempo changes lie); same beats per bar and subdivision required.
   A mashup allows `MASHUP_MAX_STRETCH` (±10 %, 94.5 % of real changes), since its vocals must ride the other beat.
   Whether a pair matches is told from the songs' overall tempos; how it is lined up is told from **fitted grids**
   (`grid.rs`). The analyzer reports each beat on one of its spectrogram frames, 20 ms apart, so beats lined up by
   those positions alone sit up to a frame apart and drift. For every cue, a least-squares line is fitted through
   each song's beats of the overlap and `FIT_MARGIN_BEATS` (16) on either side (through the overlap alone when a
   missed or extra beat makes the wider window irregular): the ratio of the two periods is the plan's speed, and the
   incoming entry is moved so the two fitted downbeats meet (a test with beats rounded to 20 ms lines the real beats
   up within 3 ms and the speed within 0.03 %). Irregular beats keep the detected positions and the overall tempos.
2. **Technique.** Beat-matched songs with compatible keys draw the first technique from Vande Veire and De Bie's
   Markov chain over relaxed, rolling and double-drop transitions, given the previous one and weighted by intensity
   (soft settings favour relaxed hand-overs, energetic ones double drops), then try the others. Clashing keys sweep
   filters. Songs that cannot be beat-matched fade. The mashup style tries a `Mashup` first when the songs allow one:
   stems can be had and both sources allow separating them (`rights.separate`: the library and YouTube), the tempos
   meet within `MASHUP_MAX_STRETCH`, the vocal model measured both songs' vocals, and the keys are compatible or
   become so by moving the outgoing vocals up to `MAX_VOCAL_SHIFT` semitones (`vocal_shift`; 76 % of real pairs,
   against 20 % whose keys fit as they are).
   It may also plan a `Fusion` when a fusion can be rendered (`fusing`), both sources allow generating
   (`rights.generate`: the library and YouTube), the keys are compatible and the tempos meet within
   `MASHUP_MAX_STRETCH`; when both suit the pair, the seed draws which it tries first. With `always_fuse`
   (`PlaybackPreferences::dj_always_fuse`, a switch) it plans a `Fusion` first for every pair, whatever the tempos,
   keys or meters: the incoming song is stretched to the outgoing beat within `FUSION_MAX_STRETCH` (±30 %, which half
   and double time make enough for any two tempos), and clashing keys are rendered from `FUSION_CLASH_STRENGTH` (0.65)
   instead of `FUSION_STRENGTH` (0.5), so the model plays both harmonies as one. On 47 real songs (all 2,162 ordered
   pairs, 8 seeds each) the style fuses 6.7 % of transitions without the switch and 88 % with it; the rest fade where
   no cue fits. Otherwise it mixes like the professional style.
3. **Cues.** Every bar where both songs start a phrase is a candidate. A phrase starts at a section whose energy
   differs from the one before (1), any other section (0.8), and every eight (0.6) or four bars (0.4) of a section,
   counted from either end. Where each song switches depends on the technique: relaxed hand-overs, sweeps and fades
   switch into the outgoing song's calm part (a strong section counts less: its chorus would be cut short) from the
   incoming intro, from its start or a later phrase of a calm intro; rolling mixes switch inside the outgoing song's
   last strong part, its fade-out ending where it turns calm, onto an incoming drop; double drops go from drop to
   drop. A mashup switches on a phrase line where the outgoing song sings at least half the ride that follows
   (`MASHUP_SUNG_SHARE`), best where the ride ends on another phrase line or where the song stops singing, onto an
   incoming stretch that starts on its first bar or a phrase line within 16 bars. Its stems mute the incoming vocals
   until the ride ends, so that stretch need not be instrumental (most songs sing from their first bars on): the
   incoming vocals best come in where they start singing, otherwise on a phrase line, and sung bars played without
   their vocals cost up to `MUTED_POINTS` (0.5). A cue must start
   after the earliest exit, switch while the outgoing song is still audible (`TrackAnalysis::audible_end`: its
   trailing silence and most of a fade-out do not count; a fade and a mashup's ride must fit before it) and leave at most 25 % of it unheard (45 % for a double drop). An
   incoming song starts on a downbeat, or without a beat grid where its sound starts (`audible_start`), never on
   silence.
4. **Length.** Hand-overs try half the bars of `MixLength::bars()` (4, 8 or 16 bars in all), then halves down to 2
   bars, each halving costing points; sweeps take at most 4 bars. In a mashup the other half is the ride. Fades
   between unmatched songs last 1 bar, or 2 or 4 bars where one song's overlapped bars are beatless (no kick or
   bassline), so drifting beats cannot clash, and never less than 4 s.
5. **Score and draw.** Points add how strongly both songs start a phrase at the switch, how far below each song's
   average the overlapped mids are, and subtract the share of the outgoing song left unheard, the incoming intro
   skipped, shorter lengths, the beat error, the loudness step and every bar both songs sing at once beyond one.
   A mashup changes the backing under a voice that goes on, so instead of quiet mids it wants the backing to keep
   its level and its beat: it loses a point per `STEP_DB_PER_POINT` (10 dB) between the outgoing bars before the
   switch and the incoming bars after it (with the loudness trim), and half as much again for the bass alone.
   A bar is sung when its vocals (`TrackAnalysis::vocal_db`) reach within 15 dB of the song's loud vocals (90th
   percentile) and −40 dBFS. As Vande Veire and De Bie tolerate, a clash is accepted only when every cue of every
   technique clashes: a technique whose cues all clash gives way to the next. The 24 best cues per technique
   are built and evaluated; among those that pass, the plan is drawn with softmax odds (a cue 0.25 points worse is
   drawn 2.7 times less often, and one more than a point worse never), and its sound is drawn too: a high-pass exit
   instead of a bass cut or a reverb tail. A mashup is played unfiltered: its vocals carry it.
6. **Lanes.** The incoming song rises with its bass and treble cut and its mids 6 dB low; bass and treble change hands
   ending on the switch downbeat, over a bar at soft settings and a quarter beat (behind the kick) otherwise; then the
   outgoing song fades. With stems (`MixRequest::separation` and both sources' `separate` right), the `vocals` and
   `instrumental` lanes play each song's vocals apart from the rest: a cue whose vocals would clash is kept by
   holding them apart (the outgoing vocals fade over the bar before the switch and the incoming ones come in over
   its last beat, whole on the downbeat where their phrase starts; 0.3 points), and a relaxed or rolling hand-over
   may let the outgoing vocals ride over the incoming instrumental (*acapella*: its instrumental leaves with the
   bass swap, its vocals fade over the ride's last bar unfiltered, where the incoming vocals come in) with odds of
   30 % and 35 %. A mashup always rides (`mashup_lanes`): the incoming instrumental rises under the outgoing song
   with its own vocals muted, its bass cut, and its treble opening from −24 dB to `TREBLE_LEAD_DB` (−9 dB) over the
   last `TREBLE_LEAD_BARS` (2), so its hats do not jump in; on the switch downbeat the bass changes hands and the
   outgoing instrumental drops to 60 % (`INSTRUMENTAL_BLEND`) and keeps blending with the incoming one over half the
   ride (`MASHUP_BLEND_SHARE`) before it leaves, and with it the bass cut of the outgoing deck, so the riding
   vocals get their low end back. They ride at the level of a quieter incoming song (the loudness trim mirrored, at
   most 6 dB, reached over their first bar) until the ride's last bar, where they fade (equal power) into a reverb
   (`VOCAL_REVERB_SEND`, 0.5) while the incoming vocals come in over the last beat, whole on the downbeat. When the
   vocals are moved the keys clash, so the two harmonies never meet: until the switch the incoming song shows only
   its hats (a high-pass at `HATS_HIGHPASS`, 5.1 kHz), and on the switch it opens while the outgoing instrumental
   leaves with the swap and the vocals change to their moved pitch. Every reverb send closes where its deck falls
   silent (`reverb_wash`), so the reverb rings out what it was given and the song still playing under the closed
   gain no longer feeds it until the deck is released. `MixPlan::uses_stems` and `stem_frames` tell
   the application which windows to separate. A louder incoming song is attenuated to the outgoing loudness (at
   most 6 dB, never boosted) and returns to its own level over 8 s after the overlap.
7. **Evaluation.** The distance between the incoming beats (downbeats with half or double time) and the outgoing
   ones over the overlap, averaged over every eight of them, must stay within `MAX_BEAT_ERROR_MS` (10 ms), and both
   basslines may be above −6 dB for at most one beat. Averaging two bars leaves out the rounding of each detected
   beat to the analyzer's 20 ms frames, which says nothing about the songs: judged beat by beat, only 1.9 % of the
   plans between 49 real songs were beat-matched, although 15.5 % of their pairs have tempos within 5 %.
8. **Fusions.** `Fusion { before, length, after, seam, strength }` (`fusion.rs`): from a phrase line of the outgoing
   song, both songs blend beat-matched (by beats alone, so meters may differ) over `FUSION_BARS` (4, 8 or 16 bars by length, halved while longer than
   `MAX_FUSION_SECONDS`, 20 s), and a generative model renders that blend again from `FUSION_STRENGTH` (0.5) noise,
   hearing `FUSION_CONTEXT_BARS` (4) of each song around it. The fusion must end at most 25 % before the outgoing
   music does; the incoming song enters after it on a phrase line of its first bars (better on a strong section) and
   plays the fusion's bars before its entry under it. Every bar either song sings during it costs 0.5 points. The
   decks hand over to the rendering over one beat before the exit and take over from it over one beat after the fusion
   (`Fusion::gain`); the incoming deck starts when the fusion ends. `MixPlan::fusion` carries it.
9. **Without a beat grid.** An outgoing song without a tempo or meter fades over 4 s ending where it stops being
   audible, or from the earliest exit when that is later.

## Invariants and trust boundary

- Pure: the only randomness is the request's seed, and the same request gives the same plan. Plans are validated
  (`MixPlan::validate`) before they are returned, and `yt-media` validates them again (`Mix::new`) before the audio
  thread sees them.
- Analyses come from the worker or the cache and were validated there; indexing never trusts them (`get`).
- `DeckProcessor::process` and `set` never allocate, lock or block; only `new` allocates the reverb lines.

## Performance

A plan scores at most a few hundred cues per technique from per-bar levels, then builds and evaluates the best 24;
each one fits two grids to at most a few hundred beats, compares as many and samples the lanes every 50 ms. On 49
real analyses (18,816 plans, release, M4 Pro) a plan takes 45 µs at the median, 0.2 ms at the 95th percentile and
0.85 ms at most. Per frame, `process` runs only the active stages (at most five biquads per channel and the reverb);
the engine calls `set` every 64 frames.

## Measured on real songs

`plan_probe` over the 49 analyses of the test profile, every ordered pair (2,352) with 8 seeds each, the mashup style
without the fusion model and a voice assumed where it was not measured (2026-11-12):

| Planner | Mashup | of them, vocals moved | Filter sweep | Fade |
|---|---|---|---|---|
| Before the fitted grids and the moved vocals | 0.6 % | – | 1.3 % | 97.9 % |
| Now | 18.8 % | 12.8 % | 3.3 % | 77.9 % |

With the professional style, beat-matched mixes went from 1.9 % to 12.9 % of the plans. What still fades are pairs
whose tempos lie further than 10 % apart (74 % of the pairs) or whose meters differ (23 %).

## Limits

- Stems only for sources that allow separating them (the library and YouTube); otherwise the songs overlap whole,
  carved by the EQ and filters. Vocals are known per bar once measured; before that (or without the vocal model) they are judged by the
  mid band alone.
- The evaluation reads analyses and lanes, not rendered audio.
- Only the incoming song changes speed, so tempos further apart than 5 % (10 % in a mashup) fade instead of mixing;
  a fusion stretches it up to 30 % and glides it back afterwards.
- Beats are known to the analyzer's 20 ms frames. The fitted grids average that away, except for a tempo whose beat
  is a whole number of frames (100, 120 or 125 BPM): its beats are all rounded the same way, which no fit can tell,
  so such a song may sit up to 10 ms off. Finer beat positions need the analyzer to refine them from the audio.
- A mashup needs the vocal model: without measured vocals, the mashup style mixes like the professional one.
- Moved vocals follow the key estimates: a wrong one moves them to a key that does not fit. Keys further than two
  semitones from a compatible one (24 % of real pairs) are not mashed up.
- A fusion needs the generative model and the right to generate, which the library and YouTube grant; its quality
  depends on the model (Stable Audio 3 renders vocals poorly, so sung bars cost points).

## Verification

`cargo test -p yt-mix`: curves (interpolation, holds, validation), lanes (identity, ranges), DSP (identity is
bit-exact, a low kill removes 50 Hz by more than 25 dB and keeps 3 kHz, filter cutoffs, the reverb rings and
decays), the Camelot wheel and the fewest semitones that fit vocals to another key, and the planner on synthetic
songs with sections: relaxed hand-overs on
a phrase of the outro, most often where it starts, from the incoming intro; the songs not singing over each other; rolling and double-drop mixes meeting the
incoming drop; mashups blending the instrumentals and handing the vocals over on a phrase line, only where both
songs' vocals are measured and the sources allow separating them, into songs that sing from their first bars, taking
over where the incoming song is as loud as the outgoing one, and moving the vocals when the keys clash (never beside
their own instrumental, never further than a whole tone); beats rounded to the analyzer's frames still meeting within
3 ms (`grid.rs` has its own tests: a grid finds the beat between the frames, and a missed beat is not fitted through);
with stems, vocals held apart or riding over the instrumental and only
for sources that allow it; fusions only where the model, the right and the style allow them, timed around their
seams; new seeds moving the cue and the sound; clashing keys; unmatched songs fading for at least
4 s on a calm phrase at every intensity; half time; rights and the earliest exit; drifting beats shortening the mix; loudness trim;
plan bounds; a reverb ringing out without being fed by the silent deck; a song strong until its end; a fading
tail; a song without a beat grid.

By hand, `cargo run --release -p yt-mix --example plan_probe -- a.json b.json …` plans every consecutive pair of
stored analyses (the `analysis` column of `rust_dj_analysis`) with several seeds and prints each plan and its time.
`--mashup` plans with the mashup style (`--no-fusion` without the fusion model), `--assume-vocals` gives unmeasured
songs a voice, `--all-pairs` plans every ordered pair, and `--quiet` prints only the share of each technique.
