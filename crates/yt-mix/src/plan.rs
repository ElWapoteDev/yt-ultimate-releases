//! The planner: from both songs' analyses and the user's settings, where the
//! mix starts, which transition it is and what each deck does.
//!
//! It follows how DJs mix and what the automatic-DJ literature measured
//! (`docs/AI_DJ.md` §5.5). The transition type varies along a Markov chain
//! (Vande Veire and De Bie, 2018). Every phrase line near the outgoing song's
//! end is a possible cue, scored by how strongly both songs start a phrase of
//! their structure there, how quiet the overlapped bars are, whether both
//! songs sing at once, how much of the outgoing song it leaves unheard and how
//! well the beats stay together; the plan is drawn among the best, so the
//! same pair mixes differently every time. The incoming song rises with its
//! bass and treble cut and takes them over on the switch downbeat, so only one
//! bassline and one treble play at a time; tempos are matched within 5 % (86 %
//! of real DJ tempo changes are smaller). The mashup style plays the songs'
//! vocals apart instead: the incoming instrumental rises under the outgoing
//! song with its own vocals muted, the outgoing vocals ride over it (moved a
//! semitone or two when the keys clash), and the incoming vocals take over on
//! a phrase line, or a generative model plays the beat-matched blend of both
//! songs again as one piece (a fusion). Songs that cannot be beat-matched fade
//! where the outgoing song is calm, and a song without a beat grid fades
//! where it stops being audible. Nothing cuts or repeats: every transition
//! blends. The same request, seed included, always gives the same plan.

use crate::{
    dsp::{FILTER_OCTAVES, MIN_EQ_DB},
    evaluate::{evaluate, Candidate},
    fusion::{
        Fusion, FUSION_CLASH_STRENGTH, FUSION_STRENGTH, MAX_FUSION_CONTEXT_SECONDS,
        MAX_FUSION_SECONDS,
    },
    grid::fit_around,
    harmony::{key_distance, vocal_shift, COMPATIBLE_KEYS, MAX_VOCAL_SHIFT},
    Curve, Effects, Lanes, MixReport, PlanError, MAX_BEAT_ERROR_MS,
};
use std::f32::consts::FRAC_PI_2;
use yt_core::{
    BandEnergy, DjStyle, MixLength, PlaybackPreferences, SourceRights, Subdivision, TrackAnalysis,
};

/// Largest change of the incoming song's speed that matches the tempos (±5 %).
pub const MAX_STRETCH: f64 = 0.05;
/// Share of the outgoing song that may go unheard after the mix ends; a
/// double drop cuts its last strong section short on purpose.
const MAX_UNHEARD_END: f64 = 0.25;
const MAX_UNHEARD_DOUBLE_DROP: f64 = 0.45;
/// Bars of the incoming song a mix may skip before its cue.
const MAX_SKIPPED_INTRO_BARS: usize = 16;
/// Shortest fade-in, in bars, and the longest with clashing keys.
const MIN_FADE_BARS: usize = 2;
const MAX_CLASH_FADE_BARS: usize = 4;
/// Length of the swap of bass and treble ending on the switch downbeat, in
/// beats: gradual when the setting is soft, otherwise a quarter beat hidden
/// behind the downbeat's kick.
const SOFT_SWAP_BEATS: f32 = 4.0;
const QUICK_SWAP_BEATS: f32 = 0.25;
/// Cut of the incoming treble while it rises, and of the outgoing treble
/// after the swap, in dB.
const TREBLE_CUT_DB: f32 = -24.0;
/// Cut of the incoming mids while it rises, in dB.
const ENTRY_MID_DB: f32 = -6.0;
/// Shortest fade between songs that cannot be beat-matched, and the length of
/// the fade out of a song without a beat grid, in seconds: shorter would
/// sound like a cut.
const FADE_SECONDS: f32 = 4.0;
/// Bars a fade between songs that cannot be beat-matched may last, longest
/// first; more than one only where one song's overlapped bars are beatless.
const FADE_BARS: [usize; 3] = [4, 2, 1];
/// Bars count as beatless when their low band sits this many dB below the
/// song's average (no kick or bassline), so unmatched beats cannot clash.
const BEATLESS_DB: f32 = 9.0;
/// Length of a cut's ramp, in seconds: short, but long enough not to click.
const CUT_SECONDS: f32 = 0.01;
/// Largest attenuation that matches a louder incoming song to the outgoing
/// one, in dB; a quieter one is never boosted.
const MAX_TRIM_DB: f32 = 6.0;
/// Seconds over which a trimmed incoming song returns to its own level.
const TRIM_RECOVERY_SECONDS: f32 = 8.0;
/// Seconds an outgoing echo or reverb keeps ringing after its last change.
const EFFECT_TAIL_SECONDS: f32 = 3.0;
/// Where the outgoing high-pass goes after the swap, in octaves above the
/// filter floor (320 Hz, then 1.8 kHz at the end of the fade-out).
const EXIT_HIGHPASS: f32 = 4.0;
const EXIT_HIGHPASS_END: f32 = 6.5;
/// Where a clashing-key sweep's outgoing high-pass ends (2.6 kHz).
const SWEEP_HIGHPASS_END: f32 = 7.0;
/// Peak reverb send of the outgoing deck.
const REVERB_SEND: f32 = 0.6;
/// Longest overlap of a plan, in seconds.
const MAX_OVERLAP_SECONDS: f32 = 90.0;
/// Seconds before the exit at which a plan may start changing the outgoing deck.
const MAX_LEAD_SECONDS: f32 = 2.0;
/// Points of an equal-power ramp.
const RAMP_POINTS: usize = 5;
/// Phrase lengths within a section, in bars.
const PHRASE_BARS: usize = 4;
const LONG_PHRASE_BARS: usize = 8;
/// How strongly a bar starts a phrase, from 0 to 1: the start of a section
/// whose energy differs from the one before, the start of a section, and
/// every eight or four bars of a section counted from either of its ends.
const ENERGY_CHANGE: f32 = 1.0;
const SECTION_START: f32 = 0.8;
const LONG_PHRASE: f32 = 0.6;
const SHORT_PHRASE: f32 = 0.4;
/// How well a switch into the outgoing song's calm part suits the section
/// it lands in: a calm one, a strong one (its chorus is cut short), or one
/// of a song whose structure is unknown.
const IN_CALM: f32 = 1.0;
const IN_STRONG: f32 = 0.4;
const IN_UNKNOWN: f32 = 0.6;
/// How well a rolling switch suits a fade-out that ends a phrase or more
/// before the outgoing song's strong section does.
const EARLY_ROLL: f32 = 0.7;
/// Points a cue earns per unit of phrase strength at each song's switch.
const STRUCTURE_POINTS: f32 = 1.0;
/// Decibels by which the overlapped mids (where vocals and leads live) sit
/// below the song's average for one point.
const QUIET_DB_PER_POINT: f32 = 10.0;
/// Points a cue loses in proportion to the share of the outgoing song it
/// leaves unheard, to the bars of intro it skips (per
/// [`MAX_SKIPPED_INTRO_BARS`]), per halving of the fade-in below the
/// setting, to its beat error (per [`MAX_BEAT_ERROR_MS`]) and per LU of
/// loudness step.
const UNHEARD_POINTS: f32 = 2.0;
const SKIP_POINTS: f32 = 1.0;
const SHORTER_POINTS: f32 = 0.3;
const BEAT_POINTS: f32 = 0.5;
const LOUDNESS_POINTS_PER_LU: f32 = 0.05;
/// Spread of the draw among good cues, in points: a cue this much worse than
/// another is drawn e (2.7) times less often.
const DRAW_POINTS: f32 = 0.25;
/// Cues more than this many points below the best are never drawn.
const MAX_DRAW_GAP: f32 = 1.0;
/// Cues built and evaluated per technique: the best ones before evaluation.
const MAX_BUILT: usize = 24;
/// Beat assumed for a song without a beat grid (120 BPM), in seconds.
const UNGRIDDED_BEAT_SECONDS: f32 = 0.5;
/// Seconds a vocal stem covers beyond the frames a plan plays.
pub const STEM_MARGIN_SECONDS: f64 = 0.5;
/// A bar is sung when its vocals reach within this many dB of the song's
/// loud vocals (their 90th percentile), and at least the floor (dBFS RMS).
const VOCAL_RANGE_DB: f32 = 15.0;
const VOCAL_FLOOR_DB: f32 = -40.0;
/// Bars both songs may sing at once; each one more costs points, so clashing
/// cues are drawn only when every cue clashes (Vande Veire and De Bie).
const TOLERATED_CLASH_BARS: usize = 1;
const CLASH_POINTS: f32 = 1.5;
/// Points a cue loses when its vocals must be held back with stems to avoid
/// a clash: a little, so a cue that needs nothing is preferred.
const GUARD_POINTS: f32 = 0.3;
/// Beats over which held-back vocals return.
const VOCAL_RETURN_BEATS: f32 = 1.0;
/// Odds that a hand-over with stems lets the outgoing vocals ride over the
/// incoming instrumental, by technique (relaxed, rolling).
const ACAPELLA_ODDS: [f64; 2] = [0.3, 0.35];
/// Largest change of the incoming song's speed in a mashup (±10 %, where
/// 94.5 % of real DJ tempo changes lie): the vocals ride over the other
/// song's beat, so the tempos must meet. It glides back afterwards.
pub const MASHUP_MAX_STRETCH: f64 = 0.10;
/// Largest change of the incoming song's speed in a fusion (±30 %): with
/// half and double time, any two tempos meet within it (the worst, √2 apart,
/// needs 29 %), so a fusion beat-matches every pair. The model renders the
/// stretched audio again, and the incoming song glides back afterwards.
pub const FUSION_MAX_STRETCH: f64 = 0.30;
/// Share of a mashup's ride the outgoing song must sing.
const MASHUP_SUNG_SHARE: f32 = 0.5;
/// How well a mashup entry suits when the incoming song is not singing where
/// its vocals come in.
const LATE_VOCALS: f32 = 0.3;
/// Points a mashup loses when every incoming bar it plays without its vocals
/// is sung: their lines go unheard, so an instrumental stretch is preferred.
const MUTED_POINTS: f32 = 0.5;
/// Decibels between the outgoing song's level before a mashup's switch and
/// the incoming song's after it for one point, and how much the bass alone
/// counts beside the whole: the backing must not jump or lose its beat.
const STEP_DB_PER_POINT: f32 = 10.0;
const BASS_STEP_WEIGHT: f32 = 0.5;
/// Share of a mashup's ride during which the outgoing instrumental keeps
/// blending with the incoming one, and its level on the switch.
const MASHUP_BLEND_SHARE: f32 = 0.5;
const INSTRUMENTAL_BLEND: f32 = 0.6;
/// Bars before a mashup's switch over which the incoming treble opens from
/// [`TREBLE_CUT_DB`] to this level, so its hats do not jump in on the switch.
const TREBLE_LEAD_BARS: f32 = 2.0;
const TREBLE_LEAD_DB: f32 = -9.0;
/// The incoming high-pass while a mashup whose vocals are moved waits for its
/// switch, in octaves above the filter floor (5.1 kHz): only hats and air,
/// nothing with a pitch that could clash with the outgoing harmony.
const HATS_HIGHPASS: f32 = 8.0;
/// Peak reverb send of the outgoing vocals as they leave a mashup.
const VOCAL_REVERB_SEND: f32 = 0.5;
/// Bars of a fusion by mix length (short, medium, long), halved while longer
/// than [`MAX_FUSION_SECONDS`].
const FUSION_BARS: [usize; 3] = [4, 8, 16];
/// Bars of each song the model hears around a fusion.
const FUSION_CONTEXT_BARS: usize = 4;
/// Beats of each seam between a song and the fusion's rendering.
const FUSION_SEAM_BEATS: f32 = 1.0;
/// How much better an incoming entry on a strong section suits a fusion: the
/// blend builds up to it.
const FUSION_DROP: f32 = 0.3;
/// Points a fusion loses for every bar either song sings during it: the
/// model renders vocals poorly.
const FUSION_SUNG_POINTS: f32 = 0.5;

/// What the planner needs to join two songs.
#[derive(Debug, Clone, Copy)]
pub struct MixRequest<'a> {
    pub outgoing: &'a TrackAnalysis,
    pub incoming: &'a TrackAnalysis,
    pub length: MixLength,
    /// `PlaybackPreferences::DJ_INTENSITY`; larger values count as the top.
    pub intensity: u8,
    /// What both songs' sources allow together (`SourceRights::with`).
    pub rights: SourceRights,
    /// Earliest frame of the outgoing song at which the mix may start: its
    /// position plus the time the application needs to arm it.
    pub earliest_exit: u64,
    /// The previous mix's technique, which shapes the odds of this one.
    pub previous: Option<Technique>,
    /// Draws among the good plans; the application passes a new one for every
    /// transition, so the same pair mixes differently each time.
    pub seed: u64,
    /// The application can separate both songs' vocals in time for this mix.
    /// With `rights.separate`, the planner may then hold one song's vocals
    /// back or let them ride over the other's instrumental.
    pub separation: bool,
    /// The DJ style: the mashup one prefers [`Technique::Mashup`] or
    /// [`Technique::Fusion`] wherever the songs allow them.
    pub style: DjStyle,
    /// The application can render a fusion of both songs in time for this
    /// mix. With `rights.generate`, the mashup style may then plan one.
    pub fusing: bool,
    /// Fuse the pair first whatever its tempos and keys
    /// (`PlaybackPreferences::dj_always_fuse`); otherwise only a pair with
    /// compatible keys and tempos within [`MASHUP_MAX_STRETCH`] fuses, and
    /// the seed draws whether a mashup goes first.
    pub always_fuse: bool,
}

/// How a mix joins the songs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Technique {
    /// The incoming intro rises under the outgoing song and takes over where
    /// it turns calm (its outro or a breakdown).
    Relaxed,
    /// The incoming song builds up under the outgoing song's strong section
    /// and takes over on its own drop.
    Rolling,
    /// Both songs reach a drop together.
    DoubleDrop,
    /// A beat-matched hand-over through sweeping filters, for clashing keys.
    FilterSweep,
    /// A short fade where the outgoing song is calm, for songs that cannot be
    /// beat-matched.
    Fade,
    /// A mashup played by stems: the incoming instrumental rises under the
    /// outgoing song with its own vocals muted, the instrumentals swap on the
    /// switch downbeat, the outgoing vocals ride over the incoming
    /// instrumental, and the incoming vocals take over on a phrase line.
    /// Needs both songs' vocals measured and separated, tempos within
    /// [`MASHUP_MAX_STRETCH`], and keys that are compatible or become so by
    /// moving the outgoing vocals ([`MixPlan::vocal_shift`]).
    Mashup,
    /// An AI fusion ([`MixPlan::fusion`]): from a phrase line of the outgoing
    /// song, both songs blend beat-matched and a generative model plays the
    /// blend again as one piece, landing on the incoming song at its entry.
    /// Needs the right to generate, a beat grid in both songs, compatible keys
    /// and tempos within [`MASHUP_MAX_STRETCH`]; with
    /// [`MixRequest::always_fuse`], any keys, meters and tempos within
    /// [`FUSION_MAX_STRETCH`] (with half and double time).
    Fusion,
}

