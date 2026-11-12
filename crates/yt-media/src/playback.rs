use crate::analysis::Analyzer;
use crate::crossfade::{envelope, Crossfade, Envelope, Mix, Rendering, Transition};
use crate::deck_stems::{stem_stage, DeckStem, StemControl};
use crate::mix::{mix_stage, DeckMix, MixControl};
use crate::tempo::{TempoControl, TempoSource};
use crate::{
    CancellationToken, DecodedAudio, MediaError, ResolvedMedia, Result, StreamingHandle,
    StreamingSnapshot,
};
use rodio::{
    buffer::SamplesBuffer, source::SeekError, OutputStream, OutputStreamBuilder, Sink, Source,
};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex, MutexGuard, TryLockError,
    },
    time::{Duration, Instant},
};
use yt_core::AudioFeatures;

/// How long the engine waits for one command when idle; also the cadence of
/// snapshot updates and fade bookkeeping.
const COMMAND_POLL: Duration = Duration::from_millis(100);
/// Audio a deck measures between two publications of its features, in
/// source time.
const ANALYSIS_PUBLISH_INTERVAL: Duration = Duration::from_secs(2);
/// Longest wait for the device to open or for a request to be answered.
const ENGINE_TIMEOUT: Duration = Duration::from_secs(10);
/// Decks fading out at once; rapid skips release the oldest early.
const MAX_FADING_DECKS: usize = 2;
/// A faded deck lives this long past its ramp so the audio thread renders
/// its last, silent frames before the sink stops.
const FADE_RELEASE_MARGIN: Duration = Duration::from_millis(150);
/// Longest accepted seek target, in seconds (one year).
const MAX_SEEK_SECONDS: f64 = 31_536_000.0;
/// A mix's incoming deck starts on its own this long after its planned exit,
/// in case the outgoing deck stops before reaching it.
const GATE_GRACE: Duration = Duration::from_millis(500);
/// Shortest and longest time over which an incoming song whose tempo was
/// matched returns to the user's speed after the mix, and the time it takes
/// per percent of speed it was changed by: a quarter of a percent a second
/// goes unnoticed, while ten percent in eight seconds is heard as the song
/// speeding up. A fusion's larger changes return faster than that rather than
/// keep the song off its tempo for minutes.
const MIN_SPEED_RETURN: Duration = Duration::from_secs(8);
const MAX_SPEED_RETURN: Duration = Duration::from_secs(40);
const SPEED_RETURN_PER_PERCENT: Duration = Duration::from_secs(4);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaybackSnapshot {
    pub path: Option<PathBuf>,
    pub remote_id: Option<String>,
    pub streaming: Option<StreamingSnapshot>,
    pub is_playing: bool,
    pub finished: bool,
    pub position_seconds: f64,
    pub duration_seconds: f64,
    pub volume: f32,
    pub speed: f32,
    pub error: Option<String>,
    /// True while a previous source is still fading out under this one.
    #[serde(default)]
    pub crossfading: bool,
    /// How the active song sounds, measured from its decoded audio in source
    /// time (speed does not change it) and refreshed after every 2 s of played
    /// audio and at its end. `None` until [`crate::Analyzer::MIN_SECONDS`] have played;
    /// `analyzed_seconds` grows while the song plays and may still be below
    /// [`AudioFeatures::MIN_ANALYZED_SECONDS`].
    #[serde(default)]
    pub analysis: Option<AudioFeatures>,
    /// The blend into the active song while one is heard or about to be.
    #[serde(default)]
    pub blend: Option<Blend>,
}

/// A blend into the active song, for the interface.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Blend {
    /// A beat-matched mix (`Transition::Mix`), or a crossfade.
    pub mix: bool,
    /// Share of the blend already heard, 0–1; 0 while a mix waits for its
    /// exit frame.
    pub progress: f32,
}

/// When the current blend is heard, in wall-clock time; moved while paused.
#[derive(Debug, Clone, Copy)]
struct BlendTiming {
    mix: bool,
    start: Instant,
    end: Instant,
}

impl BlendTiming {
    /// The blend at `now`, or `None` once it is over.
    fn at(&self, now: Instant) -> Option<Blend> {
        if now >= self.end {
            return None;
        }
        let span = self.end.saturating_duration_since(self.start).as_secs_f32();
        let heard = now.saturating_duration_since(self.start).as_secs_f32();
        Some(Blend {
            mix: self.mix,
            progress: if span > 0.0 {
                (heard / span).min(1.0)
            } else {
                1.0
            },
        })
    }
}

impl Default for PlaybackSnapshot {
    fn default() -> Self {
        Self {
            path: None,
            remote_id: None,
            streaming: None,
            is_playing: false,
            finished: false,
            position_seconds: 0.0,
            duration_seconds: 0.0,
            volume: 0.8,
            speed: 1.0,
            error: None,
            crossfading: false,
            blend: None,
            analysis: None,
        }
    }
}

type Reply = mpsc::SyncSender<Result<()>>;

#[derive(Clone)]
struct SessionToken(Arc<CancellationToken>);

impl SessionToken {
    fn new(cancel: CancellationToken) -> Self {
        Self(Arc::new(cancel))
    }

