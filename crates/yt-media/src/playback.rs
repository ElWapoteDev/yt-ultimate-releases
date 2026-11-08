use crate::tempo::{TempoControl, TempoSource};
use crate::{
    CancellationToken, DecodedAudio, MediaError, ResolvedMedia, Result, StreamingHandle,
    StreamingSnapshot,
};
use rodio::{OutputStreamBuilder, Sink, Source};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{mpsc, Arc, Mutex},
    time::{Duration, Instant},
};

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
    if !seconds.is_finite() || !(0.0..=31_536_000.0).contains(&seconds) {
        return Err(MediaError::InvalidInput(
            "La posición de reproducción no es válida.".into(),
        ));
    }
    Ok(Duration::from_secs_f64(seconds))
}

enum Command {
    Play(PathBuf, Reply),
    PlayReady(
        Box<DecodedAudio>,
        String,
        StreamingHandle,
        SessionToken,
        Reply,
    ),
    Pause,
    Resume,
    Stop(Option<SessionToken>, Option<Reply>),
    Seek(Duration, Option<SessionToken>, Reply),
    Volume(f32),
    Speed(f32),
    Sleep(Option<Duration>),
}

/// Cloneable handle. The audio device and decoder live on one worker thread;
/// UI state never owns a platform audio handle and never polls through FFI.
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
        let stream_cancel = Arc::new(Mutex::new(None::<StreamingHandle>));
        let worker_cancel = stream_cancel.clone();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("ytu-audio".into())
            .spawn(move || {
                let mut stream = match OutputStreamBuilder::open_default_stream() {
                    Ok(stream) => stream,
                    Err(error) => {
                        let _ = ready_tx.send(Err(MediaError::Playback(error.to_string())));
                        return;
                    }
                };
                stream.log_on_drop(false);
                let mut sink = Sink::connect_new(stream.mixer());
                sink.set_volume(0.8);
                let _ = ready_tx.send(Ok(()));
                let mut sleep_at: Option<Instant> = None;
                let mut decoding_error: Option<Arc<Mutex<Option<String>>>> = None;
                let mut active_stream: Option<StreamingHandle> = None;
                let mut active_session: Option<SessionToken> = None;
                let mut tempo_control: Option<TempoControl> = None;
                loop {
                    match receiver.recv_timeout(Duration::from_millis(100)) {
                        Ok(Command::Play(path, reply)) => {
                            // Decode before stopping the current track: a bad file
                            // must not interrupt an otherwise working player.
                            let decoded = crate::decode_file(&path);
                            match decoded {
                                Ok(source) => {
                                    let duration = source
                                        .total_duration()
                                        .map(|d| d.as_secs_f64())
                                        .unwrap_or(0.0);
                                    let errors = source.error_handle();
                                    let speed = worker_state
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner())
                                        .speed;
                                    let (source, control) = match TempoSource::new(source, speed) {
                                        Ok(value) => value,
                                        Err(error) => {
                                            let _ = reply.send(Err(error));
                                            continue;
                                        }
                                    };
                                    if let Some(session) = active_session.take() {
                                        session.0.cancel();
                                    }
                                    if let Some(stream) = active_stream.take() {
                                        stream.cancel();
                                    }
                                    worker_cancel
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner())
                                        .take();
                                    sink.stop();
                                    sink = Sink::connect_new(stream.mixer());
                                    let mut state =
                                        worker_state.lock().unwrap_or_else(|e| e.into_inner());
                                    sink.set_volume(state.volume);
                                    tempo_control = Some(control);
                                    decoding_error = Some(errors);
                                    sink.append(source);
                                    state.path = Some(path);
                                    state.remote_id = None;
                                    state.streaming = None;
                                    state.duration_seconds = duration;
                                    state.position_seconds = 0.0;
                                    state.finished = false;
                                    state.error = None;
                                    state.is_playing = true;
                                    let _ = reply.send(Ok(()));
                                }
                                Err(error) => {
                                    let _ = reply.send(Err(error));
                                }
                            }
                        }
                        Ok(Command::PlayReady(source, id, handle, session, reply)) => {
                            if session.0.is_cancelled() {
                                handle.cancel();
                                let _ = reply.send(Err(MediaError::Cancelled));
                                continue;
                            }
                            let duration = source
                                .total_duration()
                                .map(|d| d.as_secs_f64())
                                .unwrap_or(0.0);
                            let errors = source.error_handle();
                            let speed =
                                worker_state.lock().unwrap_or_else(|e| e.into_inner()).speed;
                            let (source, control) = match TempoSource::new(*source, speed) {
                                Ok(value) => value,
                                Err(error) => {
                                    handle.cancel();
                                    let _ = reply.send(Err(error));
                                    continue;
                                }
                            };
                            if let Some(previous) = active_session.replace(session) {
                                previous.0.cancel();
                            }
                            if let Some(stream) = active_stream.take() {
                                stream.cancel();
                            }
                            sink.stop();
                            sink = Sink::connect_new(stream.mixer());
                            let mut state = worker_state.lock().unwrap_or_else(|e| e.into_inner());
                            sink.set_volume(state.volume);
                            tempo_control = Some(control);
                            decoding_error = Some(errors);
                            sink.append(source);
                            state.path = None;
                            state.remote_id = Some(id);
                            state.streaming = Some(handle.snapshot());
                            state.duration_seconds = duration;
                            state.position_seconds = 0.0;
                            state.finished = false;
                            state.error = None;
                            state.is_playing = true;
                            *worker_cancel.lock().unwrap_or_else(|e| e.into_inner()) =
                                Some(handle.clone());
                            active_stream = Some(handle);
                            let _ = reply.send(Ok(()));
                        }
                        Ok(Command::Pause) => sink.pause(),
                        Ok(Command::Resume) => sink.play(),
                        Ok(Command::Stop(requested, reply)) => {
                            if let Err(error) =
                                require_session(active_session.as_ref(), requested.as_ref())
                            {
                                if let Some(reply) = reply {
                                    let _ = reply.send(Err(error));
                                }
                                continue;
                            }
                            if let Some(session) = active_session.take() {
                                session.0.cancel();
                            }
                            worker_cancel
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .take();
                            if let Some(stream) = active_stream.take() {
                                stream.cancel();
                            }
                            sink.stop();
                            decoding_error = None;
                            tempo_control = None;
                            let mut state = worker_state.lock().unwrap_or_else(|e| e.into_inner());
                            state.path = None;
                            state.remote_id = None;
                            state.streaming = None;
                            state.finished = false;
                            state.position_seconds = 0.0;
                            state.duration_seconds = 0.0;
                            state.is_playing = false;
                            if let Some(reply) = reply {
                                let _ = reply.send(Ok(()));
                            }
                        }
                        Ok(Command::Seek(position, requested, reply)) => {
                            if let Err(error) =
                                require_session(active_session.as_ref(), requested.as_ref())
                            {
                                let _ = reply.send(Err(error));
                                continue;
                            }
                            if requested
                                .as_ref()
                                .is_some_and(|session| session.0.is_cancelled())
                            {
                                let _ = reply.send(Err(MediaError::Cancelled));
                                continue;
                            }
                            let result = sink
                                .try_seek(position)
                                .map_err(|e| MediaError::Playback(e.to_string()));
                            let _ = reply.send(result);
                        }
                        Ok(Command::Volume(volume)) => {
                            sink.set_volume(volume);
                            worker_state
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .volume = volume;
                        }
                        Ok(Command::Speed(speed)) => {
                            if let Some(control) = &tempo_control {
                                control.set_speed(speed);
                            }
                            worker_state.lock().unwrap_or_else(|e| e.into_inner()).speed = speed;
                        }
                        Ok(Command::Sleep(duration)) => {
                            sleep_at =
                                duration.and_then(|duration| Instant::now().checked_add(duration));
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                    if sleep_at.is_some_and(|at| Instant::now() >= at) {
                        sink.pause();
                        sleep_at = None;
                    }
                    let mut state = worker_state.lock().unwrap_or_else(|e| e.into_inner());
                    state.is_playing = !sink.is_paused() && !sink.empty();
                    state.finished =
                        (state.path.is_some() || state.remote_id.is_some()) && sink.empty();
                    state.streaming = active_stream.as_ref().map(StreamingHandle::snapshot);
                    state.error = decoding_error
                        .as_ref()
                        .and_then(|error| error.lock().unwrap_or_else(|e| e.into_inner()).clone());
                    if state.path.is_some() || state.remote_id.is_some() {
                        state.position_seconds =
                            tempo_control.as_ref().map_or(0.0, TempoControl::position);
                    }
                }
                if let Some(stream) = active_stream {
                    stream.cancel();
                }
            })
            .map_err(MediaError::Io)?;
        ready_rx
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| MediaError::Playback("El dispositivo de audio no respondió.".into()))??;
        Ok(Self {
            commands,
            state,
            stream: stream_cancel,
        })
    }

    pub fn play(&self, path: impl AsRef<Path>) -> Result<()> {
        self.request(|reply| Command::Play(path.as_ref().to_owned(), reply))
    }
    pub async fn play_stream(
        &self,
        media: ResolvedMedia,
        cancel: CancellationToken,
    ) -> Result<PlaybackSession> {
        let id = media.media.id.clone();
        let token = SessionToken::new(cancel.child_token());
        let (source, handle) = crate::open_http_audio(media, (*token.0).clone()).await?;
        if token.0.is_cancelled() {
            handle.cancel();
            return Err(MediaError::Cancelled);
        }
        let player = self.clone();
        let command_handle = handle.clone();
        let command_token = token.clone();
        let result = tokio::task::spawn_blocking(move || {
            player.request(|reply| {
                Command::PlayReady(Box::new(source), id, command_handle, command_token, reply)
            })
        })
        .await
        .map_err(|error| MediaError::Playback(error.to_string()))
        .and_then(|result| result);
        if let Err(error) = result {
            token.0.cancel();
            handle.cancel();
            return Err(error);
        }
        Ok(PlaybackSession {
            player: self.clone(),
            token,
            stream: handle,
        })
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
        rx.recv_timeout(Duration::from_secs(10))
            .map_err(|_| MediaError::Playback("El motor de audio no respondió.".into()))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::Decoder;

    fn quiet_wav() -> Vec<u8> {
        let mut wav = Vec::new();
        let samples = 800u32;
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + samples * 2).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&8000u32.to_le_bytes());
        wav.extend_from_slice(&16000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(samples * 2).to_le_bytes());
        wav.resize(44 + samples as usize * 2, 0);
        wav
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