/// A move of the outgoing song's separated vocals to the incoming song's
/// key, so they ride over its instrumental although the keys clash.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VocalShift {
    /// Semitones the vocals move (negative: down), within
    /// [`MAX_VOCAL_SHIFT`] and never 0.
    pub semitones: i8,
    /// Seconds since the exit from which they sound moved: the switch, where
    /// the outgoing instrumental has left.
    pub from: f32,
}

/// A planned mix. Frames are at each song's analysis rate, which is the rate
/// the engine decodes it at; times are seconds since the exit.
#[derive(Debug, Clone, PartialEq)]
pub struct MixPlan {
    pub technique: Technique,
    pub outgoing_rate: u32,
    pub incoming_rate: u32,
    /// Frame of the outgoing song at which the mix starts: a downbeat, unless
    /// the song has no beat grid.
    pub exit_frame: u64,
    /// Frame of the incoming song heard at that moment: a downbeat, unless
    /// the song has no beat grid.
    pub entry_frame: u64,
    /// Speed of the incoming song while the songs overlap (1 when its tempo
    /// is not matched); the engine returns it to normal afterwards.
    pub speed: f32,
    /// Seconds the songs overlap.
    pub overlap: f32,
    pub outgoing: Lanes,
    pub incoming: Lanes,
    pub report: MixReport,
    /// The fusion a [`Technique::Fusion`] plays between the songs: the
    /// incoming deck starts `length` seconds after the exit.
    pub fusion: Option<Fusion>,
    /// The move of the outgoing vocals of a [`Technique::Mashup`] between
    /// songs whose keys clash; the engine plays them from a stem moved ahead
    /// of time.
    pub vocal_shift: Option<VocalShift>,
}

impl MixPlan {
    /// Seconds before the exit at which the outgoing deck starts changing.
    pub fn lead(&self) -> f32 {
        (-self.outgoing.span().0).max(0.0)
    }

    /// Seconds after the exit at which both decks are back to normal,
    /// including the outgoing echo or reverb ringing out.
    pub fn end(&self) -> f32 {
        let tail = if self.outgoing.effects.rings() {
            EFFECT_TAIL_SECONDS
        } else {
            0.0
        };
        (self.outgoing.span().1 + tail)
            .max(self.incoming.span().1)
            .max(self.overlap)
    }

    /// Whether playing the plan needs both songs' vocal stems.
    pub fn uses_stems(&self) -> bool {
        self.outgoing.uses_stems() || self.incoming.uses_stems()
    }

    /// The source frames each song's vocal stem must cover, outgoing then
    /// incoming: every frame either deck plays while its lanes change, plus
    /// [`STEM_MARGIN_SECONDS`]. `None` when the plan uses no stems.
    pub fn stem_frames(&self) -> Option<[std::ops::Range<u64>; 2]> {
        if !self.uses_stems() {
            return None;
        }
        let end = f64::from(self.end()) + STEM_MARGIN_SECONDS;
        let outgoing_rate = f64::from(self.outgoing_rate);
        let lead = (f64::from(self.lead()) + STEM_MARGIN_SECONDS) * outgoing_rate;
        let outgoing = self.exit_frame.saturating_sub(lead as u64)
            ..self.exit_frame + (end * outgoing_rate) as u64;
        let played = end * f64::from(self.speed) * f64::from(self.incoming_rate);
        let incoming = self.entry_frame..self.entry_frame + played as u64;
        Some([outgoing, incoming])
    }

    /// The outgoing song's frame from which its vocals sound moved, when the
    /// plan moves them.
    pub fn vocal_shift_frame(&self) -> Option<u64> {
        self.vocal_shift.map(|shift| {
            self.exit_frame + (f64::from(shift.from) * f64::from(self.outgoing_rate)) as u64
        })
    }

    /// `Err` when a value leaves its bounds.
    pub fn validate(&self) -> Result<(), PlanError> {
        let rates = TrackAnalysis::SAMPLE_RATE;
        if !rates.contains(&self.outgoing_rate) || !rates.contains(&self.incoming_rate) {
            return Err(PlanError::Plan("a sample rate is out of range"));
        }
        let stretch = match self.technique {
            Technique::Fusion => FUSION_MAX_STRETCH,
            Technique::Mashup => MASHUP_MAX_STRETCH,
            _ => MAX_STRETCH,
        };
        if (self.technique == Technique::Fusion) != self.fusion.is_some() {
            return Err(PlanError::Plan("only a fusion technique carries a fusion"));
        }
        if let Some(fusion) = &self.fusion {
            fusion.validate()?;
        }
        if let Some(shift) = self.vocal_shift {
            // Moved vocals never sound with their own instrumental.
            let valid = self.technique == Technique::Mashup
                && shift.semitones != 0
                && shift.semitones.abs() <= MAX_VOCAL_SHIFT
                && shift.from.is_finite()
                && (0.0..=self.overlap).contains(&shift.from)
                && self.outgoing.instrumental.at(shift.from) == 0.0
                && self.outgoing.instrumental.at(f32::MAX) == 0.0;
            if !valid {
                return Err(PlanError::Plan("the vocal shift is out of range"));
            }
        }
        if !self.speed.is_finite() || (f64::from(self.speed) - 1.0).abs() > stretch + 1e-6 {
            return Err(PlanError::Plan("the speed change is too large"));
        }
        if !self.overlap.is_finite() || self.overlap <= 0.0 || self.overlap > MAX_OVERLAP_SECONDS {
            return Err(PlanError::Plan("the overlap is out of range"));
        }
        self.outgoing.validate()?;
        self.incoming.validate()?;
        if self.lead() > MAX_LEAD_SECONDS
            || self.end()
                > self.overlap + TRIM_RECOVERY_SECONDS + EFFECT_TAIL_SECONDS + MAX_LEAD_SECONDS
        {
            return Err(PlanError::Plan("a lane changes outside the mix"));
        }
        Ok(())
    }
}

/// A mix for `request`, drawn among the best by its seed, or `None` when the
/// classic crossfade is better: the sources forbid mixing, no cue is left,
/// or no candidate passes the evaluation.
pub fn plan(request: &MixRequest<'_>) -> Option<MixPlan> {
    if !request.rights.mix {
        return None;
    }
    let intensity = request
        .intensity
        .min(*PlaybackPreferences::DJ_INTENSITY.end());
    let mut random = SplitMix(request.seed);
    let Some(outgoing) = Song::new(request.outgoing) else {
        return loudness_fade(request, &mut random);
    };
    let incoming = Song::new(request.incoming);
    let matched_within = |stretch| {
        incoming
            .as_ref()
            .and_then(|incoming| matched(&outgoing, incoming, stretch))
    };
    let matched = matched_within(MAX_STRETCH);
    let mashup_match = matched_within(MASHUP_MAX_STRETCH);
    let keys = request.outgoing.key.zip(request.incoming.key);
    let clash = keys.is_some_and(|(a, b)| key_distance(a, b) > COMPATIBLE_KEYS);
    // A mashup's outgoing vocals ride over the incoming instrumental: where
    // the keys clash, they are moved to fit it when a semitone or two does.
    let shift = keys.map_or(Some(0), |(vocals, instrumental)| {
        vocal_shift(vocals, instrumental)
    });
    let sung = |song: Option<&Song<'_>>| song.is_some_and(|song| !song.sung.is_empty());
    let mashup = request.style == DjStyle::Mashup
        && stems_allowed(Technique::Mashup, request)
        && shift.is_some()
        && mashup_match.is_some()
        && sung(Some(&outgoing))
        && sung(incoming.as_ref());
    // Asked to fuse always, neither tempo nor key keeps a pair from fusing:
    // the incoming song is stretched to the outgoing beat, and clashing keys
    // leave the model more freedom to play the two harmonies as one.
    let fusion_match = if request.always_fuse {
        incoming
            .as_ref()
            .and_then(|incoming| tempo_match(&outgoing, incoming, FUSION_MAX_STRETCH))
    } else {
        mashup_match.filter(|_| !clash)
    };
    let fusion = request.style == DjStyle::Mashup
        && request.fusing
        && request.rights.generate
        && fusion_match.is_some();
    let mut order = techniques(
        matched.is_some(),
        clash,
        intensity,
        request.previous,
        &mut random,
    );
    // The AI style tries its own techniques first: a fusion before anything
    // when asked to fuse always, otherwise in a drawn order when both suit
    // the pair, so it does not always join them the same way.
    let mut ai: Vec<Technique> = [(fusion, Technique::Fusion), (mashup, Technique::Mashup)]
        .into_iter()
        .filter_map(|(allowed, technique)| allowed.then_some(technique))
        .collect();
    if !request.always_fuse && ai.len() == 2 && random.unit() < 0.5 {
        ai.reverse();
    }
    order.splice(0..0, ai);
    // Cues where both songs sing at once are kept only for when every
    // technique's cues do.
    let mut clashing: Vec<(MixPlan, f32)> = Vec::new();
    for technique in order {
        if technique == Technique::Fusion {
            let strength = if clash {
                FUSION_CLASH_STRENGTH
            } else {
                FUSION_STRENGTH
            };
            let fused = incoming
                .as_ref()
                .zip(fusion_match)
                .and_then(|(incoming, matched)| {
                    plan_fusion(&outgoing, incoming, matched, strength, request, &mut random)
                });
            match fused {
                Some(plan) => return Some(plan),
                None => continue,
            }
        }
        let mut found = cues(technique, &outgoing, incoming.as_ref(), request);
        found.sort_by(|a, b| b.fit.total_cmp(&a.fit));
        found.truncate(MAX_BUILT);
        let (clear, clash): (Vec<Cue>, Vec<Cue>) =
            found.into_iter().partition(|cue| cue.clash == 0);
        let found = if clear.is_empty() { clash } else { clear };
        let candidates: Vec<(MixPlan, f32)> = found
            .iter()
            .copied()
            .filter_map(|cue| {
                let stems = stems_allowed(technique, request);
                let moved = shift.unwrap_or(0);
                let shape = Shape::pick(technique, intensity, stems, moved, &mut random);
                let matched = if technique == Technique::Mashup {
                    mashup_match
                } else {
                    matched
                };
                let plan = build(
                    technique,
                    &outgoing,
                    incoming.as_ref(),
                    matched,
                    cue,
                    shape,
                    request,
                )?;
                let score = cue.fit - penalty(&plan.report);
                plan.report
                    .passes(outgoing.beat as f32)
                    .then_some((plan, score))
            })
            .collect();
        if found.first().is_some_and(|cue| cue.clash > 0) {
            if clashing.is_empty() {
                clashing = candidates;
            }
            continue;
        }
        if let Some(plan) = draw(candidates, &mut random) {
            return Some(plan);
        }
    }
    draw(clashing, &mut random)
}

/// The beat grid and structure of a song.
pub(crate) struct Song<'a> {
    pub analysis: &'a TrackAnalysis,
    pub rate: f64,
    /// Frames of the downbeats, and which of the song's beats each one is.
    pub bars: Vec<u64>,
    bar_beats: Vec<usize>,
    /// Seconds per beat.
    pub beat: f64,
    beats_per_bar: u8,
    subdivision: Subdivision,
    /// Frame where the song stops being audible ([`audible_end`]).
    audible_end: u64,
    /// Mean mid-band and low-band levels of its bars, in dB.
    mean_mid: f32,
    mean_low: f32,
    /// Whether each bar is sung; empty until the vocals are measured.
    sung: Vec<bool>,
}

impl<'a> Song<'a> {
    /// The grid of `analysis`, when it has a tempo, a meter and two bars.
    fn new(analysis: &'a TrackAnalysis) -> Option<Self> {
        let tempo = analysis.tempo_bpm?;
        let meter = analysis.meter?;
        let (bar_beats, bars): (Vec<usize>, Vec<u64>) = analysis
            .downbeats
            .iter()
            .filter_map(|index| {
                let beat = *index as usize;
                Some((beat, *analysis.beats.get(beat)?))
            })
            .unzip();
        let mut song = Self {
            analysis,
            rate: f64::from(analysis.sample_rate),
            beat: 60.0 / f64::from(tempo),
            beats_per_bar: meter.beats_per_bar,
            subdivision: meter.subdivision,
            audible_end: analysis.audible_end(),
            mean_mid: 0.0,
            mean_low: 0.0,
            sung: Vec::new(),
            bars,
            bar_beats,
        };
        song.sung = sung(analysis, &song.bars);
        song.mean_mid = song
            .level(0, song.bars.len(), |bar| bar.mid_db)
            .unwrap_or(0.0);
        song.mean_low = song
            .level(0, song.bars.len(), |bar| bar.low_db)
            .unwrap_or(0.0);
        (song.bars.len() >= 2 && tempo > 0.0).then_some(song)
    }

    /// Index of the section that bar `bar` belongs to.
    fn section_of(&self, bar: usize) -> Option<usize> {
        self.analysis
            .sections
            .iter()
            .rposition(|section| section.start_bar as usize <= bar)
    }

    /// Whether bar `bar` lies in a strong section; `None` when the song's
    /// structure is unknown.
    fn strong_at(&self, bar: usize) -> Option<bool> {
        self.section_of(bar)
            .map(|index| self.analysis.sections[index].high_energy)
    }

    /// How strongly bar `bar` starts a phrase, from 0 to 1 ([`ENERGY_CHANGE`]
    /// to [`SHORT_PHRASE`]); 0 inside a phrase. Without a known structure the
    /// whole song is one section.
    fn phrase(&self, bar: usize) -> f32 {
        let sections = &self.analysis.sections;
        let index = self.section_of(bar);
        let start = index.map_or(0, |index| sections[index].start_bar as usize);
        let next = index
            .and_then(|index| sections.get(index + 1))
            .map(|section| section.start_bar as usize);
        if let Some(index) = index.filter(|_| bar == start) {
            let changes = index.checked_sub(1).is_some_and(|previous| {
                sections[previous].high_energy != sections[index].high_energy
            });
            return if changes {
                ENERGY_CHANGE
            } else {
                SECTION_START
            };
        }
        let on = |length: usize| {
            (bar - start).is_multiple_of(length)
                || next.is_some_and(|next| (next - bar).is_multiple_of(length))
        };
        if on(LONG_PHRASE_BARS) {
            LONG_PHRASE
        } else if on(PHRASE_BARS) {
            SHORT_PHRASE
        } else {
            0.0
        }
    }

    /// How well a switch at bar `bar` hands over into the song's calm part
    /// (relaxed hand-overs, sweeps and fades).
    fn calm_switch(&self, bar: usize) -> f32 {
        let place = match self.strong_at(bar) {
            Some(false) => IN_CALM,
            Some(true) => IN_STRONG,
            None => IN_UNKNOWN,
        };
        self.phrase(bar) * place
    }

