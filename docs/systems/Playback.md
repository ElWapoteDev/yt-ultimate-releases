# Playback

How the next song is chosen and how it replaces the current one: the queue, autoplay, DJ crossfades, and the
full-screen now-playing view.

## Files

- `crates/yt-media/src/playback.rs`, `crates/yt-media/src/crossfade.rs`: the engine (decks, envelopes,
  `Transition`, `PreparedStream`, the analysis tap that fills `PlaybackSnapshot::analysis` for the active deck). The
  `yt-media` README owns its guarantees; Discovery.md describes what the measurements feed.
- `crates/yt-core/src/settings.rs`: `PlaybackPreferences` (autoplay, DJ mode, crossfade length within
  `CROSSFADE_SECONDS`, `CrossfadeCurve`, crossfade on skip).
- `crates/yt-app/src/state.rs`: `play`, `play_with`, `playback_started`, `next`, `previous`, the 250 ms poll.
- `crates/yt-app/src/remote.rs`: online starts with a transition (`start_remote_with`, `start_remote_audio`) and
  `reset_remote`.
- `crates/yt-app/src/autoplay.rs`: suggestions after the queue.
- `crates/yt-app/src/dj.rs`: `transition_for`, `dj_tick`, prepared streams, curve labels (the mix planning is in
  `docs/systems/Dj.md`).
- `crates/yt-app/src/player.rs`: player bar, `TransportControls`, `SeekBar`.
- `crates/yt-app/src/queue_ui.rs`, `crates/yt-app/src/queue_ui.css`: the queue panel and its autoplay block.
- `crates/yt-app/src/now_playing.rs`, `crates/yt-app/src/now_playing_ui.css`: the full-screen view.
- `crates/yt-app/src/settings_ui.rs`: `PlaybackSection`.

## Choosing what plays next

`next(automatic)` takes, in order: the same song (repeat one, automatic only), the queue (a random entry with
shuffle, unless a DJ blend already reserved one at the front), the session again (repeat all), then autoplay. With an
empty queue and no suggestion ready, playback pauses and the first suggestion starts as soon as it arrives.

## Autoplay

`refresh_autoplay` runs when a playback starts with an empty queue and when the preference changes. It keeps up to 10
suggestions seeded by the current song: with online recommendations, YouTube's Mix and related videos for its id
(a library track uses its source, else a search by artist and title), ranked with `RankingConfig::online`; otherwise,
or when that fails, the library ranked with `RankingConfig::library_autoplay`. Suggestions exclude the current song,
the queue and the last 50 songs of the session. Picks are attributed to autoplay in the listening history. Autoplay
never runs while repeat is on. The queue panel shows the suggestions and a switch.

## DJ mode

- `transition_for(next, automatic)` returns a crossfade when DJ mode is on, both songs are audio, something is
  playing, and an online next song already has a prepared stream; otherwise a cut. Automatic transitions use the
  configured length (default 6 s); manual skips, when «Fundir también al saltar» is on, use 1.5 s.
- `dj_tick` (shared poll) watches the remaining real time (media time / rate). Fifteen seconds before the blend it
  resolves and opens the next online song's stream without making it audible; at «remaining ≤ fade length» it calls
  `next(true)`, so the blend ends exactly when the song ends. Songs shorter than three fade lengths, videos, and
  repeat one never blend; an online song whose stream is not ready ends normally instead of being cut early.
- During a blend the outgoing online session stays alive (`reset_remote(false)`); the engine releases it when its
  ramp ends. Pausing pauses both decks; seeking finishes the blend.
- With the professional style (the default) and 1× speed, the transition is a beat-matched mix that starts at a
  planned exit on a downbeat, usually before the end of the song, instead of the crossfade; the next song is chosen
  early, an online one opens 90 s before the end, and the crossfade above remains the fallback whenever no mix is
  ready. `docs/systems/Dj.md` owns the analyses, the planning and the engine's mix; stem-based and generative
  transitions are designed in `docs/AI_DJ.md` (Spanish).

## Full-screen view

The expand button in the player bar (or «Pantalla completa» on Home) opens `NowPlaying`, which enters window full
screen while mounted and restores the previous window state when closed (Esc or the chevron). It shows the cover large
(the album cover at its original size, `docs/systems/Artwork.md`) over a blurred copy and an aurora: four soft blobs
in the song's three ambient colors and its accent (`docs/systems/Theme.md`) that drift on their own compositor layers
inside a layer that turns once every four minutes, only while the song plays. Then title and artist, the favorite
button, the shared `SeekBar` and `TransportControls`, a row of bare icon actions (`NowPlayingAction`: «Letra», «Modo
DJ», «Reproducción automática», «Crear estación») and the next four songs. The actions speak the language of shuffle
and repeat instead of filled capsules: their names are their tooltips and accessible labels, and a toggle that is on
takes the accent and a dot. «Letra» shows the karaoke lyrics in place of the cover (`docs/systems/Lyrics.md`). A video
opens its own stage full screen instead. While two songs blend, the DJ's show joins the view: rays behind
everything, rings around the cover (in a stack with it, `now-playing__art-stack`), the leaving song's cover sliding
off the playing one, and a banner above the title with a crossfader from one song to the other
(`docs/systems/Dj.md`, «The blend's show»). Only `transform` and `opacity` animate, plus the ambient colors' 900 ms
transition when the song changes; reduced motion stops every movement.

## Trust boundary

Seek and slider values from the WebView are parsed and validated in Rust (`seek` clamps to the duration; crossfade
length must be inside `PlaybackPreferences::CROSSFADE_SECONDS` before it is saved, and `yt-core` validates again).

## Tests

`yt-media` crossfade tests (curves, sample-accurate ramps, re-targeting, packing), `crates/yt-app/src/dj.rs`
(transition rules, shuffle reservation), `crates/yt-app/src/autoplay.rs` (library suggestions),
`crates/yt-app/src/now_playing.rs` and `crates/yt-app/src/settings_ui.rs` (rendering from bounds).