    fn matches(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

fn require_session(active: Option<&SessionToken>, requested: Option<&SessionToken>) -> Result<()> {
    if requested.is_some_and(|requested| !active.is_some_and(|active| active.matches(requested))) {
        return Err(MediaError::Cancelled);
    }
    Ok(())
}

/// A specific remote playback, including its network cache. Commands made by a
/// delayed task cannot seek or stop a newer track, even when both use the same URL.
#[derive(Clone)]
pub struct PlaybackSession {
    player: AudioPlayer,
    token: SessionToken,
    stream: StreamingHandle,
}

impl PlaybackSession {
    pub fn snapshot(&self) -> StreamingSnapshot {
        self.stream.snapshot()
    }

    /// The file holding the whole stream once it is downloaded
    /// ([`StreamingHandle::complete_file`]).
    pub fn complete_file(&self) -> Option<std::path::PathBuf> {
        self.stream.complete_file()
    }

    pub fn seek(&self, seconds: f64) -> Result<()> {
        let position = seek_position(seconds)?;
        self.player
            .request(|reply| Command::Seek(position, Some(self.token.clone()), reply))
    }

    pub fn stop(&self) -> Result<()> {
        self.token.0.cancel();
        self.stream.cancel();
        self.player
            .request(|reply| Command::Stop(Some(self.token.clone()), Some(reply)))
    }

    /// Interrupt a blocked network read immediately and stop only this session.
    /// This nonblocking variant is suitable for UI cancellation and teardown.
    pub fn cancel(&self) {
        self.token.0.cancel();
        self.stream.cancel();
        let _ = self
            .player
            .send(Command::Stop(Some(self.token.clone()), None));
    }
}

fn seek_position(seconds: f64) -> Result<Duration> {
    if !seconds.is_finite() || !(0.0..=MAX_SEEK_SECONDS).contains(&seconds) {
        return Err(MediaError::InvalidInput(
            "La posición de reproducción no es válida.".into(),
        ));
    }
    Ok(Duration::from_secs_f64(seconds))
}

enum Command {
    Play(PathBuf, Transition, Reply),
    PlayReady(Box<PreparedInner>, Transition, Reply),
    Pause,
    Resume,
    Stop(Option<SessionToken>, Option<Reply>),
    Seek(Duration, Option<SessionToken>, Reply),
    Volume(f32),
    Speed(f32),
    Sleep(Option<Duration>),
}

struct PreparedInner {
    source: DecodedAudio,
    id: String,
    handle: StreamingHandle,
    token: SessionToken,
}

impl PreparedInner {
    fn cancel(&self) {
        self.token.0.cancel();
        self.handle.cancel();
    }
}

/// A remote stream that is open and buffering but not audible yet, so a
/// transition can start the instant it is needed. Dropping it before it is
/// played cancels its download.
pub struct PreparedStream {
    inner: Option<PreparedInner>,
}

impl PreparedStream {
    /// Remote identity of the prepared media (provider video id).
    pub fn media_id(&self) -> &str {
        self.inner.as_ref().map_or("", |inner| inner.id.as_str())
    }

    pub fn snapshot(&self) -> StreamingSnapshot {
        self.inner
            .as_ref()
            .map(|inner| inner.handle.snapshot())
            .unwrap_or_default()
    }

    /// The file holding the whole stream once it is downloaded
    /// ([`StreamingHandle::complete_file`]).
    pub fn complete_file(&self) -> Option<PathBuf> {
        self.inner.as_ref()?.handle.complete_file()
    }
}

impl Drop for PreparedStream {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            inner.cancel();
        }
    }
}

struct RemoteOrigin {
    id: String,
    handle: StreamingHandle,
    session: SessionToken,
}

/// The latest features a deck measured, shared with the engine thread.
type AnalysisSlot = Arc<Mutex<Option<AudioFeatures>>>;

/// Measures the decoded source on its way to the tempo stage, so analysis
/// runs in source time, and publishes the features every
/// `ANALYSIS_PUBLISH_INTERVAL` of audio and once more at the end. The audio
/// thread only ever `try_lock`s the slot: when the engine thread holds it,
/// that publication is skipped instead of waited for.
struct AnalysisTap<S: Source<Item = f32>> {
    source: S,
    analyzer: Analyzer,
    slot: AnalysisSlot,
    publish_every: u64,
    until_publish: u64,
}

impl<S: Source<Item = f32>> AnalysisTap<S> {
    fn new(source: S) -> (Self, AnalysisSlot) {
        let (rate, channels) = (source.sample_rate(), source.channels());
        let publish_every = (ANALYSIS_PUBLISH_INTERVAL.as_secs_f64()
            * f64::from(rate)
            * f64::from(channels.max(1))) as u64;
        let slot = AnalysisSlot::default();
        let tap = Self {
            source,
            analyzer: Analyzer::new(rate, channels),
            slot: slot.clone(),
            publish_every: publish_every.max(1),
            until_publish: publish_every.max(1),
        };
        (tap, slot)
    }

    fn publish(&mut self) {
        self.until_publish = self.publish_every;
        let Some(features) = self.analyzer.features() else {
            return;
        };
        match self.slot.try_lock() {
            Ok(mut slot) => *slot = Some(features),
            Err(TryLockError::Poisoned(poisoned)) => *poisoned.into_inner() = Some(features),
            Err(TryLockError::WouldBlock) => {}
        }
    }
}

impl<S: Source<Item = f32>> Iterator for AnalysisTap<S> {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        let Some(sample) = self.source.next() else {
            // The countdown restarts on every publication, so repeated calls
            // at the end publish once.
            if self.until_publish != self.publish_every {
                self.publish();
            }
            return None;
        };
        self.analyzer.push(sample);
        self.until_publish -= 1;
        if self.until_publish == 0 {
            self.publish();
        }
        Some(sample)
    }
}

impl<S: Source<Item = f32>> Source for AnalysisTap<S> {
    fn current_span_len(&self) -> Option<usize> {
        self.source.current_span_len()
    }
    fn channels(&self) -> u16 {
        self.source.channels()
    }
    fn sample_rate(&self) -> u32 {
        self.source.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        self.source.total_duration()
    }
    /// A seek keeps everything measured so far; only the frame in progress
    /// and the onset history restart.
    fn try_seek(&mut self, position: Duration) -> std::result::Result<(), SeekError> {
        self.source.try_seek(position)?;
        self.analyzer.restart();
        Ok(())
    }
}