    /// Bars where the song goes from a strong section to a calm one (the
    /// start of a breakdown or the outro).
    fn calm_starts(&self) -> Vec<usize> {
        self.analysis
            .sections
            .windows(2)
            .filter(|pair| pair[0].high_energy && !pair[1].high_energy)
            .map(|pair| pair[1].start_bar as usize)
            .collect()
    }

    /// Bars where the song goes from a calm section to a strong one: its drops.
    fn drops(&self) -> Vec<usize> {
        self.analysis
            .sections
            .windows(2)
            .filter(|pair| !pair[0].high_energy && pair[1].high_energy)
            .map(|pair| pair[1].start_bar as usize)
            .collect()
    }

    /// Seconds from bar `from` to bar `to`.
    fn seconds_between(&self, from: usize, to: usize) -> f32 {
        let (Some(start), Some(end)) = (self.bars.get(from), self.bars.get(to)) else {
            return 0.0;
        };
        (end.saturating_sub(*start) as f64 / self.rate) as f32
    }

    /// Mean level of one band over bars `from..to`, in dB; `None` without
    /// band levels for them.
    fn level(&self, from: usize, to: usize, band: fn(&BandEnergy) -> f32) -> Option<f32> {
        let energy = &self.analysis.bar_energy;
        let bars = energy.get(from.min(energy.len())..to.min(energy.len()))?;
        (!bars.is_empty()).then(|| bars.iter().map(band).sum::<f32>() / bars.len() as f32)
    }

    /// How much quieter than the song's average the mids of bars `from..to`
    /// are (where vocals and leads live), in points.
    fn quietness(&self, from: usize, to: usize) -> f32 {
        self.level(from, to, |bar| bar.mid_db)
            .map_or(0.0, |mid| (self.mean_mid - mid) / QUIET_DB_PER_POINT)
    }

    /// Mean level of bars `from..to` over the whole spectrum, in dB; `None`
    /// without band levels for them.
    fn loudness(&self, from: usize, to: usize) -> Option<f32> {
        self.level(from, to, |bar| {
            let power = |db: f32| 10_f32.powf(db / 10.0);
            10.0 * (power(bar.low_db) + power(bar.mid_db) + power(bar.high_db)).log10()
        })
    }

    /// How far the level of bars `after` of `next`, attenuated by `trim_db`,
    /// lies from this song's level over bars `before`, in points: the whole
    /// spectrum, and the bass alone by [`BASS_STEP_WEIGHT`]. 0 without band
    /// levels.
    fn step_into(
        &self,
        before: std::ops::Range<usize>,
        next: &Song<'_>,
        after: std::ops::Range<usize>,
        trim_db: f32,
    ) -> f32 {
        let whole = self
            .loudness(before.start, before.end)
            .zip(next.loudness(after.start, after.end));
        let bass = self
            .level(before.start, before.end, |bar| bar.low_db)
            .zip(next.level(after.start, after.end, |bar| bar.low_db));
        let apart = |levels: Option<(f32, f32)>| {
            levels.map_or(0.0, |(from, to)| (from - (to + trim_db)).abs())
        };
        (apart(whole) + BASS_STEP_WEIGHT * apart(bass)) / STEP_DB_PER_POINT
    }

    /// Share of bars `from..to` the song sings; 0 when its vocals are not
    /// measured.
    fn sung_share(&self, from: usize, to: usize) -> f32 {
        let bars = self
            .sung
            .get(from.min(self.sung.len())..to.min(self.sung.len()));
        match bars {
            Some(bars) if !bars.is_empty() && to > from => {
                bars.iter().filter(|sung| **sung).count() as f32 / (to - from) as f32
            }
            _ => 0.0,
        }
    }

    /// Whether bars `from..to` have no kick or bassline to clash ([`BEATLESS_DB`]).
    fn beatless(&self, from: usize, to: usize) -> bool {
        self.level(from, to, |bar| bar.low_db)
            .is_some_and(|low| low <= self.mean_low - BEATLESS_DB)
    }
}

/// Whether each bar from one of `bars` (downbeat frames) to the next is sung:
/// the power mean of its vocal levels reaches [`VOCAL_RANGE_DB`] of the
/// song's loud vocals and [`VOCAL_FLOOR_DB`]. Empty without vocal levels.
fn sung(analysis: &TrackAnalysis, bars: &[u64]) -> Vec<bool> {
    let levels = &analysis.vocal_db;
    if levels.is_empty() {
        return Vec::new();
    }
    let mut sorted = levels.clone();
    sorted.sort_by(f32::total_cmp);
    let loud = sorted[(sorted.len() - 1) * 9 / 10];
    let threshold = (loud - VOCAL_RANGE_DB).max(VOCAL_FLOOR_DB);
    let step = f64::from(TrackAnalysis::LOUDNESS_STEP_SECONDS) * f64::from(analysis.sample_rate);
    bars.windows(2)
        .map(|bar| {
            let first = (bar[0] as f64 / step) as usize;
            let last = ((bar[1] as f64 / step).ceil() as usize).max(first + 1);
            let steps = levels.get(first.min(levels.len())..last.min(levels.len()));
            steps.is_some_and(|steps| {
                !steps.is_empty() && {
                    let power = steps.iter().map(|db| 10_f32.powf(db / 10.0)).sum::<f32>();
                    10.0 * (power / steps.len() as f32).log10() >= threshold
                }
            })
        })
        .collect()
}

/// Bars both songs sing at once when outgoing bar `exit` meets incoming bar
/// `entry` and they overlap for `bars`; 0 when either song's vocals are not
/// measured.
fn clashing_bars(
    outgoing: &Song<'_>,
    incoming: Option<&Song<'_>>,
    exit: usize,
    entry: usize,
    bars: usize,
) -> usize {
    let Some(incoming) = incoming else {
        return 0;
    };
    (0..bars)
        .filter(|bar| {
            outgoing.sung.get(exit + bar).copied().unwrap_or(false)
                && incoming.sung.get(entry + bar).copied().unwrap_or(false)
        })
        .count()
}

/// How the incoming song's tempo meets the outgoing one: its speed, and how
/// many outgoing beats one of its beats lasts (1, or 2 and ½ for half- and
/// double-time).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Match {
    pub speed: f64,
    pub beats_per_beat: f64,
}

/// The tempo match that changes the incoming speed least, within `stretch`
/// ([`MAX_STRETCH`], or [`MASHUP_MAX_STRETCH`] for a mashup); `None` for
/// different meters.
fn matched(outgoing: &Song<'_>, incoming: &Song<'_>, stretch: f64) -> Option<Match> {
    if outgoing.beats_per_bar != incoming.beats_per_bar
        || outgoing.subdivision != incoming.subdivision
    {
        return None;
    }
    tempo_match(outgoing, incoming, stretch)
}

/// The match of the beats alone, whatever the meters: for a fusion, whose
/// model renders the blend again as one piece.
fn tempo_match(outgoing: &Song<'_>, incoming: &Song<'_>, stretch: f64) -> Option<Match> {
    [1.0, 2.0, 0.5]
        .into_iter()
        .map(|beats_per_beat| Match {
            speed: incoming.beat / (outgoing.beat * beats_per_beat),
            beats_per_beat,
        })
        .filter(|candidate| (candidate.speed - 1.0).abs() <= stretch)
        .min_by(|a, b| (a.speed - 1.0).abs().total_cmp(&(b.speed - 1.0).abs()))
}

/// The incoming speed and entry frame that keep the songs' beats together
/// from outgoing bar `exit` and incoming bar `entry` over `beats` outgoing
/// beats, from straight grids fitted to the beats around them
/// ([`fit_around`]): the detected beats sit on the analyzer's frames, up to
/// 10 ms from where they sound, and the songs' tempos there may differ from
/// their overall ones. `None` when either song's beats there are irregular.
fn align(
    outgoing: &Song<'_>,
    incoming: &Song<'_>,
    matched: Match,
    (exit, entry): (usize, usize),
    beats: usize,
) -> Option<(f64, u64)> {
    let exit_beat = *outgoing.bar_beats.get(exit)?;
    let entry_beat = *incoming.bar_beats.get(entry)?;
    let ours = fit_around(&outgoing.analysis.beats, exit_beat, beats)?;
    let incoming_beats = (beats as f64 / matched.beats_per_beat).round() as usize;
    let theirs = fit_around(&incoming.analysis.beats, entry_beat, incoming_beats)?;
    let speed =
        (theirs.period / incoming.rate) / (ours.period / outgoing.rate * matched.beats_per_beat);
    // The mix starts on the detected exit: the incoming song enters as far
    // from its own grid as that frame lies from the outgoing one.
    let late = (*outgoing.bars.get(exit)? as f64 - ours.origin) / outgoing.rate;
    let entry_frame = theirs.origin + late * speed * incoming.rate;
    (speed.is_finite() && entry_frame >= 0.0).then(|| (speed, entry_frame.round() as u64))
}

/// A small deterministic generator (SplitMix64) for the seeded choices.
struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A number in [0, 1).
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1_u64 << 53) as f64
    }
}

/// The transitions to try, in order. Beat-matched songs draw the first from
/// a Markov chain over relaxed, rolling and double-drop transitions (Vande
/// Veire and De Bie's matrix, weighted by intensity: soft settings favour
/// relaxed hand-overs, energetic ones double drops), then try the others.
/// Songs that cannot be beat-matched fade.
fn techniques(
    matched: bool,
    clash: bool,
    intensity: u8,
    previous: Option<Technique>,
    random: &mut SplitMix,
) -> Vec<Technique> {
    use Technique::*;
    if !matched {
        return vec![Fade];
    }
    if clash {
        return vec![FilterSweep, Fade];
    }
    let row: [f64; 3] = match previous {
        Some(Rolling | DoubleDrop) => [0.3, 0.7, 0.0],
        Some(Relaxed) | None => [0.2, 0.6, 0.2],
        Some(_) => [0.4, 0.6, 0.0],
    };
    let bias: [f64; 3] = match intensity {
        0 | 1 => [3.0, 1.0, 0.0],
        2 => [1.0, 1.0, 0.0],
        _ => [0.5, 1.0, 3.0],
    };
    let mut weights: Vec<(Technique, f64)> = [Relaxed, Rolling, DoubleDrop]
        .into_iter()
        .zip(row.iter().zip(bias).map(|(odds, bias)| odds * bias))
        .collect();
    let total: f64 = weights.iter().map(|(_, weight)| weight).sum();
    let mut draw = random.unit() * total;
    let first = weights
        .iter()
        .position(|(_, weight)| {
            draw -= weight;
            draw < 0.0
        })
        .unwrap_or(0);
    let mut order = vec![weights.remove(first).0];
    weights.sort_by(|a, b| b.1.total_cmp(&a.1));
    order.extend(weights.into_iter().map(|(technique, _)| technique));
    order.extend([FilterSweep, Fade]);
    order
}

/// Fade lengths to try for a technique, in bars, longest first: half of the
/// bars the setting lets both songs sound together (the other half being the
/// fade-out, or for a mashup the ride), then halved down to
/// [`MIN_FADE_BARS`]; [`FADE_BARS`] for fades between unmatched songs.
fn fades(technique: Technique, length: MixLength) -> Vec<usize> {
    let requested = usize::from(length.bars()) / 2;
    let longest = match technique {
        Technique::FilterSweep => requested.min(MAX_CLASH_FADE_BARS),
        Technique::Fade => return FADE_BARS.to_vec(),
        _ => requested,
    };
    std::iter::successors(Some(longest.max(MIN_FADE_BARS)), |bars| {
        (bars / 2 >= MIN_FADE_BARS).then_some(bars / 2)
    })
    .collect()
}

/// A possible cue of a mix.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Cue {
    /// Outgoing bar where the incoming song starts.
    exit: usize,
    /// Incoming bar it starts on.
    entry: usize,
    /// Bars of the fade ([`around`] says how they fall around the switch).
    fade: usize,
    /// How good the cue is before the evaluation, in points.
    fit: f32,
    /// Bars both songs sing at once beyond [`TOLERATED_CLASH_BARS`].
    clash: usize,
    /// The clash is avoided by holding the vocals back with stems.
    guard: bool,
}

/// Every cue for `technique` that fits the outgoing song's end, with its
/// points. Where each song switches (the outgoing bar `exit + fade` and the
/// incoming bar `entry + fade`) must start a phrase; which phrases suit
/// depends on the technique: relaxed hand-overs, sweeps and fades into the
/// outgoing song's calm part from the incoming intro, rolling ones inside its
/// strong part onto the incoming drop, double drops from drop to drop, and
/// mashups where the outgoing song sings, over a stretch of the incoming one
/// as loud as the outgoing song was, whose vocals come in on a phrase line.
fn cues(
    technique: Technique,
    outgoing: &Song<'_>,
    incoming: Option<&Song<'_>>,
    request: &MixRequest<'_>,
) -> Vec<Cue> {
    let unheard_limit = if technique == Technique::DoubleDrop {
        MAX_UNHEARD_DOUBLE_DROP
    } else {
        MAX_UNHEARD_END
    };
    let audible = outgoing.audible_end.max(1) as f64;
    let stems = stems_allowed(technique, request);
    let shortest_fade = (f64::from(FADE_SECONDS) * outgoing.rate) as u64;
    let fades = fades(technique, request.length);
    let longest = fades.first().copied().unwrap_or(0).max(1);
    let trim_db = trim_db(request.outgoing, request.incoming);
    let mut found = Vec::new();
    for fade in fades {
        let (before, after) = around(technique, fade);
        let shorter = (longest as f32 / fade.max(1) as f32).log2() * SHORTER_POINTS;
        for (switch, outgoing_fit) in switches(technique, outgoing, after) {
            let Some(exit) = switch.checked_sub(before) else {
                continue;
            };
            let end = switch + after;
            let (Some(exit_frame), Some(switch_frame), Some(end_frame)) = (
                outgoing.bars.get(exit),
                outgoing.bars.get(switch),
                outgoing.bars.get(end),
            ) else {
                continue;
            };
            let unheard = (audible - *end_frame as f64).max(0.0) / audible;
            // What the outgoing song must still play as music, never silence:
            // a fade's whole length, a mashup's ride, or up to a hand-over's
            // switch.
            let audible_until = match technique {
                Technique::Fade => (*end_frame).max(exit_frame + shortest_fade),
                Technique::Mashup => *end_frame,
                _ => *switch_frame,
            };
            let fits = *exit_frame >= request.earliest_exit
                && audible_until <= outgoing.audible_end
                && unheard <= unheard_limit;
            if !fits {
                continue;
            }
            let overlap = (before + after).max(1);
            for (entry, incoming_fit) in entries(technique, incoming, before, after) {
                // Unmatched beats drift apart, so a long fade needs one side without them.
                let long_fade = technique == Technique::Fade && fade > 1;
                if long_fade
                    && !outgoing.beatless(exit, end)
                    && !incoming.is_some_and(|song| song.beatless(entry, entry + fade))
                {
                    continue;
                }
                let skipped = entry as f32 / MAX_SKIPPED_INTRO_BARS as f32;
                // A mashup changes the backing under a voice that goes on:
                // what matters is that it keeps its level and its beat. Other
                // mixes overlap whole songs, best where both are quiet.
                let quiet = if technique == Technique::Mashup {
                    -incoming.map_or(0.0, |song| {
                        let taken = entry + before..entry + overlap;
                        outgoing.step_into(exit..switch, song, taken, trim_db)
                    })
                } else {
                    (outgoing.quietness(exit, exit + overlap)
                        + incoming.map_or(0.0, |song| song.quietness(entry, entry + overlap)))
                        / 2.0
                };
                // A mashup's lanes keep the vocals apart by themselves.
                let clash = if technique == Technique::Mashup {
                    0
                } else {
                    clashing_bars(outgoing, incoming, exit, entry, overlap)
                        .saturating_sub(TOLERATED_CLASH_BARS)
                };
                let guard = stems && clash > 0;
                let vocals = if guard {
                    GUARD_POINTS
                } else {
                    CLASH_POINTS * clash as f32
                };
                let clash = if guard { 0 } else { clash };
                let fit = STRUCTURE_POINTS * (outgoing_fit + incoming_fit) + quiet
                    - UNHEARD_POINTS * unheard as f32
                    - SKIP_POINTS * skipped
                    - shorter
                    - vocals;
                found.push(Cue {
                    exit,
                    entry,
                    fade,
                    fit,
                    clash,
                    guard,
                });
            }
        }
    }
    found
}

