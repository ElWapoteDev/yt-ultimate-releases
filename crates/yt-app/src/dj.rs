//! DJ controller: the transitions between consecutive songs. The
//! professional style plans a mix (`yt_mix`) from both songs' analyses, with
//! a new seed for every transition so the same pair never has to mix the
//! same way, and lets the engine start it on the exact exit frame; the
//! classic style, and any mix that is not ready in time, crossfade at the
//! end. The next online song is opened early so it blends in on time. See
//! `docs/systems/Playback.md` and `docs/systems/Dj.md`.

use crate::state::AppState;
use dioxus::prelude::*;
use std::{
    cell::{Cell, RefCell},
    hash::{BuildHasher, RandomState},
    path::PathBuf,
    rc::Rc,
    time::Duration,
};
use yt_core::{CrossfadeCurve, DjStyle, MediaOrigin, MixLength, SourceRights, Track};
use yt_media::{
    CancellationToken, Crossfade, FadeCurve, Mix, PreparedStream, SearchResult, StreamKind,
    Transition,
};
use yt_mix::MixRequest;

/// Blend used when the user skips or picks another song with DJ mode on.
const MANUAL_CROSSFADE: Duration = Duration::from_millis(1_500);
/// Seconds before an automatic blend at which an online song starts opening.
const PREPARE_LEAD_SECONDS: f64 = 15.0;
/// With the professional style, seconds before the end of a song at which the
/// next online song starts downloading, so it can be analyzed and mixed.
const MIX_PREPARE_LEAD_SECONDS: f64 = 90.0;
/// The same with the mashup style, whose fusions wait for the model: about
/// 15 s of GPU for an 8-bar fusion, after the next song's download, analysis
/// and the governor's rests, well before the fusion near the song's end.
const AI_PREPARE_LEAD_SECONDS: f64 = 180.0;
/// Seconds before a mix's exit at which the next song is handed to the
/// engine; the engine then starts it on the exit frame itself.
const MIX_ARM_SECONDS: f64 = 1.0;
/// A mix is planned to start at least this many seconds after the playing
/// position, so it can still be armed on time.
const MIX_PLAN_LEAD_SECONDS: f64 = 5.0;
/// Names of the DJ intensities, from `PlaybackPreferences::DJ_INTENSITY`'s
/// start to its end.
const INTENSITY_NAMES: [&str; 5] = ["Suave", "Tranquila", "Equilibrada", "Animada", "Enérgica"];
/// Songs shorter than this many crossfades never blend: the blend would cover
/// most of them.
const MIN_LENGTH_IN_CROSSFADES: f64 = 3.0;
/// Slowest playback rate used to convert remaining media time to real time.
const MIN_RATE: f64 = 0.25;

struct PreparedNext {
    track_id: String,
    media: SearchResult,
    stream: PreparedStream,
    /// Parent of the stream's session; cancelled only to discard it.
    cancel: CancellationToken,
}

/// What a planned mix was planned for; any change plans it again, such as
/// the vocals of either song arriving after the rest of its analysis.
#[derive(Clone, PartialEq)]
struct MixKey {
    current: String,
    next: String,
    length: MixLength,
    intensity: u8,
    vocals: [bool; 2],
    /// Whether the plan may play the songs' vocals apart.
    separation: bool,
    /// Whether the plan may fuse the songs with the generative model, and
    /// whether every pair should.
    fusing: bool,
    always_fuse: bool,
    style: DjStyle,
}

/// The mix between the playing song and the next one, or `None` when the
/// planner found none for them (they crossfade).
struct PlannedMix {
    key: MixKey,
    mix: Option<Mix>,
    /// Seconds into the playing song where the mix starts.
    exit_seconds: f64,
    extra: Extra,
    /// Tells this plan apart from later ones when its extra audio arrives.
    generation: u64,
}

/// Whether a planned mix waits for the songs' vocal stems.
enum Extra {
    /// Not needed, or already added to the mix.
    Ready,
    Preparing(CancellationToken),
    /// It could not be had: the pair is planned again without it.
    Failed,
}

#[derive(Clone, Default)]
pub(crate) struct Dj {
    prepared: Rc<RefCell<Option<PreparedNext>>>,
    preparing: Rc<RefCell<Option<(String, CancellationToken)>>>,
    /// The next queue entry was already chosen (shuffle) for a blend.
    reserved: Rc<Cell<bool>>,
    /// The current playback already started its automatic blend; reset by
    /// every new playback so a song that returns later can blend again.
    blend_started: Rc<Cell<bool>>,
    planned: Rc<RefCell<Option<PlannedMix>>>,
    /// The technique of the last mix that played, which shapes the next one.
    last_technique: Rc<Cell<Option<yt_mix::Technique>>>,
    /// The stems of this playback's mix could not be had; plan without them.
    extras_refused: Rc<Cell<bool>>,
    generation: Rc<Cell<u64>>,
}

/// Name and explanation of a DJ style for the interface.
pub(crate) fn style_label(style: DjStyle) -> (&'static str, &'static str) {
    match style {
        DjStyle::Professional => (
            "Profesional",
            "Mezcla a tempo sobre los compases, con cambio de bajos y filtros; si la mezcla no está lista, funde",
        ),
        DjStyle::Classic => ("Clásico", "Un fundido al final de cada canción"),
        DjStyle::Mashup => (
            "Mashup con IA (experimental)",
            "La IA junta las canciones como un mashup: mezcla sus instrumentales, deja una voz sobre el instrumental de la otra (movida de tono si hace falta para que encaje) o vuelve a tocar la mezcla como una sola canción, sin que se note el cambio",
        ),
    }
}