/// One playing source with its own sink, envelope, tempo, mix stage,
/// analysis and network cache.
struct Deck {
    sink: Sink,
    envelope: Envelope,
    tempo: TempoControl,
    mix: MixControl,
    stems: StemControl,
    /// Channels and sample rate of the decoded source.
    format: (u16, u32),
    /// Set when the deck waits for a mix's exit frame.
    gate: Option<Gate>,
    speed_ramp: Option<SpeedRamp>,
    errors: Arc<Mutex<Option<String>>>,
    analysis: AnalysisSlot,
    remote: Option<RemoteOrigin>,
}

/// The incoming deck of a mix waits for the outgoing deck's exit frame; the
/// engine opens it by force once `deadline` passes.
struct Gate {
    force: Arc<AtomicBool>,
    deadline: Instant,
}

/// How long a deck playing at `from` takes to glide back to `to`, the user's
/// speed: [`SPEED_RETURN_PER_PERCENT`] of their difference, between
/// [`MIN_SPEED_RETURN`] and [`MAX_SPEED_RETURN`].
fn speed_return(from: f32, to: f32) -> Duration {
    let percent = ((from - to).abs() / to.max(f32::EPSILON) * 100.0).min(100.0);
    SPEED_RETURN_PER_PERCENT
        .mul_f32(percent)
        .clamp(MIN_SPEED_RETURN, MAX_SPEED_RETURN)
}

/// After a mix, a tempo-matched incoming deck glides from `from` back to the
/// user's speed between `start` and `end`.
struct SpeedRamp {
    from: f32,
    start: Instant,
    end: Instant,
}

/// Where a mix lands on the decks, computed when the incoming deck installs.
struct Placement {
    mix: Mix,
    /// Position of the incoming source when it starts.
    entry: Duration,
    exit_at: Instant,
}

impl Deck {
    fn audible(&self) -> bool {
        !self.sink.is_paused() && !self.sink.empty()
    }

    fn release(self) {
        self.sink.stop();
        if let Some(remote) = self.remote {
            remote.session.0.cancel();
            remote.handle.cancel();
        }
    }
}

struct FadingDeck {
    deck: Deck,
    until: Instant,
}

/// A fusion's rendering playing on its own sink until `until`.
struct FadingRendering {
    sink: Sink,
    until: Instant,
}

/// Owns the output device and every deck. Runs on the engine thread only.
struct Worker {
    output: OutputStream,
    state: Arc<Mutex<PlaybackSnapshot>>,
    stream_slot: Arc<Mutex<Option<StreamingHandle>>>,
    active: Option<Deck>,
    fading: Vec<FadingDeck>,
    renderings: Vec<FadingRendering>,
    sleep_at: Option<Instant>,
    last_tick: Instant,
    blend: Option<BlendTiming>,
}