/// Whether `technique` may play the songs' vocals apart for `request`: a
/// beat-matched hand-over or a mashup whose sources allow separating them,
/// when the application can.
fn stems_allowed(technique: Technique, request: &MixRequest<'_>) -> bool {
    request.separation
        && request.rights.separate
        && matches!(
            technique,
            Technique::Relaxed
                | Technique::Rolling
                | Technique::DoubleDrop
                | Technique::FilterSweep
                | Technique::Mashup
        )
}

/// Outgoing bars where `technique` may switch, with how well each suits it;
/// `fade_out` bars follow the switch.
fn switches(technique: Technique, outgoing: &Song<'_>, fade_out: usize) -> Vec<(usize, f32)> {
    let bars = outgoing.bars.len();
    match technique {
        // A fusion is placed by `plan_fusion`, never on a switch.
        Technique::Fusion => Vec::new(),
        Technique::Relaxed | Technique::FilterSweep | Technique::Fade => (1..bars)
            .map(|bar| (bar, outgoing.calm_switch(bar)))
            .filter(|(_, fit)| *fit > 0.0)
            .collect(),
        // The outgoing vocals ride over the incoming instrumental from the
        // switch, so the outgoing song sings there, and they best leave where
        // a phrase of theirs ends: on a phrase line, or where they stop.
        Technique::Mashup => (1..bars)
            .filter(|bar| outgoing.phrase(*bar) > 0.0)
            .filter_map(|bar| {
                let sung = outgoing.sung_share(bar, bar + fade_out);
                let end = bar + fade_out;
                let leave = match outgoing.sung.get(end) {
                    Some(true) => outgoing.phrase(end),
                    _ => 1.0,
                };
                (sung >= MASHUP_SUNG_SHARE)
                    .then(|| (bar, sung * (outgoing.phrase(bar) + leave) / 2.0))
            })
            .collect(),
        Technique::Rolling => {
            let calm = outgoing.calm_starts();
            (1..bars)
                .filter(|bar| outgoing.strong_at(*bar) == Some(true))
                .filter_map(|bar| {
                    let ends = calm.iter().find(|calm| **calm > bar)?;
                    let fade_end = bar + fade_out;
                    let place = match fade_end.cmp(ends) {
                        std::cmp::Ordering::Equal => 1.0,
                        std::cmp::Ordering::Less => EARLY_ROLL,
                        std::cmp::Ordering::Greater => return None,
                    };
                    Some((bar, outgoing.phrase(bar) * place))
                })
                .filter(|(_, fit)| *fit > 0.0)
                .collect()
        }
        Technique::DoubleDrop => outgoing
            .drops()
            .into_iter()
            .map(|drop| (drop, outgoing.phrase(drop)))
            .collect(),
    }
}

/// Incoming bars `technique` may start on `fade` bars before the switch (and
/// `ride` bars before a mashup's end), with how well each suits it: the
/// intro, from its start or a later phrase of a calm intro, by how strongly
/// it starts a phrase there and at the switch; for rolling and double-drop
/// mixes, `fade` bars before a drop; for a mashup, whose stems mute the
/// incoming vocals until the ride ends, a stretch after which they come in on
/// a phrase line (best where they start singing) and that is sung as little
/// as possible until then. Only its start without a beat grid.
fn entries(
    technique: Technique,
    incoming: Option<&Song<'_>>,
    fade: usize,
    ride: usize,
) -> Vec<(usize, f32)> {
    let Some(song) = incoming else {
        return vec![(0, 0.0)];
    };
    match technique {
        Technique::Rolling | Technique::DoubleDrop => song
            .drops()
            .into_iter()
            .filter(|drop| *drop >= fade && drop - fade <= MAX_SKIPPED_INTRO_BARS)
            .map(|drop| (drop - fade, song.phrase(drop)))
            .collect(),
        Technique::Mashup => (0..=MAX_SKIPPED_INTRO_BARS.min(song.bars.len().saturating_sub(1)))
            .filter(|bar| *bar == 0 || song.phrase(*bar) > 0.0)
            .map(|bar| {
                let voice = bar + fade + ride;
                let sings = |bar: usize| song.sung.get(bar).copied().unwrap_or(false);
                let vocals = match (sings(voice), voice > 0 && sings(voice - 1)) {
                    (true, false) => 1.0,
                    (true, true) => song.phrase(voice),
                    (false, _) => LATE_VOCALS,
                };
                let muted = song.sung_share(bar, voice);
                (
                    bar,
                    (song.phrase(bar) + vocals) / 2.0 - MUTED_POINTS * muted,
                )
            })
            .collect(),
        _ => (0..=MAX_SKIPPED_INTRO_BARS.min(song.bars.len().saturating_sub(1)))
            .filter(|bar| {
                *bar == 0 || (song.phrase(*bar) > 0.0 && song.strong_at(*bar) == Some(false))
            })
            .map(|bar| (bar, (song.phrase(bar) + song.phrase(bar + fade)) / 2.0))
            .collect(),
    }
}

/// Bars of a `fade`-bar mix before and after its switch: the incoming song
/// rises before it, the outgoing one fades after it (half as long in a
/// double drop, the outgoing vocals riding in a mashup). Fades between
/// unmatched songs start on the switch and last `fade` bars.
fn around(technique: Technique, fade: usize) -> (usize, usize) {
    match technique {
        Technique::DoubleDrop => (fade, (fade / 2).max(1)),
        Technique::Fade => (0, fade),
        _ => (fade, fade),
    }
}

/// Points a plan loses for what its evaluation found.
fn penalty(report: &MixReport) -> f32 {
    BEAT_POINTS * report.beat_error_ms.unwrap_or(0.0) / MAX_BEAT_ERROR_MS
        + LOUDNESS_POINTS_PER_LU * report.loudness_step_lu.unwrap_or(0.0)
}

/// A plan drawn among `candidates` (each with its points): within
/// [`MAX_DRAW_GAP`] of the best, a better one more often (softmax with
/// [`DRAW_POINTS`]).
fn draw(mut candidates: Vec<(MixPlan, f32)>, random: &mut SplitMix) -> Option<MixPlan> {
    let best = candidates
        .iter()
        .map(|(_, score)| *score)
        .fold(f32::NEG_INFINITY, f32::max);
    candidates.retain(|(_, score)| *score >= best - MAX_DRAW_GAP);
    let weights: Vec<f64> = candidates
        .iter()
        .map(|(_, score)| f64::from((score - best) / DRAW_POINTS).exp())
        .collect();
    let mut left = random.unit() * weights.iter().sum::<f64>();
    let index = weights
        .iter()
        .position(|weight| {
            left -= weight;
            left < 0.0
        })
        .unwrap_or(candidates.len().checked_sub(1)?);
    Some(candidates.swap_remove(index).0)
}

/// A fade for an outgoing song without a beat grid, placed by loudness alone:
/// it ends where the song stops being audible, or starts at the earliest exit
/// when that is later.
fn loudness_fade(request: &MixRequest<'_>, random: &mut SplitMix) -> Option<MixPlan> {
    let rate = f64::from(request.outgoing.sample_rate);
    let length = (f64::from(FADE_SECONDS) * rate) as u64;
    let exit = request
        .outgoing
        .audible_end()
        .saturating_sub(length)
        .max(request.earliest_exit);
    if exit + length > request.outgoing.duration_frames {
        return None;
    }
    let incoming = Song::new(request.incoming);
    // The incoming song starts on its first downbeat, or where its sound
    // starts: never on its leading silence.
    let entry = incoming
        .as_ref()
        .and_then(|song| song.bars.first().copied())
        .unwrap_or_else(|| request.incoming.audible_start());
    let shape = Shape::pick(Technique::Fade, request.intensity, false, 0, random);
    let trim_db = trim_db(request.outgoing, request.incoming);
    let (outgoing_lanes, incoming_lanes, overlap) =
        fade_lanes(FADE_SECONDS, shape, 10_f32.powf(trim_db / 20.0))?;
    let candidate = Candidate {
        exit,
        entry,
        speed: 1.0,
        overlap,
        trim_db,
        outgoing: &outgoing_lanes,
        incoming: &incoming_lanes,
    };
    let report = evaluate(&candidate, request.outgoing, request.incoming, None);
    let plan = MixPlan {
        technique: Technique::Fade,
        outgoing_rate: request.outgoing.sample_rate,
        incoming_rate: request.incoming.sample_rate,
        exit_frame: exit,
        entry_frame: entry,
        speed: 1.0,
        overlap,
        outgoing: outgoing_lanes,
        incoming: incoming_lanes,
        report,
        fusion: None,
        vocal_shift: None,
    };
    (plan.report.passes(UNGRIDDED_BEAT_SECONDS) && plan.validate().is_ok()).then_some(plan)
}

/// Variations of a transition's sound, drawn per candidate.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Shape {
    /// The outgoing song leaves through a rising high-pass instead of a bass cut.
    highpass_exit: bool,
    /// The outgoing song's fade-out washes into a reverb.
    reverb_tail: bool,
    /// With stems, the outgoing vocals ride over the incoming instrumental
    /// after the switch, and the incoming vocals wait for them to end.
    acapella: bool,
    /// Semitones a mashup's outgoing vocals move to fit the incoming key.
    vocal_shift: i8,
}

impl Shape {
    /// The sound of one candidate; `stems` when the songs' vocals may be
    /// played apart, and `vocal_shift` the semitones a mashup moves the
    /// outgoing ones.
    fn pick(
        technique: Technique,
        intensity: u8,
        stems: bool,
        vocal_shift: i8,
        random: &mut SplitMix,
    ) -> Self {
        let mut chance = |odds: f64| random.unit() < odds;
        match technique {
            Technique::Relaxed => Self {
                highpass_exit: chance(0.4),
                reverb_tail: intensity <= 1 && chance(0.5),
                acapella: stems && chance(ACAPELLA_ODDS[0]),
                vocal_shift: 0,
            },
            Technique::Rolling => Self {
                highpass_exit: chance(0.5),
                reverb_tail: false,
                acapella: stems && chance(ACAPELLA_ODDS[1]),
                vocal_shift: 0,
            },
            Technique::DoubleDrop => Self {
                highpass_exit: chance(0.3),
                reverb_tail: false,
                acapella: false,
                vocal_shift: 0,
            },
            Technique::Fade => Self {
                highpass_exit: chance(0.5),
                reverb_tail: chance(0.6),
                acapella: false,
                vocal_shift: 0,
            },
            Technique::FilterSweep => Self {
                highpass_exit: true,
                reverb_tail: false,
                acapella: false,
                vocal_shift: 0,
            },
            // The vocals carry a mashup, and the model's rendering a fusion,
            // unfiltered.
            Technique::Mashup | Technique::Fusion => Self {
                highpass_exit: false,
                reverb_tail: false,
                acapella: true,
                vocal_shift,
            },
        }
    }
}

fn build(
    technique: Technique,
    outgoing: &Song<'_>,
    incoming: Option<&Song<'_>>,
    matched: Option<Match>,
    cue: Cue,
    shape: Shape,
    request: &MixRequest<'_>,
) -> Option<MixPlan> {
    let Cue {
        exit: exit_bar,
        entry: entry_bar,
        fade,
        ..
    } = cue;
    let exit = *outgoing.bars.get(exit_bar)?;
    let beat = outgoing.beat as f32;
    let entry_frame = incoming.and_then(|song| song.bars.get(entry_bar).copied());
    let (before, after) = around(technique, fade);
    let (speed, entry) = match technique {
        Technique::Fade => (
            matched.map_or(1.0, |matched| matched.speed),
            entry_frame.unwrap_or(0),
        ),
        _ => {
            let (matched, detected) = (matched?, entry_frame?);
            let beats = (before + after) * usize::from(outgoing.beats_per_bar);
            incoming
                .and_then(|song| align(outgoing, song, matched, (exit_bar, entry_bar), beats))
                .unwrap_or((matched.speed, detected))
        }
    };
    // Riding over a quieter song, a mashup's vocals come down to its level.
    let vocal_gain = 10_f32.powf(trim_db(request.incoming, request.outgoing) / 20.0);
    let trim_db = trim_db(request.outgoing, request.incoming);
    let trim = 10_f32.powf(trim_db / 20.0);
    let switch = outgoing.seconds_between(exit_bar, exit_bar + before);
    let fade_out = outgoing.seconds_between(exit_bar + before, exit_bar + before + after);
    let bar = beat * f32::from(outgoing.beats_per_bar);
    let swap = if request.intensity <= 1 {
        beat * SOFT_SWAP_BEATS
    } else {
        beat * QUICK_SWAP_BEATS
    };
    let ride = Ride {
        switch,
        fade_out,
        swap: swap.min(switch),
        bar,
        beat,
    };
    let (outgoing_lanes, incoming_lanes, overlap) = match technique {
        Technique::Fade => fade_lanes(fade_out.max(FADE_SECONDS), shape, trim)?,
        Technique::FilterSweep => {
            let overlap = switch + fade_out;
            let (mut out, mut inc) = sweep(overlap, switch, beat, trim).ok()?;
            if cue.guard {
                guard_vocals(&mut out, &mut inc, ride).ok()?;
            }
            (out, inc, overlap)
        }
        Technique::Mashup => {
            let (out, inc) = mashup_lanes(ride, shape.vocal_shift != 0, trim, vocal_gain).ok()?;
            (out, inc, switch + fade_out)
        }
        _ => {
            let (mut out, mut inc) = hand_over(switch, fade_out, ride.swap, shape, trim).ok()?;
            let sung_after = outgoing
                .sung
                .get(exit_bar + before..exit_bar + before + after)
                .is_some_and(|bars| bars.iter().any(|sung| *sung));
            if shape.acapella && !cue.guard && sung_after {
                acapella(&mut out, &mut inc, ride).ok()?;
            } else if cue.guard {
                guard_vocals(&mut out, &mut inc, ride).ok()?;
            }
            (out, inc, switch + fade_out)
        }
    };
    let vocal_shift =
        (technique == Technique::Mashup && shape.vocal_shift != 0).then_some(VocalShift {
            semitones: shape.vocal_shift,
            from: switch,
        });
    let candidate = Candidate {
        exit,
        entry,
        speed,
        overlap,
        trim_db,
        outgoing: &outgoing_lanes,
        incoming: &incoming_lanes,
    };
    let aligned = match technique {
        Technique::Fade => None,
        _ => incoming
            .zip(matched)
            .map(|(incoming, matched)| (outgoing, incoming, matched)),
    };
    let report = evaluate(&candidate, request.outgoing, request.incoming, aligned);
    let plan = MixPlan {
        technique,
        outgoing_rate: request.outgoing.sample_rate,
        incoming_rate: request.incoming.sample_rate,
        exit_frame: exit,
        entry_frame: entry,
        speed: speed as f32,
        overlap,
        outgoing: outgoing_lanes,
        incoming: incoming_lanes,
        report,
        fusion: None,
        vocal_shift,
    };
    plan.validate().ok().map(|()| plan)
}