/// What the DJ settings say when AI fusions are installed: the Stability AI
/// Community License asks to display «Powered by Stability AI».
pub(crate) const FUSION_ATTRIBUTION: &str =
    "Las fusiones usan Stable Audio 3 en tu equipo · Powered by Stability AI";

/// What the player calls a planned mix, and whether an AI model shaped it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MixName {
    pub(crate) label: &'static str,
    /// The songs' vocals were measured by the vocal model or are played
    /// apart, or the generative model renders the mix.
    pub(crate) ai: bool,
}

/// What the player shows of the blend being heard besides its progress: the
/// song that leaves and how the music is timed, so its effects move with it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BlendScene {
    /// The song that is leaving.
    pub(crate) from: Track,
    /// Seconds both songs sound together.
    pub(crate) seconds: f32,
    /// Seconds per beat while they do, when they are beat-matched.
    pub(crate) beat_seconds: Option<f32>,
}

/// The mix planned out of the playing song, as the player shows it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct NextMix {
    /// Seconds into the playing song where the mix starts and where the
    /// songs stop sounding together.
    pub(crate) start: f64,
    pub(crate) end: f64,
    pub(crate) name: MixName,
    /// Its vocal stems are still being separated or its fusion rendered.
    pub(crate) preparing: bool,
}

/// The name of `plan`'s technique for the player; `vocals_known` when the
/// vocal model measured either song.
pub(crate) fn mix_name(plan: &yt_mix::MixPlan, vocals_known: bool) -> MixName {
    use yt_mix::Technique::*;
    let label = match plan.technique {
        Mashup => "Mashup",
        Fusion => "Fusión IA",
        _ if plan.uses_stems() => {
            if plan.outgoing.instrumental != yt_mix::Curve::constant(1.0) {
                "Acapella"
            } else {
                "Voces separadas"
            }
        }
        Relaxed => "Entrada suave",
        Rolling => "Entrada rodando",
        DoubleDrop => "Doble drop",
        FilterSweep => "Barrido de filtros",
        Fade => "Fundido",
    };
    MixName {
        label,
        ai: vocals_known || plan.uses_stems() || plan.fusion.is_some(),
    }
}

/// Name and explanation of a mix length for the interface.
pub(crate) fn length_label(length: MixLength) -> (&'static str, String) {
    let name = match length {
        MixLength::Short => "Corta",
        MixLength::Medium => "Media",
        MixLength::Long => "Larga",
    };
    (
        name,
        format!(
            "Hasta {} compases de las dos canciones a la vez",
            length.bars()
        ),
    )
}

/// What the player calls a blend while it is heard.
pub(crate) fn blend_label(blend: yt_media::Blend) -> &'static str {
    if blend.mix {
        "Mezclando"
    } else {
        "Fundiendo"
    }
}

/// Name of a DJ intensity for the interface.
pub(crate) fn intensity_label(intensity: u8) -> &'static str {
    let first = *yt_core::PlaybackPreferences::DJ_INTENSITY.start();
    INTENSITY_NAMES
        .get(usize::from(intensity.saturating_sub(first)))
        .copied()
        .unwrap_or(INTENSITY_NAMES[INTENSITY_NAMES.len() - 1])
}

/// Name and explanation of a curve for the interface.
pub(crate) fn curve_label(curve: CrossfadeCurve) -> (&'static str, &'static str) {
    match curve {
        CrossfadeCurve::EqualPower => (
            "Potencia constante",
            "El volumen se siente parejo durante todo el fundido (recomendado)",
        ),
        CrossfadeCurve::Linear => (
            "Lineal",
            "Cambio directo; el punto medio suena un poco más bajo",
        ),
        CrossfadeCurve::Smooth => (
            "Suave",
            "Empieza y termina despacio; el cambio se siente más corto",
        ),
    }
}

/// Seconds into a song of `duration` seconds where its music ends, before
/// any trailing silence, from its `analysis` when there is one.
fn music_end(analysis: Option<&yt_core::TrackAnalysis>, duration: f64) -> f64 {
    analysis.map_or(duration, |analysis| {
        analysis.seconds(analysis.audible_end()).min(duration)
    })
}

fn fade_curve(curve: CrossfadeCurve) -> FadeCurve {
    match curve {
        CrossfadeCurve::EqualPower => FadeCurve::EqualPower,
        CrossfadeCurve::Linear => FadeCurve::Linear,
        CrossfadeCurve::Smooth => FadeCurve::Smooth,
    }
}

impl AppState {
    /// Turn DJ mode on or off; turning it off cancels a prepared stream and
    /// every analysis.
    pub(crate) fn set_dj_mode(&self, enabled: bool) {
        self.edit_settings(|settings| settings.playback.dj_mode = enabled);
        if !enabled {
            self.discard_prepared_stream();
            self.cancel_dj_analyses();
        }
    }

    /// How `next` should replace what plays: a blend when DJ mode allows it
    /// and both songs are audio, otherwise a cut. Online songs blend only
    /// when their stream is already prepared, so no gap of silence appears
    /// in the middle of a blend. Publishes the blend's scene for the player.
    pub(crate) fn transition_for(&self, next: &Track, automatic: bool) -> Transition {
        let transition = self.pick_transition(next, automatic);
        let scene = self.blend_scene_of(&transition);
        if *self.blend_scene.peek() != scene {
            let mut blend_scene = self.blend_scene;
            blend_scene.set(scene);
        }
        transition
    }