impl Worker {
    fn state(&self) -> MutexGuard<'_, PlaybackSnapshot> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn active_session(&self) -> Option<&SessionToken> {
        self.active
            .as_ref()
            .and_then(|deck| deck.remote.as_ref())
            .map(|remote| &remote.session)
    }

    /// Where `mix` lands if the playing deck can take it: audible, at the
    /// plan's sample rate and normal speed, and not past the middle of the
    /// mix already. A late install moves the incoming entry by the delay so
    /// the beats still meet.
    fn place(&self, mix: &Mix, incoming_rate: u32) -> Option<Placement> {
        let plan = mix.plan();
        let active = self.active.as_ref().filter(|deck| deck.audible())?;
        if active.format.1 != plan.outgoing_rate
            || incoming_rate != plan.incoming_rate
            || self.state().speed != 1.0
        {
            return None;
        }
        // A plan that plays the vocals apart needs stems in both decks' formats.
        if plan.uses_stems()
            && mix
                .stems()
                .is_none_or(|[outgoing, _]| outgoing.format() != active.format)
        {
            return None;
        }
        let outgoing_rate = f64::from(plan.outgoing_rate);
        let now = active.tempo.frame();
        let late = now.saturating_sub(plan.exit_frame) as f64 / outgoing_rate;
        if late > f64::from(plan.overlap) / 2.0 {
            return None;
        }
        // A fusion's rendering starts a seam before the exit, so it cannot be
        // placed late.
        if let Some(fusion) = plan.fusion {
            let seam = (f64::from(fusion.seam) * outgoing_rate) as u64;
            if mix.rendering().is_none() || now + seam > plan.exit_frame {
                return None;
            }
        }
        let until_exit = plan.exit_frame.saturating_sub(now) as f64 / outgoing_rate;
        let entry =
            plan.entry_frame as f64 / f64::from(plan.incoming_rate) + late * f64::from(plan.speed);
        Some(Placement {
            mix: mix.clone(),
            entry: Duration::from_secs_f64(entry),
            exit_at: Instant::now() + Duration::from_secs_f64(until_exit),
        })
    }

    /// Make `source` the active deck. Anything that can fail happens before
    /// the current deck is touched, so a bad file never interrupts playback.
    fn install(
        &mut self,
        source: DecodedAudio,
        path: Option<PathBuf>,
        remote: Option<RemoteOrigin>,
        transition: Transition,
    ) -> Result<()> {
        let duration = source
            .total_duration()
            .map(|duration| duration.as_secs_f64())
            .unwrap_or(0.0);
        let errors = source.error_handle();
        let format = (source.channels(), source.sample_rate());
        let (source, analysis) = AnalysisTap::new(source);
        let (source, stems) = stem_stage(source);
        let speed = self.state().speed;
        let placement = match &transition {
            Transition::Mix(mix) => self.place(mix, format.1).filter(|placed| {
                placed
                    .mix
                    .stems()
                    .is_none_or(|[_, incoming]| incoming.format() == format)
            }),
            _ => None,
        };
        let deck_speed = placement
            .as_ref()
            .map_or(speed, |placed| placed.mix.plan().speed * speed);
        let (mut source, tempo) = TempoSource::new(source, deck_speed)?;
        if let Some(placed) = &placement {
            source
                .try_seek(placed.entry)
                .map_err(|error| MediaError::Playback(error.to_string()))?;
        }
        let force = Arc::new(AtomicBool::new(false));
        let incoming = placement
            .as_ref()
            .zip(self.active.as_ref())
            .map(|(placed, outgoing)| {
                let plan = placed.mix.plan();
                // After a fusion, the incoming deck starts where it ends.
                let delay = plan.fusion.map_or(0.0, |fusion| fusion.length);
                DeckMix::incoming(
                    Arc::new(plan.incoming.clone()),
                    outgoing.tempo.clone(),
                    plan.exit_frame + seconds_to_frames(delay, plan.outgoing_rate),
                    delay,
                    plan.outgoing_rate,
                    force.clone(),
                    format,
                    plan.end(),
                )
            });
        let (mut source, mix) = mix_stage(source, incoming);
        let audible = self.active.as_ref().is_some_and(Deck::audible);
        let crossfade = match &transition {
            Transition::Crossfade(fade) if audible => Some(*fade),
            Transition::Mix(mix) if audible && placement.is_none() => mix.fallback(),
            _ => None,
        };
        // A blend starts the new song where its sound starts, never on its
        // leading silence.
        let start = crossfade.map_or(Duration::ZERO, Crossfade::start);
        if placement.is_none() && !start.is_zero() {
            source
                .try_seek(start)
                .map_err(|error| MediaError::Playback(error.to_string()))?;
        }
        let (source, envelope) = envelope(source, crossfade.is_some());
        let mut gate = None;
        let mut speed_ramp = None;
        if let Some(placed) = &placement {
            let plan = placed.mix.plan();
            if let (Some(rendering), Some(outgoing)) = (placed.mix.rendering(), &self.active) {
                self.start_rendering(rendering, plan, outgoing.tempo.clone(), placed.exit_at);
            }
            if let Some([outgoing_stem, incoming_stem]) = placed.mix.stems() {
                stems.start(DeckStem::new(
                    incoming_stem.clone(),
                    plan.incoming.vocals.clone(),
                    plan.incoming.instrumental.clone(),
                    plan.entry_frame,
                    plan.speed,
                ));
                if let Some(previous) = &self.active {
                    previous.stems.start(DeckStem::new(
                        outgoing_stem.clone(),
                        plan.outgoing.vocals.clone(),
                        plan.outgoing.instrumental.clone(),
                        plan.exit_frame,
                        1.0,
                    ));
                }
            }
            if let Some(previous) = self.active.take() {
                previous.mix.start(DeckMix::outgoing(
                    Arc::new(plan.outgoing.clone()),
                    previous.tempo.clone(),
                    plan.exit_frame,
                    previous.format,
                    plan.end(),
                ));
                self.fading.push(FadingDeck {
                    deck: previous,
                    until: placed.exit_at
                        + Duration::from_secs_f32(plan.end())
                        + FADE_RELEASE_MARGIN,
                });
            }
            self.trim_fading();
            let delay = plan.fusion.map_or(0.0, |fusion| fusion.length);
            gate = Some(Gate {
                force,
                deadline: placed.exit_at + Duration::from_secs_f32(delay) + GATE_GRACE,
            });
            self.blend = Some(BlendTiming {
                mix: true,
                start: placed.exit_at,
                end: placed.exit_at + Duration::from_secs_f32(plan.overlap),
            });
            if deck_speed != speed {
                let start = placed.exit_at + Duration::from_secs_f32(plan.overlap);
                speed_ramp = Some(SpeedRamp {
                    from: deck_speed,
                    start,
                    end: start + speed_return(deck_speed, speed),
                });
            }
        } else {
            match crossfade {
                Some(fade) => {
                    if let Some(previous) = self.active.take() {
                        previous.envelope.fade_out(fade);
                        self.fading.push(FadingDeck {
                            deck: previous,
                            until: Instant::now() + fade.duration() + FADE_RELEASE_MARGIN,
                        });
                    }
                    self.trim_fading();
                    envelope.fade_in(fade);
                    let now = Instant::now();
                    self.blend = Some(BlendTiming {
                        mix: false,
                        start: now,
                        end: now + fade.duration(),
                    });
                }
                None => self.release_all(),
            }
        }
        let sink = Sink::connect_new(self.output.mixer());
        let mut state = self.state();
        sink.set_volume(state.volume);
        sink.append(source);
        state.path = path;
        state.remote_id = remote.as_ref().map(|remote| remote.id.clone());
        state.streaming = remote.as_ref().map(|remote| remote.handle.snapshot());
        state.duration_seconds = duration;
        state.position_seconds = 0.0;
        state.finished = false;
        state.error = None;
        state.is_playing = true;
        state.analysis = None;
        drop(state);
        *self
            .stream_slot
            .lock()
            .unwrap_or_else(|error| error.into_inner()) =
            remote.as_ref().map(|remote| remote.handle.clone());
        self.active = Some(Deck {
            sink,
            envelope,
            tempo,
            mix,
            stems,
            format,
            gate,
            speed_ramp,
            errors,
            analysis,
            remote,
        });
        Ok(())
    }

    /// Play a fusion's `rendering` on its own sink from a seam before the
    /// exit frame of the deck whose clock is `gate`, at its gain lane, silent
    /// until then like an incoming deck.
    fn start_rendering(
        &mut self,
        rendering: &Rendering,
        plan: &yt_mix::MixPlan,
        gate: TempoControl,
        exit_at: Instant,
    ) {
        let Some(fusion) = plan.fusion else {
            return;
        };
        let Ok(gain) = fusion.gain() else {
            return;
        };
        let lanes = yt_mix::Lanes {
            gain,
            ..yt_mix::Lanes::unchanged()
        };
        let end = fusion.length + fusion.seam;
        let mix = DeckMix::incoming(
            Arc::new(lanes),
            gate,
            plan.exit_frame
                .saturating_sub(seconds_to_frames(fusion.seam, plan.outgoing_rate)),
            -fusion.seam,
            plan.outgoing_rate,
            Arc::new(AtomicBool::new(false)),
            (rendering.channels, rendering.rate),
            end,
        );
        let source = SamplesBuffer::new(
            rendering.channels,
            rendering.rate,
            rendering.samples.to_vec(),
        );
        let (source, _) = mix_stage(source, Some(mix));
        let sink = Sink::connect_new(self.output.mixer());
        sink.set_volume(self.state().volume);
        sink.append(source);
        self.renderings.push(FadingRendering {
            sink,
            until: exit_at + Duration::from_secs_f32(end) + FADE_RELEASE_MARGIN,
        });
    }

    /// Release the oldest fading decks beyond `MAX_FADING_DECKS`.
    fn trim_fading(&mut self) {
        while self.fading.len() > MAX_FADING_DECKS {
            self.fading.remove(0).deck.release();
        }
    }

    /// Release every fading deck and rendering: whatever blend was heard is
    /// over.
    fn release_fading(&mut self) {
        for fading in self.fading.drain(..) {
            fading.deck.release();
        }
        for rendering in self.renderings.drain(..) {
            rendering.sink.stop();
        }
        self.blend = None;
    }

    fn release_all(&mut self) {
        self.release_fading();
        if let Some(deck) = self.active.take() {
            deck.release();
        }
        self.stream_slot
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
    }

    fn sinks(&self) -> impl Iterator<Item = &Sink> {
        self.active
            .iter()
            .map(|deck| &deck.sink)
            .chain(self.fading.iter().map(|fading| &fading.deck.sink))
            .chain(self.renderings.iter().map(|rendering| &rendering.sink))
    }

    fn stop(&mut self, requested: Option<&SessionToken>) -> Result<()> {
        require_session(self.active_session(), requested)?;
        self.release_all();
        let mut state = self.state();
        state.path = None;
        state.remote_id = None;
        state.streaming = None;
        state.finished = false;
        state.position_seconds = 0.0;
        state.duration_seconds = 0.0;
        state.is_playing = false;
        state.analysis = None;
        Ok(())
    }

    /// A seek during a blend finishes the blend: the outgoing deck stops.
    fn seek(&mut self, position: Duration, requested: Option<&SessionToken>) -> Result<()> {
        require_session(self.active_session(), requested)?;
        if requested.is_some_and(|session| session.0.is_cancelled()) {
            return Err(MediaError::Cancelled);
        }
        self.release_fading();
        match &self.active {
            Some(deck) => deck
                .sink
                .try_seek(position)
                .map_err(|error| MediaError::Playback(error.to_string())),
            None => Ok(()),
        }
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::Play(path, transition, reply) => {
                // Decode before touching the current deck: a bad file must not
                // interrupt an otherwise working player.
                let result = crate::decode_file(&path)
                    .and_then(|source| self.install(source, Some(path), None, transition));
                let _ = reply.send(result);
            }
            Command::PlayReady(prepared, transition, reply) => {
                let PreparedInner {
                    source,
                    id,
                    handle,
                    token,
                } = *prepared;
                if token.0.is_cancelled() {
                    handle.cancel();
                    let _ = reply.send(Err(MediaError::Cancelled));
                    return;
                }
                let origin = RemoteOrigin {
                    id,
                    handle: handle.clone(),
                    session: token.clone(),
                };
                let result = self.install(source, None, Some(origin), transition);
                if result.is_err() {
                    token.0.cancel();
                    handle.cancel();
                }
                let _ = reply.send(result);
            }
            Command::Pause => self.sinks().for_each(Sink::pause),
            Command::Resume => self.sinks().for_each(Sink::play),
            Command::Stop(requested, reply) => {
                let result = self.stop(requested.as_ref());
                if let Some(reply) = reply {
                    let _ = reply.send(result);
                }
            }
            Command::Seek(position, requested, reply) => {
                let _ = reply.send(self.seek(position, requested.as_ref()));
            }
            Command::Volume(volume) => {
                self.sinks().for_each(|sink| sink.set_volume(volume));
                self.state().volume = volume;
            }
            Command::Speed(speed) => {
                if let Some(deck) = &mut self.active {
                    deck.tempo.set_speed(speed);
                    deck.speed_ramp = None;
                }
                self.state().speed = speed;
            }
            Command::Sleep(duration) => {
                self.sleep_at = duration.and_then(|duration| Instant::now().checked_add(duration));
            }
        }
    }

    /// Keep a paused mix or fade where it was: its deadlines move with the
    /// time spent paused.
    fn hold_while_paused(&mut self, now: Instant) {
        let paused = now.saturating_duration_since(self.last_tick);
        self.last_tick = now;
        let all_paused = self
            .active
            .as_ref()
            .is_some_and(|deck| deck.sink.is_paused());
        if !all_paused || paused.is_zero() {
            return;
        }
        for fading in &mut self.fading {
            fading.until += paused;
        }
        for rendering in &mut self.renderings {
            rendering.until += paused;
        }
        if let Some(blend) = &mut self.blend {
            blend.start += paused;
            blend.end += paused;
        }
        if let Some(deck) = &mut self.active {
            if let Some(gate) = &mut deck.gate {
                gate.deadline += paused;
            }
            if let Some(ramp) = &mut deck.speed_ramp {
                ramp.start += paused;
                ramp.end += paused;
            }
        }
    }

    /// Open a mix's gate once its deadline passes, and glide a matched deck
    /// back to the user's speed.
    fn advance_mix(&mut self, now: Instant) {
        let speed = self.state().speed;
        let Some(deck) = &mut self.active else {
            return;
        };
        if deck.gate.as_ref().is_some_and(|gate| now >= gate.deadline) {
            if let Some(gate) = deck.gate.take() {
                gate.force.store(true, Ordering::Relaxed);
            }
        }
        if let Some(ramp) = &deck.speed_ramp {
            if now >= ramp.start {
                let span = ramp.end.saturating_duration_since(ramp.start).as_secs_f32();
                let progress = if span > 0.0 {
                    (now.saturating_duration_since(ramp.start).as_secs_f32() / span).min(1.0)
                } else {
                    1.0
                };
                deck.tempo
                    .set_speed(ramp.from + (speed - ramp.from) * progress);
                if progress >= 1.0 {
                    deck.speed_ramp = None;
                }
            }
        }
    }

    /// Release decks whose fade ended, apply the sleep timer, and publish the
    /// active deck's state.
    fn tick(&mut self) {
        let now = Instant::now();
        self.hold_while_paused(now);
        self.advance_mix(now);
        let (expired, fading): (Vec<_>, Vec<_>) = self
            .fading
            .drain(..)
            .partition(|fading| now >= fading.until || fading.deck.sink.empty());
        self.fading = fading;
        for fading in expired {
            fading.deck.release();
        }
        self.renderings.retain(|rendering| {
            let playing = now < rendering.until && !rendering.sink.empty();
            if !playing {
                rendering.sink.stop();
            }
            playing
        });
        if self.sleep_at.is_some_and(|at| now >= at) {
            self.sinks().for_each(Sink::pause);
            self.sleep_at = None;
        }
        let active = self.active.as_ref();
        let streaming = active
            .and_then(|deck| deck.remote.as_ref())
            .map(|remote| remote.handle.snapshot());
        let error = active.and_then(|deck| {
            deck.errors
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
        });
        let analysis = active.and_then(|deck| {
            *deck
                .analysis
                .lock()
                .unwrap_or_else(|error| error.into_inner())
        });
        let empty = active.is_none_or(|deck| deck.sink.empty());
        let position = active.map_or(0.0, |deck| deck.tempo.position());
        let is_playing = active.is_some_and(Deck::audible);
        let crossfading = !self.fading.is_empty();
        let blend = self.blend.and_then(|timing| timing.at(now));
        if blend.is_none() {
            self.blend = None;
        }
        let mut state = self.state();
        state.is_playing = is_playing;
        state.finished = (state.path.is_some() || state.remote_id.is_some()) && empty;
        state.streaming = streaming;
        state.error = error;
        state.crossfading = crossfading;
        state.blend = blend;
        state.analysis = analysis;
        if state.path.is_some() || state.remote_id.is_some() {
            state.position_seconds = position;
        }
    }
}