/// An AI fusion of the songs at `matched` speed, rendered from `strength`
/// noise, drawn among the best exits and entries, or
/// `None` when no exit near the outgoing song's end or no entry near the
/// incoming song's start leaves the model enough of both songs.
///
/// The outgoing song exits on a phrase line and plays on through the fusion,
/// which ends at most [`MAX_UNHEARD_END`] before its music does; the incoming
/// song enters after it on a phrase line of its first bars, better on a strong
/// section the blend builds up to, and plays the fusion's bars before its
/// entry under it. Every bar either song sings during the fusion costs points.
/// The fusion lasts [`FUSION_BARS`] of the outgoing song by mix length, and the
/// model hears [`FUSION_CONTEXT_BARS`] of each song around it.
fn plan_fusion(
    outgoing: &Song<'_>,
    incoming: &Song<'_>,
    matched: Match,
    strength: f32,
    request: &MixRequest<'_>,
    random: &mut SplitMix,
) -> Option<MixPlan> {
    let bar = (outgoing.beat * f64::from(outgoing.beats_per_bar)) as f32;
    let length = match request.length {
        MixLength::Short => 0,
        MixLength::Medium => 1,
        MixLength::Long => 2,
    };
    let mut bars = FUSION_BARS[length];
    while bars > 1 && bars as f32 * bar > MAX_FUSION_SECONDS {
        bars /= 2;
    }
    let context = (FUSION_CONTEXT_BARS as f32 * bar).min(MAX_FUSION_CONTEXT_SECONDS);
    let fusion = Fusion {
        before: context,
        length: bars as f32 * bar,
        after: context,
        seam: FUSION_SEAM_BEATS * outgoing.beat as f32,
        strength,
    };
    fusion.validate().ok()?;
    let lead = ((f64::from(context) + f64::from(fusion.seam)) * outgoing.rate) as u64;
    let played = (f64::from(fusion.length) * outgoing.rate) as u64;
    let audible = outgoing.audible_end.max(1) as f64;
    let exits: Vec<(usize, f32)> = (1..outgoing.bars.len())
        .filter_map(|exit| {
            let frame = outgoing.bars[exit];
            let unheard = (audible - (frame + played) as f64).max(0.0) / audible;
            let fits = frame >= request.earliest_exit
                && frame >= lead
                && frame + played <= outgoing.audible_end
                && unheard <= MAX_UNHEARD_END
                && outgoing.phrase(exit) > 0.0;
            let sung = (exit..exit + bars)
                .filter(|bar| outgoing.sung.get(*bar).copied().unwrap_or(false))
                .count();
            fits.then(|| {
                (
                    exit,
                    STRUCTURE_POINTS * outgoing.phrase(exit)
                        - UNHEARD_POINTS * unheard as f32
                        - FUSION_SUNG_POINTS * sung as f32,
                )
            })
        })
        .collect();
    // The incoming song plays the fusion's bars before its entry, and the
    // model hears it after its entry, at the mix's speed.
    let at_speed = |seconds: f32| (f64::from(seconds) * matched.speed * incoming.rate) as u64;
    let (under, heard) = (at_speed(fusion.length), at_speed(context));
    let last = (MAX_SKIPPED_INTRO_BARS + bars).min(incoming.bars.len().saturating_sub(1));
    let entries: Vec<(usize, f32)> = (bars..=last)
        .filter(|entry| incoming.phrase(*entry) > 0.0)
        .filter(|entry| {
            let frame = incoming.bars[*entry];
            frame >= under && frame + heard < incoming.analysis.duration_frames
        })
        .map(|entry| {
            let drop = if incoming.strong_at(entry) == Some(true) {
                FUSION_DROP
            } else {
                0.0
            };
            let sung = (entry - bars..entry)
                .filter(|bar| incoming.sung.get(*bar).copied().unwrap_or(false))
                .count();
            let skipped = (entry - bars) as f32 / MAX_SKIPPED_INTRO_BARS as f32;
            (
                entry,
                STRUCTURE_POINTS * incoming.phrase(entry) + drop
                    - SKIP_POINTS * skipped
                    - FUSION_SUNG_POINTS * sung as f32,
            )
        })
        .collect();
    let mut cues: Vec<(usize, usize, f32)> = exits
        .iter()
        .flat_map(|(exit, out_fit)| {
            entries
                .iter()
                .map(move |(entry, in_fit)| (*exit, *entry, out_fit + in_fit))
        })
        .collect();
    cues.sort_by(|a, b| b.2.total_cmp(&a.2));
    cues.truncate(MAX_BUILT);
    let trim_db = trim_db(request.outgoing, request.incoming);
    let trim = 10_f32.powf(trim_db / 20.0);
    let candidates: Vec<(MixPlan, f32)> = cues
        .into_iter()
        .filter_map(|(exit, entry, fit)| {
            let exit_frame = outgoing.bars[exit];
            let entry_frame = incoming.bars[entry];
            let (outgoing_lanes, incoming_lanes) = fusion_lanes(&fusion, trim).ok()?;
            let overlap = fusion.length + fusion.seam;
            let candidate = Candidate {
                exit: exit_frame,
                entry: entry_frame,
                speed: matched.speed,
                overlap,
                trim_db,
                outgoing: &outgoing_lanes,
                incoming: &incoming_lanes,
            };
            let report = evaluate(&candidate, request.outgoing, request.incoming, None);
            let plan = MixPlan {
                technique: Technique::Fusion,
                outgoing_rate: request.outgoing.sample_rate,
                incoming_rate: request.incoming.sample_rate,
                exit_frame,
                entry_frame,
                speed: matched.speed as f32,
                overlap,
                outgoing: outgoing_lanes,
                incoming: incoming_lanes,
                report,
                fusion: Some(fusion),
                vocal_shift: None,
            };
            let score = fit - penalty(&plan.report);
            (plan.validate().is_ok() && plan.report.passes(outgoing.beat as f32))
                .then_some((plan, score))
        })
        .collect();
    draw(candidates, random)
}

/// The decks of a fusion: the outgoing song hands over to the rendering over
/// the seam before the exit, and the incoming song takes over from it over the
/// seam after the fusion, trimmed by `trim` and back to its own level
/// afterwards.
fn fusion_lanes(fusion: &Fusion, trim: f32) -> Result<(Lanes, Lanes), PlanError> {
    let outgoing = Lanes {
        gain: Curve::new(ramp(-fusion.seam, 0.0, false, 1.0))?,
        ..Lanes::unchanged()
    };
    let end = fusion.length + fusion.seam;
    let incoming = Lanes {
        gain: incoming_gain(ramp(fusion.length, end, true, trim), end, trim)?,
        ..Lanes::unchanged()
    };
    Ok((outgoing, incoming))
}

/// Attenuation that brings a louder incoming song to the outgoing one's
/// integrated loudness, in dB (0 or negative).
fn trim_db(outgoing: &TrackAnalysis, incoming: &TrackAnalysis) -> f32 {
    match (outgoing.integrated_lufs, incoming.integrated_lufs) {
        (Some(out), Some(inc)) => (out - inc).clamp(-MAX_TRIM_DB, 0.0),
        _ => 0.0,
    }
}

/// An equal-power ramp from `from` to `to` seconds, rising (sine) or falling
/// (cosine), scaled by `scale`.
pub(crate) fn ramp(from: f32, to: f32, rising: bool, scale: f32) -> Vec<(f32, f32)> {
    (0..RAMP_POINTS)
        .map(|point| {
            let progress = point as f32 / (RAMP_POINTS - 1) as f32;
            let angle = progress * FRAC_PI_2;
            let value = if rising { angle.sin() } else { angle.cos() };
            // cos(π/2) is a tiny negative number in f32.
            (from + (to - from) * progress, value.clamp(0.0, 1.0) * scale)
        })
        .collect()
}

/// The incoming gain: `points`, then, when trimmed, back to its own level
/// over [`TRIM_RECOVERY_SECONDS`] after `until`.
fn incoming_gain(mut points: Vec<(f32, f32)>, until: f32, trim: f32) -> Result<Curve, PlanError> {
    if trim < 1.0 {
        points.push((until, trim));
        points.push((until + TRIM_RECOVERY_SECONDS, 1.0));
    }
    Curve::new(points)
}

/// The DJ hand-over: the incoming song rises until `switch` with its bass and
/// treble cut and its mids a little low; over `swap` seconds ending on the
/// switch downbeat, bass and treble change hands; then the outgoing song
/// fades over `fade_out` seconds. Only one bassline and one treble play at a
/// time (Vande Veire and De Bie's crossfade pattern).
fn hand_over(
    switch: f32,
    fade_out: f32,
    swap: f32,
    shape: Shape,
    trim: f32,
) -> Result<(Lanes, Lanes), PlanError> {
    let end = switch + fade_out;
    let swap_start = switch - swap;
    let incoming = Lanes {
        gain: incoming_gain(ramp(0.0, switch, true, trim), end, trim)?,
        low_db: Curve::new(vec![
            (0.0, MIN_EQ_DB),
            (swap_start, MIN_EQ_DB),
            (switch, 0.0),
        ])?,
        high_db: Curve::new(vec![
            (0.0, TREBLE_CUT_DB),
            (swap_start, TREBLE_CUT_DB),
            (switch, 0.0),
        ])?,
        mid_db: Curve::new(vec![(0.0, ENTRY_MID_DB), (switch, 0.0)])?,
        ..Lanes::unchanged()
    };
    let mut fade = vec![(0.0, 1.0)];
    fade.extend(ramp(switch, end.max(switch + CUT_SECONDS), false, 1.0));
    let mut outgoing = Lanes {
        gain: Curve::new(fade)?,
        high_db: Curve::new(vec![(swap_start, 0.0), (switch, TREBLE_CUT_DB)])?,
        ..Lanes::unchanged()
    };
    if shape.highpass_exit {
        outgoing.highpass = Curve::new(vec![
            (swap_start, 0.0),
            (switch, EXIT_HIGHPASS),
            (end.max(switch), EXIT_HIGHPASS_END),
        ])?;
    } else {
        outgoing.low_db = Curve::new(vec![(swap_start, 0.0), (switch, MIN_EQ_DB)])?;
    }
    if shape.reverb_tail {
        outgoing.reverb_send = reverb_wash(switch, end.max(switch), REVERB_SEND)?;
        outgoing.effects.reverb = true;
    }
    Ok((outgoing, incoming))
}

/// A reverb send that rises from `from` to `peak` at `until`, where the deck
/// falls silent, and closes there: the reverb then rings out what it was
/// given, and nothing the deck still plays under its closed gain feeds it.
fn reverb_wash(from: f32, until: f32, peak: f32) -> Result<Curve, PlanError> {
    Curve::new(vec![(from, 0.0), (until, peak), (until + CUT_SECONDS, 0.0)])
}

/// Holds the vocals apart with stems, handing them over on the switch: the
/// outgoing vocals fade over the bar before it and the incoming ones come in
/// over its last beat, at full level on the downbeat where their phrase
/// starts, so both songs never sing at once and no word is cut short.
fn guard_vocals(outgoing: &mut Lanes, incoming: &mut Lanes, ride: Ride) -> Result<(), PlanError> {
    let Ride {
        switch, bar, beat, ..
    } = ride;
    let mut leaving = vec![(0.0, 1.0)];
    leaving.extend(ramp((switch - bar).max(0.0), switch, false, 1.0));
    outgoing.vocals = Curve::new(leaving)?;
    incoming.vocals = Curve::new(vec![
        (0.0, 0.0),
        ((switch - beat * VOCAL_RETURN_BEATS).max(0.0), 0.0),
        (switch, 1.0),
    ])?;
    Ok(())
}

/// Where a mix that plays the vocals apart changes hands, in seconds since
/// the exit.
#[derive(Debug, Clone, Copy)]
struct Ride {
    switch: f32,
    /// From the switch to the end of the mix: how long outgoing vocals ride.
    fade_out: f32,
    /// The bass and treble swap ending on the switch.
    swap: f32,
    bar: f32,
    beat: f32,
}

impl Ride {
    /// Where the ride ends.
    fn end(&self) -> f32 {
        self.switch + self.fade_out.max(CUT_SECONDS)
    }

    /// The incoming vocals, silent through the ride and back over its last
    /// [`VOCAL_RETURN_BEATS`]: at full level on the downbeat where their
    /// phrase starts, with the notes that lead into it.
    fn incoming_vocals(&self) -> Result<Curve, PlanError> {
        let end = self.end();
        Curve::new(vec![
            (0.0, 0.0),
            ((end - self.beat * VOCAL_RETURN_BEATS).max(self.switch), 0.0),
            (end, 1.0),
        ])
    }
}

/// The outgoing vocals ride over the incoming instrumental: the outgoing
/// instrumental leaves with the bass swap; its vocals stay full and
/// unfiltered and fade during the ride's last bar, where the incoming vocals
/// come in.
fn acapella(outgoing: &mut Lanes, incoming: &mut Lanes, ride: Ride) -> Result<(), PlanError> {
    let Ride {
        switch, swap, bar, ..
    } = ride;
    let end = ride.end();
    outgoing.gain = Curve::new(vec![(0.0, 1.0), (end, 1.0), (end + CUT_SECONDS, 0.0)])?;
    outgoing.instrumental = Curve::new(vec![(switch - swap, 1.0), (switch, 0.0)])?;
    outgoing.low_db = Curve::constant(0.0);
    let mut vocals = vec![(0.0, 1.0)];
    vocals.extend(ramp((end - bar).max(switch), end, false, 1.0));
    outgoing.vocals = Curve::new(vocals)?;
    outgoing.high_db = Curve::constant(0.0);
    outgoing.highpass = Curve::constant(0.0);
    incoming.vocals = ride.incoming_vocals()?;
    Ok(())
}