    /// The scene of the blend `transition` starts out of the playing song.
    fn blend_scene_of(&self, transition: &Transition) -> Option<BlendScene> {
        let from = self.current.peek().clone()?;
        match transition {
            Transition::Cut => None,
            Transition::Crossfade(fade) => Some(BlendScene {
                from,
                seconds: fade.duration().as_secs_f32(),
                beat_seconds: None,
            }),
            Transition::Mix(mix) => {
                let plan = mix.plan();
                // Songs that fade without matching tempos share no beat.
                let beat_seconds = (plan.technique != yt_mix::Technique::Fade)
                    .then(|| self.dj_analysis.ready(&from.id)?.tempo_bpm)
                    .flatten()
                    .map(|tempo| 60.0 / tempo);
                Some(BlendScene {
                    from,
                    seconds: plan.overlap,
                    beat_seconds,
                })
            }
        }
    }

    fn pick_transition(&self, next: &Track, automatic: bool) -> Transition {
        let preferences = self.settings.peek().playback.clone();
        if !preferences.dj_mode || (!automatic && !preferences.crossfade_on_skip) {
            return Transition::Cut;
        }
        let audio_now = self
            .current
            .peek()
            .as_ref()
            .is_some_and(|track| track.media_type != "video");
        if !audio_now || next.media_type == "video" || !*self.playing.peek() {
            return Transition::Cut;
        }
        if crate::remote::is_remote(next) && !self.stream_prepared_for(&next.id) {
            return Transition::Cut;
        }
        if automatic {
            if let Some(mix) = self.planned_mix_for(next) {
                self.dj.last_technique.set(Some(mix.plan().technique));
                let vocals_known = self
                    .dj
                    .planned
                    .borrow()
                    .as_ref()
                    .is_some_and(|planned| planned.key.vocals.contains(&true));
                let mut mixing_as = self.mixing_as;
                mixing_as.set(Some(mix_name(mix.plan(), vocals_known)));
                return Transition::Mix(mix);
            }
        }
        let duration = if automatic {
            Duration::from_secs(preferences.crossfade_seconds.into())
        } else {
            MANUAL_CROSSFADE
        };
        // An automatic blend starts the next song where its sound starts.
        let start = automatic
            .then(|| self.dj_analysis.ready(&next.id))
            .flatten()
            .map_or(Duration::ZERO, |analysis| {
                Duration::from_secs_f64(analysis.seconds(analysis.audible_start()))
            });
        Crossfade::new(duration, fade_curve(preferences.crossfade_curve))
            .map(|fade| Transition::Crossfade(fade.starting_at(start)))
            .unwrap_or_default()
    }

    /// Start the automatic transition and open the next online song's stream
    /// ahead of it. With the professional style the next song is chosen
    /// early (shuffle reserves it) so both songs can be analyzed and the mix
    /// planned; the mix starts at its planned exit. Otherwise, and whenever
    /// no mix is ready, the songs crossfade at the end. Called from the
    /// shared poll.
    pub(crate) fn dj_tick(&self) {
        let preferences = self.settings.peek().playback.clone();
        if !preferences.dj_mode {
            self.discard_prepared_stream();
            self.cancel_dj_analyses();
            return;
        }
        let professional = preferences.dj_style.plans_mixes() && *self.rate.peek() == 1.0;
        if professional && *self.playing.peek() && *self.repeat.peek() != 2 {
            let _ = self.peek_next();
        }
        self.request_dj_analyses();
        if *self.repeat.peek() == 2 || *self.stream_loading.peek() || !*self.playing.peek() {
            return;
        }
        let audio_now = self
            .current
            .peek()
            .as_ref()
            .is_some_and(|track| track.media_type != "video");
        if !audio_now {
            return;
        }
        let duration = *self.duration.peek();
        let fade = f64::from(preferences.crossfade_seconds);
        if duration <= fade * MIN_LENGTH_IN_CROSSFADES {
            return;
        }
        let rate = f64::from(*self.rate.peek()).max(MIN_RATE);
        let position = *self.position.peek();
        // Blends end where the music does, never in a trailing silence.
        let current = self.current.peek().clone();
        let analysis = current
            .as_ref()
            .and_then(|track| self.dj_analysis.ready(&track.id));
        let music_end = music_end(analysis.as_deref(), duration);
        let remaining = (music_end - position).max(0.0) / rate;
        let lead = if preferences.dj_style == DjStyle::Mashup && professional {
            AI_PREPARE_LEAD_SECONDS
        } else if professional {
            MIX_PREPARE_LEAD_SECONDS
        } else {
            fade + PREPARE_LEAD_SECONDS
        };
        if remaining > lead {
            return;
        }
        let Some(next) = self.peek_next() else {
            return;
        };
        if next.media_type == "video" {
            return;
        }
        let ready = if crate::remote::is_remote(&next) {
            self.prepare_stream_for(&next);
            self.stream_prepared_for(&next.id)
        } else {
            !next.missing
        };
        if professional {
            if let Some(exit) = self.planned_exit(&next, position) {
                let until_exit = (exit - position) / rate;
                if ready && until_exit <= MIX_ARM_SECONDS && !self.dj.blend_started.replace(true) {
                    self.next(true);
                }
                return;
            }
        }
        if ready && remaining <= fade && !self.dj.blend_started.replace(true) {
            self.next(true);
        }
    }

