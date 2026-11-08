use crate::{MediaError, Result};
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

enum Command {
    Play(PathBuf, Reply),
    Pause,
    Resume,
    Stop,
    Seek(Duration, Reply),
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
}

impl AudioPlayer {
    pub fn new() -> Result<Self> {
        let (commands, receiver) = mpsc::channel();
        let state = Arc::new(Mutex::new(PlaybackSnapshot::default()));
        let worker_state = state.clone();
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
                                    sink.stop();
                                    sink = Sink::connect_new(stream.mixer());
                                    let mut state =
                                        worker_state.lock().unwrap_or_else(|e| e.into_inner());
                                    sink.set_volume(state.volume);
                                    sink.set_speed(state.speed);
                                    decoding_error = Some(source.error_handle());
                                    sink.append(source);
                                    state.path = Some(path);
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
                        Ok(Command::Pause) => sink.pause(),
                        Ok(Command::Resume) => sink.play(),
                        Ok(Command::Stop) => {
                            sink.stop();
                            decoding_error = None;
                            let mut state = worker_state.lock().unwrap_or_else(|e| e.into_inner());
                            state.path = None;
                            state.finished = false;
                            state.position_seconds = 0.0;
                            state.duration_seconds = 0.0;
                        }
                        Ok(Command::Seek(position, reply)) => {
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
                            sink.set_speed(speed);
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
                    state.finished = state.path.is_some() && sink.empty();
                    state.error = decoding_error
                        .as_ref()
                        .and_then(|error| error.lock().unwrap_or_else(|e| e.into_inner()).clone());
                    if state.path.is_some() {
                        state.position_seconds = sink.get_pos().as_secs_f64();
                    }
                }
            })
            .map_err(MediaError::Io)?;
        ready_rx
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| MediaError::Playback("El dispositivo de audio no respondió.".into()))??;
        Ok(Self { commands, state })
    }

    pub fn play(&self, path: impl AsRef<Path>) -> Result<()> {
        self.request(|reply| Command::Play(path.as_ref().to_owned(), reply))
    }
    pub fn pause(&self) -> Result<()> {
        self.send(Command::Pause)
    }
    pub fn resume(&self) -> Result<()> {
        self.send(Command::Resume)
    }
    pub fn stop(&self) -> Result<()> {
        self.send(Command::Stop)
    }
    pub fn seek(&self, seconds: f64) -> Result<()> {
        if !seconds.is_finite() || !(0.0..=31_536_000.0).contains(&seconds) {
            return Err(MediaError::InvalidInput(
                "La posición de reproducción no es válida.".into(),
            ));
        }
        self.request(|reply| Command::Seek(Duration::from_secs_f64(seconds), reply))
    }
    pub fn set_volume(&self, value: f32) -> Result<()> {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(MediaError::InvalidInput(
                "El volumen debe estar entre 0 y 1.".into(),
            ));
        }
        self.send(Command::Volume(value))
    }
    /// Playback-rate adjustment changes pitch as well as speed.
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
}