/// A mashup's decks. The incoming instrumental rises under the outgoing song
/// with its own vocals muted, its bass cut and its treble opening over the
/// last [`TREBLE_LEAD_BARS`]; on the switch downbeat the bass changes hands
/// and the outgoing instrumental drops to [`INSTRUMENTAL_BLEND`], blending
/// with the incoming one over [`MASHUP_BLEND_SHARE`] of the ride before it
/// leaves. The outgoing vocals ride over the incoming instrumental at
/// `vocal_gain` (the level of a quieter incoming song), whole again once
/// their own instrumental is gone, and leave over the ride's last bar into a
/// reverb while the incoming vocals come in.
///
/// With `moved` vocals the keys clash, so the two harmonies never meet: the
/// incoming song shows only its hats ([`HATS_HIGHPASS`]) until the switch,
/// where it opens while the outgoing instrumental leaves with the swap.
fn mashup_lanes(
    ride: Ride,
    moved: bool,
    trim: f32,
    vocal_gain: f32,
) -> Result<(Lanes, Lanes), PlanError> {
    let Ride {
        switch,
        fade_out,
        swap,
        bar,
        beat,
    } = ride;
    let end = ride.end();
    let swap_start = switch - swap;
    let mut incoming = Lanes {
        gain: incoming_gain(ramp(0.0, switch, true, trim), end, trim)?,
        vocals: ride.incoming_vocals()?,
        ..Lanes::unchanged()
    };
    if moved {
        incoming.highpass = Curve::new(vec![
            (0.0, HATS_HIGHPASS),
            (swap_start, HATS_HIGHPASS),
            (switch, 0.0),
        ])?;
    } else {
        incoming.low_db = Curve::new(vec![
            (0.0, MIN_EQ_DB),
            (swap_start, MIN_EQ_DB),
            (switch, 0.0),
        ])?;
        incoming.high_db = Curve::new(vec![
            (0.0, TREBLE_CUT_DB),
            (
                (swap_start - bar * TREBLE_LEAD_BARS).max(0.0),
                TREBLE_CUT_DB,
            ),
            (swap_start, TREBLE_LEAD_DB),
            (switch, 0.0),
        ])?;
        incoming.mid_db = Curve::new(vec![(0.0, ENTRY_MID_DB), (switch, 0.0)])?;
    }
    // The riding vocals settle at the incoming level over their first bar.
    let settled = (switch + bar).min(end - bar).max(switch);
    let mut vocals = vec![(switch, 1.0), (settled, vocal_gain)];
    vocals.extend(ramp((end - bar).max(settled), end, false, vocal_gain));
    let mut outgoing = Lanes {
        gain: Curve::new(vec![(0.0, 1.0), (end, 1.0), (end + CUT_SECONDS, 0.0)])?,
        vocals: Curve::new(vocals)?,
        reverb_send: reverb_wash((end - bar).max(switch), end, VOCAL_REVERB_SEND)?,
        effects: Effects { reverb: true },
        ..Lanes::unchanged()
    };
    if moved {
        outgoing.instrumental = Curve::new(vec![(swap_start, 1.0), (switch, 0.0)])?;
    } else {
        let blended = (switch + fade_out * MASHUP_BLEND_SHARE).min(end);
        outgoing.instrumental = Curve::new(vec![
            (swap_start, 1.0),
            (switch, INSTRUMENTAL_BLEND),
            (blended, 0.0),
        ])?;
        // Only one bassline plays; the vocals get their low end back once
        // their own instrumental is gone.
        outgoing.low_db = Curve::new(vec![
            (swap_start, 0.0),
            (switch, MIN_EQ_DB),
            (blended, MIN_EQ_DB),
            (blended + beat, 0.0),
        ])?;
    }
    Ok((outgoing, incoming))
}

/// A filter hand-over for clashing keys: the outgoing high-pass climbs while
/// the incoming low-pass opens, so the two harmonies barely meet; the
/// incoming bass waits for the switch.
fn sweep(overlap: f32, switch: f32, beat: f32, trim: f32) -> Result<(Lanes, Lanes), PlanError> {
    let open = switch.max(overlap * 0.6);
    let incoming = Lanes {
        gain: incoming_gain(ramp(0.0, switch.max(beat), true, trim), overlap, trim)?,
        low_db: Curve::new(vec![
            (0.0, MIN_EQ_DB),
            ((switch - beat).max(0.0), MIN_EQ_DB),
            (switch, 0.0),
        ])?,
        lowpass: Curve::new(vec![(0.0, EXIT_HIGHPASS), (open, FILTER_OCTAVES)])?,
        ..Lanes::unchanged()
    };
    let outgoing = Lanes {
        gain: Curve::new(vec![(0.0, 1.0), (open, 1.0), (overlap, 0.0)])?,
        highpass: Curve::new(vec![(0.0, 0.0), (overlap, SWEEP_HIGHPASS_END)])?,
        ..Lanes::unchanged()
    };
    Ok((outgoing, incoming))
}

/// A fade over `length` seconds for songs that cannot be beat-matched:
/// equal-power gains, the incoming bass waiting for the second half,
/// optionally through a high-pass or into a reverb.
fn fade_lanes(length: f32, shape: Shape, trim: f32) -> Option<(Lanes, Lanes, f32)> {
    let half = length / 2.0;
    let incoming = Lanes {
        gain: incoming_gain(ramp(0.0, length, true, trim), length, trim).ok()?,
        low_db: Curve::new(vec![(0.0, MIN_EQ_DB), (half, MIN_EQ_DB), (length, 0.0)]).ok()?,
        ..Lanes::unchanged()
    };
    let mut outgoing = Lanes {
        gain: Curve::new(ramp(0.0, length, false, 1.0)).ok()?,
        low_db: Curve::new(vec![(0.0, 0.0), (half, MIN_EQ_DB)]).ok()?,
        ..Lanes::unchanged()
    };
    if shape.highpass_exit {
        outgoing.highpass = Curve::new(vec![(0.0, 0.0), (length, EXIT_HIGHPASS)]).ok()?;
    }
    if shape.reverb_tail {
        outgoing.reverb_send = reverb_wash(0.0, length, REVERB_SEND).ok()?;
        outgoing.effects.reverb = true;
    }
    Some((outgoing, incoming, length))
}

#[cfg(test)]
mod tests {
    use super::*;
    use yt_core::{BandEnergy, KeyEstimate, KeyMode, Meter, PitchClass, Section};

    const RATE: u32 = 44_100;

    struct Fixture {
        bpm: f32,
        bars: usize,
        beats_per_bar: u8,
        key: (u8, KeyMode),
        lufs: f32,
        /// Relative growth of each beat interval over the previous one.
        drift: f64,
        /// Frames the beats are rounded to, as the analyzer rounds them to
        /// its spectrogram frames (1: exact).
        frame_step: f64,
        /// (start bar, strong) of each section.
        sections: Vec<(u32, bool)>,
        /// First and past-last bars of each sung stretch; vocals are not
        /// measured when empty.
        vocals: Vec<(usize, usize)>,
    }

    impl Fixture {
        /// A 96-bar pop-like song: intro, verse, chorus, verse, bridge, chorus, outro.
        fn new(bpm: f32) -> Self {
            Self {
                bpm,
                bars: 96,
                beats_per_bar: 4,
                key: (0, KeyMode::Major),
                lufs: -9.0,
                drift: 0.0,
                frame_step: 1.0,
                sections: vec![
                    (0, false),
                    (8, false),
                    (16, true),
                    (32, false),
                    (40, false),
                    (56, true),
                    (80, false),
                ],
                vocals: Vec::new(),
            }
        }

        fn analysis(&self) -> TrackAnalysis {
            let rate = f64::from(RATE);
            let mut interval = 60.0 / f64::from(self.bpm) * rate;
            let mut position = 0.5 * rate;
            let count = self.bars * usize::from(self.beats_per_bar);
            let mut beats = Vec::with_capacity(count);
            for _ in 0..count {
                beats.push(((position / self.frame_step).round() * self.frame_step) as u64);
                position += interval;
                interval *= 1.0 + self.drift;
            }
            let duration_frames = (position + interval).round() as u64;
            let seconds = duration_frames as f64 / rate;
            let strong = |bar: usize| {
                self.sections
                    .iter()
                    .rev()
                    .find(|(start, _)| *start as usize <= bar)
                    .is_some_and(|(_, strong)| *strong)
            };
            TrackAnalysis {
                analyzer_version: 2,
                sample_rate: RATE,
                duration_frames,
                downbeats: (0..self.bars)
                    .map(|bar| (bar * usize::from(self.beats_per_bar)) as u32)
                    .collect(),
                beats,
                meter: Some(Meter {
                    beats_per_bar: self.beats_per_bar,
                    subdivision: Subdivision::Binary,
                    confidence: 0.9,
                }),
                tempo_bpm: Some(self.bpm),
                tempo_stability: 1.0,
                key: Some(KeyEstimate {
                    tonic: PitchClass::new(self.key.0).unwrap(),
                    mode: self.key.1,
                    confidence: 0.8,
                }),
                integrated_lufs: Some(self.lufs),
                true_peak_dbtp: -1.0,
                short_term_lufs: vec![
                    self.lufs;
                    (seconds / f64::from(TrackAnalysis::LOUDNESS_STEP_SECONDS))
                        as usize
                ],
                momentary_lufs: vec![
                    self.lufs;
                    (seconds / f64::from(TrackAnalysis::LOUDNESS_STEP_SECONDS))
                        as usize
                ],
                bar_energy: (0..self.bars - 1)
                    .map(|bar| {
                        let mid = if strong(bar) { -14.0 } else { -24.0 };
                        BandEnergy {
                            low_db: mid + 2.0,
                            mid_db: mid,
                            high_db: mid - 6.0,
                        }
                    })
                    .collect(),
                sections: self
                    .sections
                    .iter()
                    .map(|(start, strong)| Section {
                        start_bar: *start,
                        high_energy: *strong,
                        level_db: if *strong { -12.0 } else { -22.0 },
                    })
                    .collect(),
                vocal_db: if self.vocals.is_empty() {
                    Vec::new()
                } else {
                    let bar_seconds = 60.0 / f64::from(self.bpm) * f64::from(self.beats_per_bar);
                    (0..(seconds / f64::from(TrackAnalysis::LOUDNESS_STEP_SECONDS)) as usize)
                        .map(|step| {
                            let time = (step as f64 + 0.5)
                                * f64::from(TrackAnalysis::LOUDNESS_STEP_SECONDS);
                            let bar = ((time - 0.5) / bar_seconds).floor().max(0.0) as usize;
                            if self
                                .vocals
                                .iter()
                                .any(|(start, end)| (*start..*end).contains(&bar))
                            {
                                -18.0
                            } else {
                                -90.0
                            }
                        })
                        .collect()
                },
            }
        }
    }