    /// A new playback started: its own automatic blend has not begun and the
    /// previous pair's mix is no longer wanted.
    pub(crate) fn reset_blend(&self) {
        self.dj.blend_started.set(false);
        self.dj.extras_refused.set(false);
        if let Some(PlannedMix {
            extra: Extra::Preparing(cancel),
            ..
        }) = self.dj.planned.borrow_mut().take()
        {
            cancel.cancel();
        }
    }

    /// The extra audio of the plan of `generation` arrived (`with_extra`, the
    /// mix with it), or could not be had (`None`).
    pub(crate) fn extra_ready(&self, generation: u64, with_extra: Option<Mix>) {
        let mut planned = self.dj.planned.borrow_mut();
        let Some(planned) = planned
            .as_mut()
            .filter(|planned| planned.generation == generation)
        else {
            return;
        };
        match with_extra {
            Some(mix) => {
                planned.mix = Some(mix);
                planned.extra = Extra::Ready;
            }
            None => planned.extra = Extra::Failed,
        }
    }

    /// Seconds into the playing song where the mix into `next` starts, planning
    /// it once both analyses are ready. `None` while they are not, when no mix
    /// suits the pair, or when the playing position has passed the planned
    /// exit and nothing later suits it.
    fn planned_exit(&self, next: &Track, position: f64) -> Option<f64> {
        let current = self.current.peek().clone()?;
        let preferences = self.settings.peek().playback.clone();
        let outgoing = self.dj_analysis.ready(&current.id);
        let incoming = self.dj_analysis.ready(&next.id);
        // Stems come from the same files the analysis read: a library file or
        // a complete online download.
        let audio = self
            .audio_file(&current, true)
            .zip(self.audio_file(next, false));
        let client = self.dj_analysis.client(&self.services.profile);
        let extras = !self.dj.extras_refused.get() && audio.is_some();
        let separation = extras
            && client
                .as_ref()
                .is_some_and(|client| client.measures_vocals());
        let fusing = extras
            && client
                .as_ref()
                .is_some_and(|client| client.renders_fusions());
        let key = MixKey {
            current: current.id.clone(),
            next: next.id.clone(),
            length: preferences.dj_length,
            intensity: preferences.dj_intensity,
            vocals: [&outgoing, &incoming]
                .map(|analysis| analysis.as_ref().is_some_and(|a| !a.vocal_db.is_empty())),
            separation,
            fusing,
            always_fuse: preferences.dj_always_fuse,
            style: preferences.dj_style,
        };
        {
            let mut planned = self.dj.planned.borrow_mut();
            // A fusion does not depend on the vocals: their measurement
            // arriving does not throw away its rendering.
            let stale = planned.as_ref().is_some_and(|planned| {
                let fused = planned
                    .mix
                    .as_ref()
                    .is_some_and(|mix| mix.plan().fusion.is_some());
                let unsung = |key: &MixKey| MixKey {
                    vocals: [false; 2],
                    ..key.clone()
                };
                planned.key != key && !(fused && unsung(&planned.key) == unsung(&key))
            });
            if stale {
                if let Some(PlannedMix {
                    extra: Extra::Preparing(cancel),
                    ..
                }) = planned.take()
                {
                    cancel.cancel();
                }
            }
            if let Some(current_plan) = planned.as_ref() {
                let missed = position > current_plan.exit_seconds - MIX_ARM_SECONDS / 2.0;
                match (&current_plan.mix, &current_plan.extra) {
                    (None, _) => return None,
                    (Some(_), Extra::Preparing(_)) if !missed => return None,
                    (Some(_), Extra::Ready) if !missed => return Some(current_plan.exit_seconds),
                    (Some(_), Extra::Ready) => {}
                    // Extra audio that failed or came too late: the next tick
                    // plans the pair again without it.
                    (Some(_), Extra::Preparing(_) | Extra::Failed) => {
                        if let Some(PlannedMix {
                            extra: Extra::Preparing(cancel),
                            ..
                        }) = planned.take()
                        {
                            cancel.cancel();
                        }
                        self.dj.extras_refused.set(true);
                        return None;
                    }
                }
            }
        }
        let (outgoing, incoming) = (outgoing?, incoming?);
        let rights = SourceRights::of(MediaOrigin::of_media_id(&current.id))
            .with(SourceRights::of(MediaOrigin::of_media_id(&next.id)));
        let earliest = (position + MIX_PLAN_LEAD_SECONDS) * f64::from(outgoing.sample_rate);
        let mix = yt_mix::plan(&MixRequest {
            outgoing: &outgoing,
            incoming: &incoming,
            length: preferences.dj_length,
            intensity: preferences.dj_intensity,
            rights,
            earliest_exit: earliest as u64,
            previous: self.dj.last_technique.get(),
            // Every `RandomState` has fresh random keys, so each plan draws anew.
            seed: RandomState::new().hash_one((&current.id, &next.id)),
            separation,
            style: preferences.dj_style,
            fusing,
            always_fuse: preferences.dj_always_fuse,
        })
        .and_then(|plan| Mix::new(plan).ok());
        let exit_seconds = mix.as_ref().map_or(0.0, |mix| {
            mix.plan().exit_frame as f64 / f64::from(mix.plan().outgoing_rate)
        });
        let generation = self.dj.generation.get() + 1;
        self.dj.generation.set(generation);
        let prepare = mix
            .as_ref()
            .filter(|mix| mix.plan().uses_stems() || mix.plan().fusion.is_some())
            .map(|mix| (mix.clone(), CancellationToken::new()));
        let extra = match &prepare {
            Some((_, cancel)) => Extra::Preparing(cancel.clone()),
            None => Extra::Ready,
        };
        let planned = (mix.is_some() && prepare.is_none()).then_some(exit_seconds);
        *self.dj.planned.borrow_mut() = Some(PlannedMix {
            key,
            mix,
            exit_seconds,
            extra,
            generation,
        });
        // After the plan is stored: work that cannot start marks it at once.
        if let Some((mix, cancel)) = prepare {
            match audio {
                Some((from, to)) if mix.plan().fusion.is_some() => {
                    self.dj_analysis.preempt_vocals();
                    let prompt = (outgoing.tempo_bpm.unwrap_or_default(), incoming.key);
                    self.render_fusion(generation, mix, [from, to], prompt, cancel)
                }
                Some((from, to)) => self.separate_stems(generation, mix, [from, to], cancel),
                None => self.extra_ready(generation, None),
            }
        }
        planned
    }