/// Frames of `seconds` at `rate`.
fn seconds_to_frames(seconds: f32, rate: u32) -> u64 {
    (f64::from(seconds.max(0.0)) * f64::from(rate)).round() as u64
}

/// Cloneable handle. The audio device, decoders and decks live on one worker
/// thread; UI state never owns a platform audio handle and never polls through FFI.
#[derive(Clone)]
pub struct AudioPlayer {
    commands: mpsc::Sender<Command>,
    state: Arc<Mutex<PlaybackSnapshot>>,
    stream: Arc<Mutex<Option<StreamingHandle>>>,
}

impl AudioPlayer {
    pub fn new() -> Result<Self> {
        let (commands, receiver) = mpsc::channel();
        let state = Arc::new(Mutex::new(PlaybackSnapshot::default()));
        let worker_state = state.clone();
        let stream_slot = Arc::new(Mutex::new(None::<StreamingHandle>));
        let worker_slot = stream_slot.clone();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("ytu-audio".into())
            .spawn(move || {
                let mut output = match OutputStreamBuilder::open_default_stream() {
                    Ok(output) => output,
                    Err(error) => {
                        let _ = ready_tx.send(Err(MediaError::Playback(error.to_string())));
                        return;
                    }
                };
                output.log_on_drop(false);
                let mut worker = Worker {
                    output,
                    state: worker_state,
                    stream_slot: worker_slot,
                    active: None,
                    fading: Vec::new(),
                    renderings: Vec::new(),
                    sleep_at: None,
                    last_tick: Instant::now(),
                    blend: None,
                };
                let _ = ready_tx.send(Ok(()));
                loop {
                    match receiver.recv_timeout(COMMAND_POLL) {
                        Ok(command) => worker.handle(command),
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                    worker.tick();
                }
                worker.release_all();
            })
            .map_err(MediaError::Io)?;
        ready_rx
            .recv_timeout(ENGINE_TIMEOUT)
            .map_err(|_| MediaError::Playback("El dispositivo de audio no respondió.".into()))??;
        Ok(Self {
            commands,
            state,
            stream: stream_slot,
        })
    }

    /// Play a local file, replacing the current source with a cut.
    pub fn play(&self, path: impl AsRef<Path>) -> Result<()> {
        self.play_with(path, Transition::Cut)
    }

    /// Play a local file with `transition`. A crossfade only blends when the
    /// current source is audible; otherwise it starts like a cut.
    pub fn play_with(&self, path: impl AsRef<Path>, transition: Transition) -> Result<()> {
        self.request(|reply| Command::Play(path.as_ref().to_owned(), transition, reply))
    }

    /// Open a remote stream and start buffering it without making it audible.
    pub async fn prepare_stream(
        &self,
        media: ResolvedMedia,
        cancel: CancellationToken,
    ) -> Result<PreparedStream> {
        let id = media.media.id.clone();
        let token = SessionToken::new(cancel.child_token());
        let (source, handle) = crate::open_http_audio(media, (*token.0).clone()).await?;
        if token.0.is_cancelled() {
            handle.cancel();
            return Err(MediaError::Cancelled);
        }
        Ok(PreparedStream {
            inner: Some(PreparedInner {
                source,
                id,
                handle,
                token,
            }),
        })
    }

    /// Make a prepared stream audible with `transition`.
    pub async fn play_prepared(
        &self,
        mut prepared: PreparedStream,
        transition: Transition,
    ) -> Result<PlaybackSession> {
        let inner = prepared.inner.take().ok_or(MediaError::Cancelled)?;
        let token = inner.token.clone();
        let stream = inner.handle.clone();
        let player = self.clone();
        let result = tokio::task::spawn_blocking(move || {
            player.request(|reply| Command::PlayReady(Box::new(inner), transition, reply))
        })
        .await
        .map_err(|error| MediaError::Playback(error.to_string()))
        .and_then(|result| result);
        if let Err(error) = result {
            token.0.cancel();
            stream.cancel();
            return Err(error);
        }
        Ok(PlaybackSession {
            player: self.clone(),
            token,
            stream,
        })
    }

    /// Open and play a remote stream with a cut.
    pub async fn play_stream(
        &self,
        media: ResolvedMedia,
        cancel: CancellationToken,
    ) -> Result<PlaybackSession> {
        let prepared = self.prepare_stream(media, cancel).await?;
        self.play_prepared(prepared, Transition::Cut).await
    }
    pub fn pause(&self) -> Result<()> {
        self.send(Command::Pause)
    }
    pub fn resume(&self) -> Result<()> {
        self.send(Command::Resume)
    }
    pub fn stop(&self) -> Result<()> {
        // Interrupt network reads immediately, including a seek waiting on the
        // device callback, rather than queueing cancellation behind that seek.
        if let Some(stream) = self.stream.lock().unwrap_or_else(|e| e.into_inner()).take() {
            stream.cancel();
        }
        self.send(Command::Stop(None, None))
    }
    pub fn seek(&self, seconds: f64) -> Result<()> {
        let position = seek_position(seconds)?;
        self.request(|reply| Command::Seek(position, None, reply))
    }
    pub fn set_volume(&self, value: f32) -> Result<()> {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(MediaError::InvalidInput(
                "El volumen debe estar entre 0 y 1.".into(),
            ));
        }
        self.send(Command::Volume(value))
    }
    /// Change tempo while preserving pitch. Position remains in source seconds.
    pub fn set_speed(&self, value: f32) -> Result<()> {
        if !value.is_finite() || !(0.25..=4.0).contains(&value) {
            return Err(MediaError::InvalidInput(
                "La velocidad debe estar entre 0.25 y 4.".into(),
            ));
        }
        self.send(Command::Speed(value))
    }
    pub fn set_sleep_timer(&self, duration: Option<Duration>) -> Result<()> {
        self.send(Command::Sleep(duration))
    }
    pub fn snapshot(&self) -> PlaybackSnapshot {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    fn send(&self, command: Command) -> Result<()> {
        self.commands
            .send(command)
            .map_err(|_| MediaError::Playback("El motor de audio se ha detenido.".into()))
    }
    fn request(&self, command: impl FnOnce(Reply) -> Command) -> Result<()> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.send(command(tx))?;
        rx.recv_timeout(ENGINE_TIMEOUT)
            .map_err(|_| MediaError::Playback("El motor de audio no respondió.".into()))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_matched_song_returns_to_its_tempo_slowly_enough_to_go_unnoticed() {
        assert_eq!(speed_return(1.01, 1.0), MIN_SPEED_RETURN);
        let far = speed_return(0.9, 1.0);
        assert!(
            (far.as_secs_f32() - 40.0).abs() < 0.01,
            "ten percent takes forty seconds: {far:?}"
        );
        assert_eq!(speed_return(1.0, 1.0), MIN_SPEED_RETURN);
        assert_eq!(
            speed_return(1.3, 1.0),
            MAX_SPEED_RETURN,
            "a fusion's larger change does not take minutes"
        );
    }
    use crate::analysis::tests::pcm16_wav;
    use rodio::{buffer::SamplesBuffer, Decoder};

    fn quiet_wav() -> Vec<u8> {
        pcm16_wav(8000, &[0; 800])
    }

    #[test]
    fn a_blend_reports_its_progress_until_it_ends() {
        let start = Instant::now();
        let timing = BlendTiming {
            mix: true,
            start: start + Duration::from_secs(1),
            end: start + Duration::from_secs(5),
        };
        assert_eq!(
            timing.at(start),
            Some(Blend {
                mix: true,
                progress: 0.0
            }),
            "a mix waiting for its exit is shown at its start"
        );
        let halfway = timing.at(start + Duration::from_secs(3)).unwrap();
        assert!((halfway.progress - 0.5).abs() < 1e-6);
        assert_eq!(timing.at(start + Duration::from_secs(5)), None);
    }

    /// The real engine on the default output device, muted: a mix armed one
    /// second before its exit starts the next song at its entry, reports a
    /// blend while the songs overlap, and releases the outgoing song after.
    #[test]
    #[ignore = "needs an audio output device"]
    fn an_armed_mix_starts_the_next_song_at_its_entry_and_releases_the_previous() {
        let folder = tempfile::tempdir().unwrap();
        let rate = 8_000_u32;
        let tone = |hz: f32| -> Vec<i16> {
            (0..rate * 20)
                .map(|n| {
                    ((n as f32 * hz * std::f32::consts::TAU / rate as f32).sin() * 8_000.0) as i16
                })
                .collect()
        };
        let (first, second) = (folder.path().join("a.wav"), folder.path().join("b.wav"));
        std::fs::write(&first, pcm16_wav(rate, &tone(220.0))).unwrap();
        std::fs::write(&second, pcm16_wav(rate, &tone(330.0))).unwrap();
        let player = AudioPlayer::new().unwrap();
        player.set_volume(0.0).unwrap();
        player.play(&first).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        let exit = player.snapshot().position_seconds + 1.0;
        let overlap = 2.0_f32;
        let lanes = |gain: Vec<(f32, f32)>| yt_mix::Lanes {
            gain: yt_mix::Curve::new(gain).unwrap(),
            ..yt_mix::Lanes::unchanged()
        };
        let plan = yt_mix::MixPlan {
            technique: yt_mix::Technique::Relaxed,
            outgoing_rate: rate,
            incoming_rate: rate,
            exit_frame: (exit * f64::from(rate)) as u64,
            entry_frame: u64::from(rate) * 3,
            speed: 1.0,
            overlap,
            outgoing: lanes(vec![(0.0, 1.0), (overlap, 0.0)]),
            incoming: lanes(vec![(0.0, 0.0), (overlap, 1.0)]),
            report: yt_mix::MixReport {
                beat_error_ms: None,
                bass_overlap_seconds: 0.0,
                loudness_step_lu: None,
            },
            fusion: None,
            vocal_shift: None,
        };
        player
            .play_with(&second, Transition::Mix(Mix::new(plan).unwrap()))
            .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let waiting = player.snapshot();
        assert_eq!(waiting.path.as_deref(), Some(second.as_path()));
        assert!(waiting.crossfading, "the playing song is still mixing out");
        assert_eq!(
            waiting.blend.map(|blend| blend.mix),
            Some(true),
            "the interface already shows the coming mix"
        );
        assert!(
            (waiting.position_seconds - 3.0).abs() < 0.05,
            "the next song waits at its entry: {}",
            waiting.position_seconds
        );
        std::thread::sleep(Duration::from_millis(1_700));
        let mixing = player.snapshot();
        assert!(
            (mixing.position_seconds - 4.0).abs() < 0.3,
            "about a second after the exit: {}",
            mixing.position_seconds
        );
        std::thread::sleep(Duration::from_secs(3));
        assert!(
            !player.snapshot().crossfading,
            "the previous song was released"
        );
        assert_eq!(player.snapshot().blend, None);
        player.stop().unwrap();
    }

    #[test]
    fn analysis_tap_never_waits_for_the_engine_and_keeps_measuring_across_seeks() {
        const RATE: u32 = 8_000;
        const SECONDS: u32 = 8;
        let samples: Vec<f32> = (0..RATE * SECONDS)
            .map(|index| {
                let time = index as f32 / RATE as f32;
                (std::f32::consts::TAU * 440.0 * time).sin() * 0.25
            })
            .collect();
        let (mut tap, slot) = AnalysisTap::new(SamplesBuffer::new(1, RATE, samples.clone()));
        let played: Vec<f32> = {
            // The engine thread holds the slot: the audio side must keep
            // producing samples and simply skip its publications.
            let held = slot.lock().unwrap();
            let played = tap.by_ref().take((RATE * 7) as usize).collect();
            assert!(held.is_none());
            played
        };
        assert_eq!(played, samples[..(RATE * 7) as usize]);
        tap.try_seek(Duration::from_secs(1)).unwrap();
        assert_eq!(tap.by_ref().count(), (RATE * 7) as usize);
        let features = slot
            .lock()
            .unwrap()
            .expect("published once the engine let go");
        assert!(
            (features.analyzed_seconds - 14.0).abs() < 1e-6,
            "{features:?}"
        );
        assert!(features.energy > 0.5, "{features:?}");
        assert_eq!(features.onset_rate, 0.0, "{features:?}");
    }

    #[test]
    fn native_decoder_reads_wav_without_audio_device() {
        let source = Decoder::try_from(std::io::Cursor::new(quiet_wav())).unwrap();
        assert_eq!(source.channels(), 1);
        assert_eq!(source.sample_rate(), 8000);
        assert_eq!(source.count(), 800);
    }

    #[test]
    fn native_decoder_rejects_invalid_media() {
        assert!(Decoder::try_from(std::io::Cursor::new(vec![0u8; 64])).is_err());
    }

    #[test]
    fn queued_commands_cannot_control_replacement_playback() {
        let previous = SessionToken::new(CancellationToken::new());
        let replacement = SessionToken::new(CancellationToken::new());
        // The actor receives a delayed seek/stop only after installing another
        // remote playback. Equal URLs are deliberately irrelevant to ownership.
        let (sender, receiver) = mpsc::channel();
        let (reply, _) = mpsc::sync_channel(1);
        sender
            .send(Command::Seek(
                Duration::from_secs(40),
                Some(previous.clone()),
                reply,
            ))
            .unwrap();
        sender
            .send(Command::Stop(Some(previous.clone()), None))
            .unwrap();
        for _ in 0..2 {
            let requested = match receiver.recv().unwrap() {
                Command::Seek(_, requested, _) | Command::Stop(requested, _) => requested,
                _ => panic!("unexpected command"),
            };
            assert!(matches!(
                require_session(Some(&replacement), requested.as_ref()),
                Err(MediaError::Cancelled)
            ));
        }
        assert!(require_session(Some(&replacement), Some(&replacement.clone())).is_ok());
        // A local track or a stopped actor has no remote owner either.
        assert!(require_session(None, Some(&previous)).is_err());
        assert!(require_session(Some(&replacement), None).is_ok());
        previous.0.cancel();
        assert!(!replacement.0.is_cancelled());
    }

    #[test]
    fn session_seek_rejects_invalid_positions_before_queueing() {
        for position in [f64::NAN, f64::INFINITY, -0.1, 31_536_001.0] {
            assert!(seek_position(position).is_err());
        }
        assert_eq!(seek_position(2.25).unwrap(), Duration::from_millis(2250));
    }
}