    fn request<'a>(
        outgoing: &'a TrackAnalysis,
        incoming: &'a TrackAnalysis,
        intensity: u8,
    ) -> MixRequest<'a> {
        MixRequest {
            outgoing,
            incoming,
            length: MixLength::Medium,
            intensity,
            rights: SourceRights::LIBRARY,
            earliest_exit: 0,
            previous: None,
            seed: 7,
            separation: false,
            style: DjStyle::Professional,
            fusing: false,
            always_fuse: false,
        }
    }

    /// The outgoing bar where `plan` switches: where the incoming bass is
    /// back, or the cue itself for fades.
    fn switch_bar(analysis: &TrackAnalysis, plan: &MixPlan) -> usize {
        let seconds = match plan.technique {
            Technique::Fade => 0.0,
            _ => plan.incoming.low_db.span().1,
        };
        let frame = plan.exit_frame + (f64::from(seconds) * f64::from(RATE)).round() as u64;
        analysis
            .downbeats
            .iter()
            .position(|index| analysis.beats[*index as usize].abs_diff(frame) <= 2)
            .expect("the switch is a downbeat")
    }

    fn bar_of(analysis: &TrackAnalysis, frame: u64) -> usize {
        analysis
            .downbeats
            .iter()
            .position(|index| analysis.beats[*index as usize] == frame)
            .expect("the cue is a downbeat")
    }

    /// Plans of the pair for many seeds.
    fn plans(outgoing: &TrackAnalysis, incoming: &TrackAnalysis, intensity: u8) -> Vec<MixPlan> {
        (0..200)
            .filter_map(|seed| {
                plan(&MixRequest {
                    seed,
                    ..request(outgoing, incoming, intensity)
                })
            })
            .collect()
    }

    #[test]
    fn a_relaxed_mix_swaps_on_a_phrase_of_the_outro_and_starts_the_intro() {
        let outgoing = Fixture::new(128.0).analysis();
        let incoming = Fixture::new(126.0).analysis();
        let relaxed: Vec<MixPlan> = plans(&outgoing, &incoming, 1)
            .into_iter()
            .filter(|plan| plan.technique == Technique::Relaxed)
            .collect();
        assert!(!relaxed.is_empty(), "soft settings give relaxed mixes");
        let switches: Vec<usize> = relaxed
            .iter()
            .map(|plan| switch_bar(&outgoing, plan))
            .collect();
        assert!(
            switches
                .iter()
                .all(|bar| *bar >= 80 && bar.is_multiple_of(4)),
            "the swap lands on a phrase of the outro: {switches:?}"
        );
        let at = |bar: usize| switches.iter().filter(|switch| **switch == bar).count();
        assert!(
            at(80) > at(84) && at(80) > at(92),
            "where the outro starts is the likeliest: {switches:?}"
        );
        let entries: Vec<usize> = relaxed
            .iter()
            .map(|plan| bar_of(&incoming, plan.entry_frame))
            .collect();
        let count = |bar: usize| entries.iter().filter(|entry| **entry == bar).count();
        assert!(
            entries.iter().all(|entry| *entry < 16) && (4..16).all(|bar| count(0) > count(bar)),
            "the incoming intro plays, most often from its start: {entries:?}"
        );
        let plan = relaxed
            .iter()
            .find(|plan| plan.entry_frame == incoming.beats[0])
            .unwrap();
        assert!((plan.speed - 128.0 / 126.0).abs() < 1e-4, "{}", plan.speed);
        let switch = 4.0 * 4.0 * 60.0 / 128.0;
        assert_eq!(plan.incoming.at(0.1).low_db, MIN_EQ_DB);
        assert_eq!(
            plan.incoming.at(0.1).high_db,
            TREBLE_CUT_DB,
            "treble waits too"
        );
        assert_eq!(plan.incoming.at(switch + 0.01).low_db, 0.0);
        assert!(plan.report.beat_error_ms.unwrap() < 1.0);
        assert_eq!(plan.report.bass_overlap_seconds, 0.0);
    }

    #[test]
    fn rolling_and_double_drop_mixes_meet_the_incoming_drop() {
        let outgoing = Fixture::new(128.0).analysis();
        let incoming = Fixture::new(128.0).analysis();
        let techniques: Vec<Technique> = (0..64)
            .filter_map(|seed| {
                plan(&MixRequest {
                    seed,
                    ..request(&outgoing, &incoming, 3)
                })
            })
            .map(|plan| {
                if matches!(plan.technique, Technique::Rolling | Technique::DoubleDrop) {
                    let entry = bar_of(&incoming, plan.entry_frame);
                    assert!(
                        [2, 4].iter().any(|fade| [16, 56].contains(&(entry + fade))),
                        "the incoming drop lands on the switch: {entry}"
                    );
                }
                plan.technique
            })
            .collect();
        assert!(techniques.contains(&Technique::Rolling), "{techniques:?}");
        assert!(
            techniques.contains(&Technique::DoubleDrop),
            "{techniques:?}"
        );
    }

    #[test]
    fn the_seed_and_the_previous_mix_vary_the_transition() {
        let outgoing = Fixture::new(124.0).analysis();
        let incoming = Fixture::new(124.0).analysis();
        let same = |seed| {
            plan(&MixRequest {
                seed,
                ..request(&outgoing, &incoming, 2)
            })
        };
        assert_eq!(same(3), same(3), "a seed always gives the same plan");
        let exits: std::collections::HashSet<u64> = (0..32)
            .filter_map(same)
            .map(|plan| plan.exit_frame)
            .collect();
        assert!(exits.len() >= 2, "new seeds move the cue: {exits:?}");
        let shapes: std::collections::HashSet<String> = (0..32)
            .filter_map(same)
            .map(|plan| {
                let filtered = plan.outgoing.at(plan.overlap).highpass > 0.0;
                format!(
                    "{:?} {:?} {filtered}",
                    plan.technique, plan.outgoing.effects
                )
            })
            .collect();
        assert!(shapes.len() >= 3, "{shapes:?}");
    }

    #[test]
    fn clashing_keys_mix_through_short_filter_sweeps() {
        let outgoing = Fixture::new(128.0).analysis();
        let mut tritone = Fixture::new(128.0);
        tritone.key = (6, KeyMode::Major);
        let incoming = tritone.analysis();
        let plan = plan(&MixRequest {
            length: MixLength::Long,
            ..request(&outgoing, &incoming, 2)
        })
        .unwrap();
        assert_eq!(plan.technique, Technique::FilterSweep);
        let eight_bars = 8.0 * 4.0 * 60.0 / 128.0;
        assert!(plan.overlap <= eight_bars + 0.01, "{}", plan.overlap);
    }

    #[test]
    fn songs_that_cannot_be_matched_fade_where_the_outgoing_one_is_calm() {
        let outgoing = Fixture::new(128.0).analysis();
        let slow = Fixture::new(96.0).analysis();
        let fade = plan(&request(&outgoing, &slow, 2)).unwrap();
        assert_eq!(fade.technique, Technique::Fade);
        assert_eq!(fade.speed, 1.0);
        let exit = bar_of(&outgoing, fade.exit_frame);
        assert!(
            exit >= 80 && exit.is_multiple_of(4),
            "on a phrase of the outro: {exit}"
        );
        assert!(fade.overlap <= FADE_SECONDS + 0.5);
        for intensity in [0, 4] {
            assert!(
                plans(&outgoing, &slow, intensity).iter().all(|plan| {
                    plan.technique == Technique::Fade && plan.overlap >= FADE_SECONDS - 0.01
                }),
                "at any intensity they fade, never quicker than {FADE_SECONDS} s"
            );
        }
        let mut waltz = Fixture::new(128.0);
        waltz.beats_per_bar = 3;
        let waltz = waltz.analysis();
        assert_eq!(
            plan(&request(&outgoing, &waltz, 2)).unwrap().technique,
            Technique::Fade
        );
    }

    #[test]
    fn half_time_songs_are_matched_beat_for_two_beats() {
        let outgoing = Fixture::new(128.0).analysis();
        let incoming = Fixture::new(64.5).analysis();
        let plan = plan(&request(&outgoing, &incoming, 1)).unwrap();
        assert_ne!(plan.technique, Technique::Fade);
        assert!((plan.speed - 128.0 / 129.0).abs() < 1e-4, "{}", plan.speed);
    }

    #[test]
    fn rights_and_the_earliest_exit_are_respected() {
        let outgoing = Fixture::new(128.0).analysis();
        let incoming = Fixture::new(128.0).analysis();
        let forbidden = MixRequest {
            rights: SourceRights::NONE,
            ..request(&outgoing, &incoming, 2)
        };
        assert!(plan(&forbidden).is_none());
        let late = MixRequest {
            earliest_exit: outgoing.duration_frames,
            ..request(&outgoing, &incoming, 2)
        };
        assert!(plan(&late).is_none());
    }

    #[test]
    fn drifting_beats_shorten_the_mix_until_they_stay_together() {
        let outgoing = Fixture::new(128.0).analysis();
        let mut drifting = Fixture::new(128.0);
        drifting.drift = 0.0004;
        let incoming = drifting.analysis();
        let plan = plan(&MixRequest {
            length: MixLength::Long,
            ..request(&outgoing, &incoming, 1)
        })
        .unwrap();
        let sixteen_bars = 16.0 * 4.0 * 60.0 / 128.0;
        assert!(plan.overlap < sixteen_bars, "{}", plan.overlap);
        assert!(plan
            .report
            .beat_error_ms
            .is_none_or(|error| error <= crate::MAX_BEAT_ERROR_MS));
    }

    #[test]
    fn a_louder_incoming_song_enters_at_the_outgoing_level_and_recovers() {
        let outgoing = Fixture::new(128.0).analysis();
        let mut loud = Fixture::new(128.0);
        loud.lufs = -5.0;
        let incoming = loud.analysis();
        let plan = plan(&request(&outgoing, &incoming, 1)).unwrap();
        let trim = 10_f32.powf(-4.0 / 20.0);
        assert!((plan.incoming.at(plan.overlap).gain - trim).abs() < 1e-3);
        assert_eq!(
            plan.incoming.at(plan.overlap + TRIM_RECOVERY_SECONDS).gain,
            1.0
        );
    }

    #[test]
    fn plans_outside_their_bounds_are_refused() {
        let outgoing = Fixture::new(128.0).analysis();
        let incoming = Fixture::new(126.0).analysis();
        let good = plan(&request(&outgoing, &incoming, 2)).unwrap();
        let too_fast = MixPlan {
            speed: 1.2,
            ..good.clone()
        };
        assert!(too_fast.validate().is_err());
        let endless = MixPlan {
            overlap: 1_000.0,
            ..good
        };
        assert!(endless.validate().is_err());
    }

    #[test]
    fn the_songs_do_not_sing_over_each_other() {
        let mut singer = Fixture::new(128.0);
        singer.vocals = vec![(8, 32), (40, 84)];
        let outgoing = singer.analysis();
        let mut second = Fixture::new(126.0);
        second.vocals = vec![(4, 40)];
        let incoming = second.analysis();
        let song = Song::new(&outgoing).unwrap();
        assert!(song.sung[80] && !song.sung[84] && !song.sung[4]);
        let bar_seconds = 4.0 * 60.0 / 128.0;
        for plan in plans(&outgoing, &incoming, 1) {
            let exit = bar_of(&outgoing, plan.exit_frame);
            let entry = bar_of(&incoming, plan.entry_frame);
            let bars = (plan.overlap / bar_seconds).round() as usize;
            let clash = (0..bars)
                .filter(|bar| (40..84).contains(&(exit + bar)) && (4..40).contains(&(entry + bar)))
                .count();
            assert!(
                clash <= 1,
                "{:?} from bar {exit} into bar {entry} for {bars} bars",
                plan.technique
            );
        }
        let unmeasured = Fixture::new(126.0).analysis();
        assert!(
            plans(&outgoing, &unmeasured, 1)
                .iter()
                .any(|plan| bar_of(&outgoing, plan.exit_frame) + 4 <= 80),
            "without the incoming vocals, earlier cues stay possible"
        );
    }

    #[test]
    fn with_stems_the_vocals_are_held_apart_or_ride_over_the_instrumental() {
        let mut singer = Fixture::new(128.0);
        singer.vocals = vec![(8, 32), (40, 84)];
        let outgoing = singer.analysis();
        let mut second = Fixture::new(128.0);
        second.vocals = vec![(12, 40)];
        let incoming = second.analysis();
        let plans: Vec<MixPlan> = (0..200)
            .filter_map(|seed| {
                plan(&MixRequest {
                    seed,
                    separation: true,
                    ..request(&outgoing, &incoming, 2)
                })
            })
            .collect();
        assert!(plans.iter().any(MixPlan::uses_stems));
        let bar_seconds = 4.0 * 60.0 / 128.0;
        let sung = |ranges: &[(usize, usize)], bar: f32| {
            ranges
                .iter()
                .any(|(start, end)| (*start as f32..*end as f32).contains(&bar))
        };
        let mut acapella = false;
        for plan in &plans {
            let exit = bar_of(&outgoing, plan.exit_frame) as f32;
            let entry = bar_of(&incoming, plan.entry_frame) as f32;
            let steps = (plan.overlap / 0.05) as usize;
            let both = (0..steps)
                .filter(|step| {
                    let t = *step as f32 * 0.05;
                    let heard = |lanes: &Lanes| lanes.gain.at(t) * lanes.vocals.at(t) > 0.1;
                    heard(&plan.outgoing)
                        && heard(&plan.incoming)
                        && sung(&[(8, 32), (40, 84)], exit + t / bar_seconds)
                        && sung(&[(12, 40)], entry + t / bar_seconds)
                })
                .count() as f32
                * 0.05;
            assert!(
                both <= bar_seconds + 0.05,
                "{:?}: {both} s of both singing",
                plan.technique
            );
            let ride = plan.overlap - 0.2;
            acapella |=
                plan.outgoing.instrumental.at(ride) == 0.0 && plan.outgoing.vocals.at(ride) > 0.0;
            if let Some([out, inc]) = plan.stem_frames() {
                let rate = f64::from(RATE);
                assert!(out.start < plan.exit_frame);
                assert!(out.end as f64 >= plan.exit_frame as f64 + f64::from(plan.overlap) * rate);
                assert!(inc.start == plan.entry_frame && inc.end > inc.start);
            }
        }
        assert!(
            acapella,
            "some mixes let the outgoing vocals ride over the instrumental"
        );
        let online = (0..200).filter_map(|seed| {
            plan(&MixRequest {
                seed,
                separation: true,
                rights: SourceRights::YOUTUBE,
                ..request(&outgoing, &incoming, 2)
            })
        });
        assert!(
            online.into_iter().any(|plan| plan.uses_stems()),
            "online songs are split too"
        );
        let forbidden = plan(&MixRequest {
            separation: true,
            rights: SourceRights {
                separate: false,
                ..SourceRights::LIBRARY
            },
            ..request(&outgoing, &incoming, 2)
        });
        assert!(
            forbidden.is_none_or(|plan| !plan.uses_stems()),
            "only sources that allow it are split"
        );
    }

    #[test]
    fn fusions_blend_both_songs_where_the_model_may_hear_them() {
        let outgoing = Fixture::new(128.0).analysis();
        // 6.7 % slower: too far to beat-match a hand-over, close enough to fuse.
        let incoming = Fixture::new(120.0).analysis();
        let fuse = |seed, rights, fusing, style| {
            plan(&MixRequest {
                seed,
                rights,
                fusing,
                style,
                ..request(&outgoing, &incoming, 2)
            })
        };
        let fusions: Vec<MixPlan> = (0..60)
            .filter_map(|seed| fuse(seed, SourceRights::LIBRARY, true, DjStyle::Mashup))
            .collect();
        assert_eq!(fusions.len(), 60, "every seed fuses the pair");
        let bar = 4.0 * 60.0 / 128.0;
        for plan in &fusions {
            assert_eq!(plan.technique, Technique::Fusion);
            let fusion = plan.fusion.expect("the plan carries its fusion");
            assert!(
                (fusion.length - 8.0 * bar).abs() < 1e-3,
                "8 bars at medium length"
            );
            assert!((fusion.seam - bar / 4.0).abs() < 1e-3, "one beat of seam");
            assert_eq!(fusion.strength, FUSION_STRENGTH);
            assert!((plan.speed - 128.0 / 120.0).abs() < 1e-3);
            assert_eq!(plan.outgoing.gain.at(-fusion.seam), 1.0);
            assert_eq!(
                plan.outgoing.gain.at(0.0),
                0.0,
                "the rendering takes over at the exit"
            );
            assert_eq!(plan.incoming.gain.at(fusion.length), 0.0);
            assert!(plan.incoming.gain.at(fusion.length + fusion.seam) > 0.9);
            let rate = f64::from(plan.outgoing_rate);
            let played = plan.exit_frame + (f64::from(fusion.length) * rate) as u64;
            assert!(
                played <= outgoing.audible_end(),
                "the outgoing song plays through it"
            );
            let under = (f64::from(fusion.length)
                * f64::from(plan.speed)
                * f64::from(plan.incoming_rate)) as u64;
            assert!(
                plan.entry_frame >= under,
                "the incoming song plays under it before its entry"
            );
            assert!(plan.stem_frames().is_none());
        }
        let exits: std::collections::HashSet<u64> =
            fusions.iter().map(|plan| plan.exit_frame).collect();
        assert!(exits.len() > 1, "new seeds move the fusion");
        for (rights, fusing, style) in [
            (
                SourceRights {
                    generate: false,
                    ..SourceRights::LIBRARY
                },
                true,
                DjStyle::Mashup,
            ),
            (SourceRights::LIBRARY, false, DjStyle::Mashup),
            (SourceRights::LIBRARY, true, DjStyle::Professional),
        ] {
            assert!(
                fuse(3, rights, fusing, style)
                    .is_none_or(|plan| plan.technique != Technique::Fusion),
                "no fusion without the right, the model or the style"
            );
        }
    }

    #[test]
    fn asked_to_fuse_always_any_tempo_key_or_meter_fuses() {
        let outgoing = Fixture::new(128.0).analysis();
        let mut far = Fixture::new(100.0);
        far.key = (6, KeyMode::Major);
        far.beats_per_bar = 3;
        let incoming = far.analysis();
        let plans: Vec<MixPlan> = (0..20)
            .filter_map(|seed| {
                plan(&MixRequest {
                    seed,
                    fusing: true,
                    always_fuse: true,
                    style: DjStyle::Mashup,
                    ..request(&outgoing, &incoming, 2)
                })
            })
            .collect();
        assert_eq!(plans.len(), 20);
        for plan in &plans {
            assert_eq!(
                plan.technique,
                Technique::Fusion,
                "a tritone, 28 % and 3/4 still fuse"
            );
            assert!((plan.speed - 1.28).abs() < 1e-3, "{}", plan.speed);
            assert_eq!(
                plan.fusion.map(|fusion| fusion.strength),
                Some(FUSION_CLASH_STRENGTH)
            );
        }
        let professional = plan(&MixRequest {
            fusing: true,
            always_fuse: true,
            ..request(&outgoing, &incoming, 2)
        });
        assert!(professional.is_none_or(|plan| plan.technique == Technique::Fade));
        let suited = plan(&MixRequest {
            fusing: true,
            style: DjStyle::Mashup,
            ..request(&outgoing, &incoming, 2)
        });
        assert!(
            suited.is_none_or(|plan| plan.technique != Technique::Fusion),
            "without the switch, only pairs that suit it fuse"
        );
    }

    /// Plans of the pair with the mashup style and stems, for many seeds.
    fn mashups(outgoing: &TrackAnalysis, incoming: &TrackAnalysis) -> Vec<MixPlan> {
        (0..100)
            .filter_map(|seed| {
                plan(&MixRequest {
                    seed,
                    separation: true,
                    style: DjStyle::Mashup,
                    ..request(outgoing, incoming, 2)
                })
            })
            .collect()
    }

    #[test]
    fn mashups_blend_the_instrumentals_and_hand_the_vocals_over() {
        let mut singer = Fixture::new(128.0);
        singer.vocals = vec![(8, 32), (40, 84)];
        let outgoing = singer.analysis();
        // 6.7 % slower: too far to beat-match a hand-over, close enough for a mashup.
        let mut second = Fixture::new(120.0);
        second.vocals = vec![(16, 40)];
        let incoming = second.analysis();
        let found = mashups(&outgoing, &incoming);
        assert_eq!(found.len(), 100, "every seed finds a mashup");
        let (bar, beat) = (4.0 * 60.0 / 128.0, 60.0 / 128.0);
        for plan in &found {
            assert_eq!(plan.technique, Technique::Mashup);
            assert_eq!(plan.vocal_shift, None, "the keys fit as they are");
            assert!((plan.speed - 128.0 / 120.0).abs() < 1e-3, "{}", plan.speed);
            let switch = plan.incoming.low_db.span().1;
            let ride_from = switch_bar(&outgoing, plan);
            let entry = bar_of(&incoming, plan.entry_frame);
            let bars = (plan.overlap / bar).round() as usize;
            assert!(
                (40..84).contains(&ride_from),
                "the outgoing song sings as it rides: bar {ride_from}"
            );
            assert!(
                (entry + bars).is_multiple_of(4),
                "the incoming vocals come in on a phrase line: bar {}",
                entry + bars
            );
            assert!(
                plan.outgoing.instrumental.at(switch + 0.1) > 0.3
                    && plan.incoming.gain.at(switch + 0.1) > 0.5,
                "both instrumentals blend past the switch"
            );
            assert_eq!(plan.outgoing.instrumental.at(plan.overlap - 0.1), 0.0);
            assert!(
                plan.incoming.high_db.at(switch - bar) > TREBLE_CUT_DB
                    && plan.incoming.high_db.at(switch - bar) < 0.0,
                "the incoming treble opens before the switch"
            );
            assert_eq!(
                plan.outgoing.vocals.at(switch + 0.1),
                1.0,
                "the vocals ride full"
            );
            assert_eq!(
                plan.outgoing.low_db.at(plan.overlap - beat),
                0.0,
                "whole again once their own instrumental has left"
            );
            assert!(
                plan.outgoing.reverb_send.at(plan.overlap) > 0.0 && plan.outgoing.effects.reverb,
                "they leave into a reverb"
            );
            assert_eq!(
                plan.outgoing.reverb_send.at(plan.overlap + 0.1),
                0.0,
                "which rings out without being fed"
            );
            assert_eq!(plan.incoming.vocals.at(plan.overlap - beat - 0.01), 0.0);
            assert_eq!(
                plan.incoming.vocals.at(plan.overlap),
                1.0,
                "the incoming vocals are whole on the downbeat where they take over"
            );
            assert_eq!(plan.outgoing.vocals.at(plan.overlap), 0.0);
            assert_eq!(plan.report.bass_overlap_seconds, 0.0);
            assert!(plan.uses_stems());
        }
        let on_their_start = found
            .iter()
            .filter(|plan| {
                let bars = (plan.overlap / bar).round() as usize;
                bar_of(&incoming, plan.entry_frame) + bars == 16
            })
            .count();
        assert!(
            on_their_start > 0,
            "some mashups hand over where the incoming song starts singing"
        );
        let without_stems = plan(&MixRequest {
            style: DjStyle::Mashup,
            ..request(&outgoing, &incoming, 2)
        });
        assert!(
            without_stems.is_none_or(|plan| plan.technique != Technique::Mashup),
            "without stems the style mixes as the professional one"
        );
        let unmeasured = Fixture::new(128.0).analysis();
        assert!(mashups(&outgoing, &unmeasured)
            .iter()
            .all(|plan| plan.technique != Technique::Mashup));
    }

    #[test]
    fn a_mashup_needs_no_instrumental_intro() {
        let mut singer = Fixture::new(100.0);
        singer.vocals = vec![(4, 90)];
        let outgoing = singer.analysis();
        // Like most songs, the incoming one sings from its second bar on.
        let mut second = Fixture::new(96.0);
        second.vocals = vec![(2, 92)];
        let incoming = second.analysis();
        let found = mashups(&outgoing, &incoming);
        assert_eq!(found.len(), 100);
        for plan in &found {
            assert_eq!(plan.technique, Technique::Mashup);
            let switch = plan.incoming.low_db.span().1;
            assert_eq!(
                plan.incoming.vocals.at(switch),
                0.0,
                "its stem keeps the incoming vocals out of the ride"
            );
            assert_eq!(plan.incoming.instrumental.at(switch), 1.0);
        }
    }

    #[test]
    fn a_mashup_keeps_the_level_of_the_backing() {
        let mut singer = Fixture::new(128.0);
        singer.vocals = vec![(8, 84)];
        let outgoing = singer.analysis();
        let mut second = Fixture::new(128.0);
        second.vocals = vec![(2, 92)];
        let incoming = second.analysis();
        let bar = 4.0 * 60.0 / 128.0;
        let strong = |song: &Fixture, bar: usize| {
            song.sections
                .iter()
                .rev()
                .find(|(start, _)| *start as usize <= bar)
                .is_some_and(|(_, strong)| *strong)
        };
        let found = mashups(&outgoing, &incoming);
        let level = found
            .iter()
            .filter(|plan| {
                let switch = plan.incoming.low_db.span().1;
                let taken = bar_of(&incoming, plan.entry_frame) + (switch / bar).round() as usize;
                strong(&singer, switch_bar(&outgoing, plan) - 1) == strong(&second, taken)
            })
            .count();
        assert!(
            level * 10 >= found.len() * 9,
            "the incoming song takes over where it is as loud as the outgoing one: {level} of {}",
            found.len()
        );
    }

    #[test]
    fn a_mashup_moves_the_vocals_when_the_keys_clash() {
        let mut singer = Fixture::new(128.0);
        singer.vocals = vec![(8, 32), (40, 84)];
        let outgoing = singer.analysis();
        // A whole tone above: clashing as they are, the same key two semitones up.
        let mut second = Fixture::new(124.0);
        second.key = (2, KeyMode::Major);
        second.vocals = vec![(16, 40)];
        let incoming = second.analysis();
        let found = mashups(&outgoing, &incoming);
        assert_eq!(found.len(), 100);
        let beat = 60.0 / 128.0;
        for plan in &found {
            assert_eq!(plan.technique, Technique::Mashup);
            let shift = plan
                .vocal_shift
                .expect("the vocals move to the incoming key");
            assert_eq!(shift.semitones, 2);
            let switch = shift.from;
            assert_eq!(
                plan.vocal_shift_frame(),
                Some(plan.exit_frame + (f64::from(switch) * f64::from(RATE)) as u64)
            );
            assert_eq!(
                plan.outgoing.instrumental.at(switch),
                0.0,
                "the two harmonies never meet: the outgoing instrumental has left"
            );
            assert_eq!(plan.outgoing.instrumental.at(switch - beat), 1.0);
            assert!(
                plan.incoming.highpass.at(switch - beat) >= HATS_HIGHPASS,
                "only the incoming hats sound before the switch"
            );
            assert_eq!(plan.incoming.highpass.at(switch), 0.0);
            assert!(plan.outgoing.vocals.at(switch + beat) > 0.9);
            assert_eq!(plan.report.bass_overlap_seconds, 0.0);
        }
        // A minor key a minor third above: no move within a whole tone fits.
        second.key = (3, KeyMode::Minor);
        let far = second.analysis();
        assert!(mashups(&outgoing, &far)
            .iter()
            .all(|plan| plan.technique != Technique::Mashup));
        let mut refused = found[0].clone();
        refused.vocal_shift = Some(VocalShift {
            semitones: 3,
            ..refused.vocal_shift.unwrap()
        });
        assert!(refused.validate().is_err());
        let mut heard = found[0].clone();
        heard.outgoing.instrumental = Curve::constant(1.0);
        assert!(
            heard.validate().is_err(),
            "moved vocals never sound with their own instrumental"
        );
    }

    #[test]
    fn beats_rounded_to_the_analyzer_frames_still_meet() {
        // Both songs' beats sit on 20 ms frames, as the analyzer reports them.
        let step = f64::from(RATE) * 0.02;
        let mut first = Fixture::new(98.0);
        first.frame_step = step;
        first.vocals = vec![(4, 90)];
        let outgoing = first.analysis();
        let mut second = Fixture::new(94.0);
        second.frame_step = step;
        second.vocals = vec![(2, 92)];
        let incoming = second.analysis();
        let true_bar = |bpm: f64, frame: u64| {
            let bar = 4.0 * 60.0 / bpm * f64::from(RATE);
            let start = 0.5 * f64::from(RATE);
            let index = ((frame as f64 - start) / bar).round();
            start + index * bar
        };
        let found = mashups(&outgoing, &incoming);
        assert_eq!(
            found.len(),
            100,
            "rounded beats do not keep songs from mixing"
        );
        for plan in &found {
            assert_eq!(plan.technique, Technique::Mashup);
            assert!(
                (f64::from(plan.speed) - 98.0 / 94.0).abs() < 3e-4,
                "the speed follows the real tempos: {}",
                plan.speed
            );
            // The incoming song enters as far from its real downbeat as the
            // exit lies from the outgoing one's: the real beats meet.
            let late = plan.exit_frame as f64 - true_bar(98.0, plan.exit_frame);
            let entry = plan.entry_frame as f64 - true_bar(94.0, plan.entry_frame);
            let apart = (entry - late * f64::from(plan.speed)) / f64::from(RATE) * 1_000.0;
            assert!(apart.abs() < 3.0, "the beats meet within 3 ms: {apart}");
            assert!(plan
                .report
                .beat_error_ms
                .is_some_and(|error| error <= MAX_BEAT_ERROR_MS));
        }
        let professional = plans(&outgoing, &incoming, 2);
        assert!(
            professional
                .iter()
                .any(|plan| plan.report.beat_error_ms.is_some()),
            "the professional style beat-matches them too"
        );
    }

    #[test]
    fn a_reverb_rings_out_without_being_fed_by_the_silent_deck() {
        let outgoing = Fixture::new(128.0).analysis();
        // Too far apart to beat-match: fades, most of them into a reverb.
        let incoming = Fixture::new(100.0).analysis();
        let washed: Vec<MixPlan> = plans(&outgoing, &incoming, 1)
            .into_iter()
            .filter(|plan| plan.outgoing.effects.reverb)
            .collect();
        assert!(!washed.is_empty());
        for plan in washed {
            let silent = plan.outgoing.span().1;
            assert!(plan.outgoing.reverb_send.at(plan.overlap - 0.05) > 0.0);
            assert_eq!(plan.outgoing.gain.at(silent), 0.0);
            assert_eq!(
                plan.outgoing.reverb_send.at(silent),
                0.0,
                "the deck plays on under its closed gain until it is released"
            );
            assert!(plan.end() >= silent + EFFECT_TAIL_SECONDS);
        }
    }

    #[test]
    fn a_song_strong_until_its_end_still_mixes_on_a_phrase_line() {
        let mut anthem = Fixture::new(122.0);
        anthem.bars = 99;
        anthem.sections = vec![(0, false), (16, true), (48, false), (52, true)];
        let outgoing = anthem.analysis();
        let incoming = Fixture::new(122.0).analysis();
        let found = plans(&outgoing, &incoming, 2);
        assert_eq!(found.len(), 200, "every seed finds a mix");
        for plan in found {
            let switch = switch_bar(&outgoing, &plan);
            assert!(
                switch >= 52 && (switch - 52).is_multiple_of(4) || (99 - switch).is_multiple_of(4),
                "{:?} switches on bar {switch}",
                plan.technique
            );
        }
    }

    #[test]
    fn a_fading_tail_is_not_mixed_over() {
        let mut fading = Fixture::new(128.0).analysis();
        let steps = fading.momentary_lufs.len();
        // The last 20 s fade out by 2 LU a second.
        for (step, lufs) in fading
            .momentary_lufs
            .iter_mut()
            .enumerate()
            .skip(steps - 40)
        {
            *lufs -= (step + 40 - steps) as f32;
        }
        let end = fading.audible_end();
        let expected = ((steps - 40 + 11) as f64 * 0.5 * f64::from(RATE)) as u64;
        assert!(
            end.abs_diff(expected) <= u64::from(RATE),
            "{end} vs {expected}"
        );
        let incoming = Fixture::new(128.0).analysis();
        let song = Song::new(&fading).unwrap();
        for plan in plans(&fading, &incoming, 2) {
            let switch = song.bars[switch_bar(&fading, &plan)];
            assert!(switch <= end, "the switch comes before the song fades away");
        }
    }

    #[test]
    fn a_song_without_a_beat_grid_fades_where_it_stops_being_audible() {
        let mut free = Fixture::new(128.0).analysis();
        free.tempo_bpm = None;
        let steps = free.momentary_lufs.len();
        for lufs in free.momentary_lufs.iter_mut().skip(steps - 20) {
            *lufs = -60.0;
        }
        let incoming = Fixture::new(128.0).analysis();
        let plan = plan(&request(&free, &incoming, 2)).unwrap();
        assert_eq!(plan.technique, Technique::Fade);
        let fade = (f64::from(FADE_SECONDS) * f64::from(RATE)) as u64;
        assert_eq!(plan.exit_frame + fade, free.audible_end());
        assert!(free.audible_end() < free.duration_frames - 9 * u64::from(RATE));
        assert_eq!(plan.entry_frame, incoming.beats[0]);
        let late = MixRequest {
            earliest_exit: free.duration_frames,
            ..request(&free, &incoming, 2)
        };
        assert!(plan_none(&late));
    }

    fn plan_none(request: &MixRequest<'_>) -> bool {
        plan(request).is_none()
    }
}