    /// Whether the planned mix's fusion is still being rendered: the model
    /// goes before vocal measurements meanwhile.
    pub(crate) fn preparing_fusion(&self) -> bool {
        self.dj.planned.borrow().as_ref().is_some_and(|planned| {
            matches!(planned.extra, Extra::Preparing(_))
                && planned
                    .mix
                    .as_ref()
                    .is_some_and(|mix| mix.plan().fusion.is_some())
        })
    }

    /// Whether this installation renders AI fusions: their program and
    /// weights are installed.
    pub(crate) fn renders_fusions(&self) -> bool {
        self.dj_analysis
            .client(&self.services.profile)
            .is_some_and(|client| client.renders_fusions())
    }

    /// Publish the mix planned out of the playing song for the player, only
    /// when it changed. Called from the shared poll after `dj_tick`.
    pub(crate) fn publish_next_mix(&self) {
        let preview = self.next_mix_preview();
        if *self.next_mix.peek() != preview {
            let mut next_mix = self.next_mix;
            next_mix.set(preview);
        }
    }

    fn next_mix_preview(&self) -> Option<NextMix> {
        if !self.settings.peek().playback.dj_mode {
            return None;
        }
        let current = self.current.peek().as_ref()?.id.clone();
        let planned = self.dj.planned.borrow();
        let planned = planned
            .as_ref()
            .filter(|planned| planned.key.current == current)?;
        let plan = planned.mix.as_ref()?.plan();
        Some(NextMix {
            start: planned.exit_seconds,
            end: planned.exit_seconds + f64::from(plan.overlap),
            name: mix_name(plan, planned.key.vocals.contains(&true)),
            preparing: matches!(planned.extra, Extra::Preparing(_)),
        })
    }

    /// The mix planned from the playing song into `next`.
    fn planned_mix_for(&self, next: &Track) -> Option<Mix> {
        let current = self.current.peek().as_ref()?.id.clone();
        let planned = self.dj.planned.borrow();
        let planned = planned.as_ref()?;
        (planned.key.current == current && planned.key.next == next.id)
            .then(|| planned.mix.clone())
            .flatten()
    }

    /// The complete download of the stream prepared for `track_id`, and its
    /// size, so the next online song can be analyzed before it plays.
    pub(crate) fn prepared_stream_file(&self, track_id: &str) -> Option<(PathBuf, u64)> {
        let prepared = self.dj.prepared.borrow();
        let prepared = prepared
            .as_ref()
            .filter(|prepared| prepared.track_id == track_id)?;
        Some((
            prepared.stream.complete_file()?,
            prepared.stream.snapshot().total_bytes,
        ))
    }

    /// Whether `next` should take the first queue entry because a blend
    /// already chose it; consumes the reservation.
    pub(crate) fn take_reserved_next(&self) -> bool {
        self.dj.reserved.replace(false)
    }

    /// The prepared stream for `track_id`, consumed.
    pub(crate) fn take_prepared_stream(
        &self,
        track_id: &str,
    ) -> Option<(SearchResult, PreparedStream)> {
        let mut slot = self.dj.prepared.borrow_mut();
        if slot
            .as_ref()
            .is_some_and(|prepared| prepared.track_id == track_id)
        {
            return slot
                .take()
                .map(|prepared| (prepared.media, prepared.stream));
        }
        None
    }

    /// Cancel any stream opened or opening for a blend.
    pub(crate) fn discard_prepared_stream(&self) {
        if let Some((_, cancel)) = self.dj.preparing.borrow_mut().take() {
            cancel.cancel();
        }
        if let Some(prepared) = self.dj.prepared.borrow_mut().take() {
            prepared.cancel.cancel();
        }
    }

    fn stream_prepared_for(&self, track_id: &str) -> bool {
        self.dj
            .prepared
            .borrow()
            .as_ref()
            .is_some_and(|prepared| prepared.track_id == track_id)
    }

    /// What `next(true)` will play. With shuffle on, the pick is made now and
    /// moved to the front of the queue so the blend and `next` agree.
    fn peek_next(&self) -> Option<Track> {
        let length = self.queue.peek().len();
        if length > 0 && *self.shuffle.peek() && !self.dj.reserved.get() {
            let index = rand::random_range(0..length);
            let mut queue = self.queue;
            let mut items = queue.write();
            let pick = items.remove(index);
            items.insert(0, pick);
            self.dj.reserved.set(true);
        }
        self.upcoming_track()
    }

    /// What `next(true)` would play, without making a shuffled pick: `None`
    /// while shuffle has not reserved one yet, and while one song repeats.
    pub(crate) fn upcoming_track(&self) -> Option<Track> {
        if *self.repeat.peek() == 2 {
            return None;
        }
        if self.queue.peek().is_empty() {
            return if *self.repeat.peek() == 0 {
                self.peek_autoplay()
            } else {
                None
            };
        }
        if *self.shuffle.peek() && !self.dj.reserved.get() {
            return None;
        }
        self.queue.peek().first().cloned()
    }

    fn prepare_stream_for(&self, track: &Track) {
        let preparing = self
            .dj
            .preparing
            .borrow()
            .as_ref()
            .is_some_and(|(id, _)| *id == track.id);
        if preparing || self.stream_prepared_for(&track.id) {
            return;
        }
        self.discard_prepared_stream();
        let cancel = CancellationToken::new();
        *self.dj.preparing.borrow_mut() = Some((track.id.clone(), cancel.clone()));
        let state = self.clone();
        let track = track.clone();
        dioxus::core::spawn_forever(async move {
            let result = async {
                let youtube = state.services.youtube_client().map_err(|error| error.to_string())?;
                let resolved = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err("cancelled".to_owned()),
                    result = youtube.resolve(&track.source_url, StreamKind::Audio) => result.map_err(|error| error.to_string())?,
                };
                let media = resolved.media.clone();
                let audio = state.services.audio.as_ref().map_err(Clone::clone)?;
                let stream = audio
                    .prepare_stream(resolved, cancel.clone())
                    .await
                    .map_err(|error| error.to_string())?;
                Ok::<_, String>((media, stream))
            }
            .await;
            let wanted = state
                .dj
                .preparing
                .borrow()
                .as_ref()
                .is_some_and(|(id, _)| *id == track.id);
            if !wanted || cancel.is_cancelled() {
                return;
            }
            state.dj.preparing.borrow_mut().take();
            match result {
                Ok((media, stream)) => {
                    *state.dj.prepared.borrow_mut() = Some(PreparedNext {
                        track_id: track.id.clone(),
                        media,
                        stream,
                        cancel,
                    });
                }
                Err(error) => crate::native::record_log(
                    &state.services.profile,
                    &format!("A DJ transition could not prepare the next song: {error}"),
                ),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{remote_track, TestApp};

    fn local_audio(id: &str) -> Track {
        Track {
            id: id.into(),
            media_type: "audio".into(),
            path: std::path::PathBuf::from(format!("/music/{id}.m4a")),
            ..remote_track("aqz-KE-bpKQ", id)
        }
    }

    /// A steady 4/4 song of `bars` bars at `bpm`, as its analysis.
    fn steady(bpm: f32, bars: u32) -> yt_core::TrackAnalysis {
        let rate = 44_100;
        let beat = 60.0 / f64::from(bpm) * f64::from(rate);
        let beats: Vec<u64> = (0..bars * 4)
            .map(|index| (f64::from(rate) * 0.5 + f64::from(index) * beat) as u64)
            .collect();
        let duration_frames = beats[beats.len() - 1] + beat as u64 * 2;
        yt_core::TrackAnalysis {
            analyzer_version: yt_worker::ANALYZER_VERSION,
            sample_rate: rate,
            duration_frames,
            downbeats: (0..bars).map(|bar| bar * 4).collect(),
            beats,
            meter: Some(yt_core::Meter {
                beats_per_bar: 4,
                subdivision: yt_core::Subdivision::Binary,
                confidence: 0.9,
            }),
            tempo_bpm: Some(bpm),
            tempo_stability: 1.0,
            key: None,
            integrated_lufs: Some(-9.0),
            true_peak_dbtp: -1.0,
            short_term_lufs: vec![-9.0; (duration_frames / u64::from(rate) * 2) as usize],
            bar_energy: Vec::new(),
            sections: Vec::new(),
            vocal_db: Vec::new(),
            momentary_lufs: Vec::new(),
        }
    }

    #[tokio::test]
    async fn a_planned_mix_replaces_the_automatic_crossfade() {
        let mut app = TestApp::new();
        app.run(|state| {
            let current = local_audio("current");
            let next = local_audio("next");
            state.current.set(Some(current.clone()));
            state.playing.set(true);
            state.set_dj_mode(true);
            state
                .dj_analysis
                .insert_ready(&current.id, steady(128.0, 96));
            state.dj_analysis.insert_ready(&next.id, steady(126.0, 64));
            let exit = state
                .planned_exit(&next, 0.0)
                .expect("a mix suits the pair");
            assert!(exit > 0.0);
            assert!(matches!(
                state.transition_for(&next, true),
                Transition::Mix(_)
            ));
            // The player's show knows the song that leaves and its beat.
            let scene = state.blend_scene.peek().clone().expect("a scene");
            assert_eq!(scene.from.id, current.id);
            assert_eq!(scene.beat_seconds, Some(60.0 / 128.0));
            assert!(scene.seconds > 0.0);
            assert!(
                matches!(state.transition_for(&next, false), Transition::Crossfade(_)),
                "a skip keeps the short crossfade"
            );
            let faded = state.blend_scene.peek().clone().expect("a scene");
            assert_eq!(faded.beat_seconds, None, "a crossfade shares no beat");
            assert_eq!(faded.seconds, MANUAL_CROSSFADE.as_secs_f32());
            let other = local_audio("other");
            assert!(
                matches!(state.transition_for(&other, true), Transition::Crossfade(_)),
                "the plan belongs to its pair"
            );
            state.reset_blend();
            assert!(matches!(
                state.transition_for(&next, true),
                Transition::Crossfade(_)
            ));
            state.set_dj_mode(false);
            assert_eq!(state.transition_for(&next, true), Transition::Cut);
            assert_eq!(*state.blend_scene.peek(), None, "a cut has no show");
        });
    }

    #[tokio::test]
    async fn vocals_that_arrive_later_plan_the_mix_again() {
        let mut app = TestApp::new();
        app.run(|state| {
            let current = local_audio("current");
            let next = local_audio("next");
            state.current.set(Some(current.clone()));
            state.playing.set(true);
            state.set_dj_mode(true);
            state
                .dj_analysis
                .insert_ready(&current.id, steady(128.0, 96));
            state.dj_analysis.insert_ready(&next.id, steady(128.0, 64));
            assert!(state.planned_exit(&next, 0.0).is_some());
            let planned_for = |state: &AppState| {
                state
                    .dj
                    .planned
                    .borrow()
                    .as_ref()
                    .map(|planned| planned.key.vocals)
            };
            assert_eq!(planned_for(state), Some([false, false]));
            let mut sung = steady(128.0, 96);
            sung.vocal_db = vec![-20.0; sung.short_term_lufs.len()];
            state.dj_analysis.insert_ready(&current.id, sung);
            assert!(state.planned_exit(&next, 0.0).is_some());
            assert_eq!(planned_for(state), Some([true, false]), "planned again");
        });
    }

    #[tokio::test]
    async fn a_mix_whose_stems_cannot_be_had_is_planned_again_without_them() {
        let mut app = TestApp::new();
        app.run(|state| {
            let current = local_audio("current");
            let next = local_audio("next");
            state.current.set(Some(current.clone()));
            state.playing.set(true);
            state.set_dj_mode(true);
            state
                .dj_analysis
                .insert_ready(&current.id, steady(128.0, 96));
            state.dj_analysis.insert_ready(&next.id, steady(128.0, 64));
            assert!(state.planned_exit(&next, 0.0).is_some());
            let cancel = CancellationToken::new();
            let generation = {
                let mut planned = state.dj.planned.borrow_mut();
                let planned = planned.as_mut().unwrap();
                planned.extra = Extra::Preparing(cancel.clone());
                planned.generation
            };
            assert_eq!(
                state.planned_exit(&next, 0.0),
                None,
                "a mix waits for its stems"
            );
            assert!(
                matches!(state.transition_for(&next, true), Transition::Mix(_)),
                "the planned mix is still the pair's"
            );
            state.extra_ready(generation, None);
            assert_eq!(state.planned_exit(&next, 0.0), None);
            assert!(state.dj.extras_refused.get() && state.dj.planned.borrow().is_none());
            assert!(
                state.planned_exit(&next, 0.0).is_some(),
                "planned again without stems"
            );
            state.reset_blend();
            assert!(!state.dj.extras_refused.get());
        });
    }

    /// `steady` with silence for its first `lead` and last `tail` seconds.
    fn with_silence(bpm: f32, bars: u32, lead: f64, tail: f64) -> yt_core::TrackAnalysis {
        let mut analysis = steady(bpm, bars);
        let steps = analysis.short_term_lufs.len();
        let step = f64::from(yt_core::TrackAnalysis::LOUDNESS_STEP_SECONDS);
        analysis.momentary_lufs = (0..steps)
            .map(|index| {
                let at = index as f64 * step;
                if at < lead || at >= steps as f64 * step - tail {
                    yt_core::TrackAnalysis::SILENCE_LUFS
                } else {
                    -9.0
                }
            })
            .collect();
        analysis
    }

    #[test]
    fn blends_end_where_the_music_does_not_where_the_file_does() {
        let analysis = with_silence(120.0, 40, 0.0, 10.0);
        let duration = analysis.seconds(analysis.duration_frames);
        let end = music_end(Some(&analysis), duration);
        assert!((duration - end - 10.0).abs() <= 0.5, "{duration} {end}");
        assert_eq!(music_end(None, duration), duration);
    }

    #[tokio::test]
    async fn an_automatic_crossfade_starts_the_next_song_where_its_sound_starts() {
        let mut app = TestApp::new();
        app.run(|state| {
            let current = local_audio("current");
            let next = local_audio("next");
            state.current.set(Some(current));
            state.playing.set(true);
            state.set_dj_mode(true);
            state.edit_settings(|settings| settings.playback.dj_style = DjStyle::Classic);
            state
                .dj_analysis
                .insert_ready(&next.id, with_silence(120.0, 40, 2.5, 0.0));
            let Transition::Crossfade(fade) = state.transition_for(&next, true) else {
                panic!("an automatic blend crossfades");
            };
            assert_eq!(fade.start(), Duration::from_millis(2_500));
            let Transition::Crossfade(manual) = state.transition_for(&next, false) else {
                panic!("a skip crossfades too");
            };
            assert_eq!(
                manual.start(),
                Duration::ZERO,
                "a song the user picks starts at its start"
            );
        });
    }

    #[tokio::test]
    async fn a_pair_without_a_mix_crossfades_and_is_not_planned_again() {
        let mut app = TestApp::new();
        app.run(|state| {
            let current = local_audio("current");
            let next = local_audio("next");
            state.current.set(Some(current.clone()));
            state.playing.set(true);
            state.set_dj_mode(true);
            assert_eq!(
                state.planned_exit(&next, 0.0),
                None,
                "nothing is analyzed yet"
            );
            state
                .dj_analysis
                .insert_ready(&current.id, steady(128.0, 96));
            state.dj_analysis.insert_ready(&next.id, steady(128.0, 96));
            let late = 60.0 / 128.0 * 4.0 * 95.0;
            assert_eq!(state.planned_exit(&next, late), None, "no exit is left");
            assert!(
                state.dj.planned.borrow().is_some(),
                "the miss is remembered"
            );
            assert!(matches!(
                state.transition_for(&next, true),
                Transition::Crossfade(_)
            ));
        });
    }

    #[test]
    fn every_setting_has_a_spanish_label() {
        let intensity = yt_core::PlaybackPreferences::DJ_INTENSITY;
        assert_eq!(INTENSITY_NAMES.len(), intensity.clone().count());
        assert_eq!(intensity_label(*intensity.start()), "Suave");
        assert_eq!(intensity_label(*intensity.end()), "Enérgica");
        for style in DjStyle::ALL {
            assert!(!style_label(style).0.is_empty());
        }
        for length in MixLength::ALL {
            assert!(length_label(length).1.contains(&length.bars().to_string()));
        }
    }

    #[test]
    fn a_fusion_is_named_and_tagged_as_ai() {
        let lanes = yt_mix::Lanes::unchanged;
        let plan = yt_mix::MixPlan {
            technique: yt_mix::Technique::Fusion,
            outgoing_rate: 44_100,
            incoming_rate: 44_100,
            exit_frame: 44_100 * 60,
            entry_frame: 44_100 * 20,
            speed: 1.0,
            overlap: 8.0,
            outgoing: lanes(),
            incoming: lanes(),
            report: yt_mix::MixReport {
                beat_error_ms: None,
                bass_overlap_seconds: 0.0,
                loudness_step_lu: None,
            },
            fusion: Some(yt_mix::Fusion {
                before: 8.0,
                length: 7.5,
                after: 8.0,
                seam: 0.5,
                strength: yt_mix::FUSION_STRENGTH,
            }),
            vocal_shift: None,
        };
        let name = mix_name(&plan, false);
        assert_eq!(name.label, "Fusión IA");
        assert!(name.ai, "the generative model renders it");
    }

    #[tokio::test]
    async fn transitions_blend_only_between_audio_when_dj_mode_allows_it() {
        let mut app = TestApp::new();
        app.run(|state| {
            let current = local_audio("current");
            let next = local_audio("next");
            state.current.set(Some(current));
            state.playing.set(true);
            assert_eq!(state.transition_for(&next, true), Transition::Cut);
            state.set_dj_mode(true);
            let seconds = state.settings.peek().playback.crossfade_seconds;
            match state.transition_for(&next, true) {
                Transition::Crossfade(fade) => {
                    assert_eq!(fade.duration(), Duration::from_secs(seconds.into()));
                    assert_eq!(fade.curve(), FadeCurve::EqualPower);
                }
                other => panic!("expected an automatic blend, got {other:?}"),
            }
            match state.transition_for(&next, false) {
                Transition::Crossfade(fade) => assert_eq!(fade.duration(), MANUAL_CROSSFADE),
                other => panic!("expected a manual blend, got {other:?}"),
            }
            state.edit_settings(|settings| settings.playback.crossfade_on_skip = false);
            assert_eq!(state.transition_for(&next, false), Transition::Cut);
            let mut video = next.clone();
            video.media_type = "video".into();
            assert_eq!(state.transition_for(&video, true), Transition::Cut);
            assert_eq!(
                state.transition_for(&remote_track("abcdefghijk", "Online"), true),
                Transition::Cut,
                "an online song blends only once its stream is prepared"
            );
            state.playing.set(false);
            assert_eq!(state.transition_for(&next, true), Transition::Cut);
        });
    }

    #[tokio::test]
    async fn a_shuffled_pick_is_reserved_so_the_blend_and_next_agree() {
        let mut app = TestApp::new();
        let tracks: Vec<Track> = ["a", "b", "c", "d"]
            .into_iter()
            .map(|name| app.video_track(name))
            .collect();
        app.run(|state| {
            state.shuffle.set(true);
            state.queue.set(tracks.clone());
            let chosen = state.peek_next().unwrap();
            assert_eq!(state.queue.peek()[0].id, chosen.id);
            assert_eq!(state.peek_next().unwrap().id, chosen.id);
            assert!(state.take_reserved_next());
            assert!(!state.take_reserved_next());
        });
    }

    #[test]
    fn every_preference_curve_maps_to_an_engine_curve() {
        for curve in CrossfadeCurve::ALL {
            let mapped = fade_curve(curve);
            assert_eq!(
                format!("{curve:?}"),
                format!("{mapped:?}"),
                "curves keep their meaning across layers"
            );
        }
    }
}
