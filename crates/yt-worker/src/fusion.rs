//! AI fusions of two songs (`yt_mix::Fusion`): the canvas a generative model
//! renders again, and the run of the model that renders it.
//!
//! The canvas holds a few bars of the outgoing song alone, then both songs
//! together over the fusion, beat-matched (the incoming one stretched to the
//! mix's speed), the outgoing one falling and the incoming one rising with
//! equal power, then a few bars of the incoming song alone. The model plays the
//! whole canvas again from part noise (audio to audio), so the blend becomes
//! one piece that still keeps both songs' beat and harmony. The clip handed
//! back covers the fusion plus one seam on each side, where the engine
//! crossfades with the real songs. See `docs/AI_DJ.md` §4.4.

use rodio::{buffer::SamplesBuffer, source::UniformSourceIterator};
use std::{
    f32::consts::FRAC_PI_2,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use thiserror::Error;
use yt_core::{KeyEstimate, KeyMode};
use yt_mix::Fusion;

/// Sample rate of every canvas and clip, the model's own, in Hz.
pub const FUSION_RATE: u32 = 44_100;
/// Channels of every canvas and clip.
pub const FUSION_CHANNELS: u16 = 2;
/// Fastest change of the incoming song's speed a fusion accepts, either way.
pub const FUSION_SPEEDS: std::ops::RangeInclusive<f32> = 0.5..=2.0;
/// Largest level correction of a clip, as a gain factor (±6 dB).
const MAX_LEVEL_GAIN: f32 = 2.0;
/// Longest run of the model before it is stopped.
const MAX_RUN: Duration = Duration::from_secs(240);
/// How often a running model is checked for cancellation.
const POLL: Duration = Duration::from_millis(50);
/// Longest error detail kept from the model's log, in characters.
const MAX_LOG_CHARS: usize = 200;
/// Extra seconds of the incoming song read for the stretch, so SoundTouch's
/// latency never leaves the canvas short.
const STRETCH_MARGIN_SECONDS: f32 = 0.5;
/// Note names for the model's prompt, from C.
const NOTES: [&str; 12] = [
    "C", "C#", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B",
];

/// Why a fusion could not be made.
#[derive(Debug, Error)]
pub enum FusionError {
    /// The fusion or the positions do not fit the songs.
    #[error("the fusion does not fit the songs: {0}")]
    Layout(&'static str),
    /// The audio could not be prepared.
    #[error("the audio cannot be prepared: {0}")]
    Audio(String),
    /// The model failed or returned unusable audio.
    #[error("the model failed: {0}")]
    Model(String),
    /// Stopped before it finished.
    #[error("the fusion was cancelled")]
    Cancelled,
}

/// Interleaved audio of one song, as decoded.
#[derive(Debug, Clone, Copy)]
pub struct Audio<'a> {
    pub samples: &'a [f32],
    pub channels: u16,
    pub rate: u32,
}

/// The audio the model renders, at [`FUSION_RATE`] in stereo.
#[derive(Debug, Clone)]
pub struct Canvas {
    samples: Vec<f32>,
    fusion: Fusion,
}

impl Canvas {
    /// The canvas of `fusion` from `outgoing` at source frame `exit` into
    /// `incoming` at source frame `entry` (where the incoming deck starts
    /// after the fusion), the incoming song played at `speed`. Fails when the
    /// fusion is out of bounds, the outgoing song is too short around its
    /// exit, or the incoming song around its entry.
    pub fn new(
        outgoing: Audio<'_>,
        exit: u64,
        incoming: Audio<'_>,
        entry: u64,
        speed: f32,
        fusion: Fusion,
    ) -> Result<Self, FusionError> {
        fusion
            .validate()
            .map_err(|_| FusionError::Layout("a fusion value is out of range"))?;
        if !(speed.is_finite() && FUSION_SPEEDS.contains(&speed)) {
            return Err(FusionError::Layout("the speed is out of range"));
        }
        let (before, length, after) = (
            frames(fusion.before),
            frames(fusion.length),
            frames(fusion.after),
        );
        let out_rate = outgoing.rate;
        let first = exit.checked_sub(source_frames(fusion.before, out_rate));
        let window = source_window(
            outgoing,
            first,
            exit + source_frames(fusion.length, out_rate) + 1,
        )?;
        let outgoing = to_canvas(window, outgoing.channels, outgoing.rate)?;
        let in_rate = incoming.rate;
        let first = entry.checked_sub(source_frames(fusion.length * speed, in_rate));
        let end = entry + source_frames((fusion.after + STRETCH_MARGIN_SECONDS) * speed, in_rate);
        let window = source_window(incoming, first, end)?;
        let stretched = yt_media::stretch(window.to_vec(), incoming.channels, in_rate, speed)
            .map_err(|error| FusionError::Audio(error.to_string()))?;
        let incoming = to_canvas(&stretched, incoming.channels, in_rate)?;
        let channels = usize::from(FUSION_CHANNELS);
        if outgoing.len() < (before + length) * channels
            || incoming.len() < (length + after) * channels
        {
            return Err(FusionError::Layout("a song is too short around the fusion"));
        }
        let total = before + length + after;
        let mut samples = vec![0.0_f32; total * channels];
        for frame in 0..total {
            // Equal power across the fusion: the outgoing song alone before
            // it, the incoming song alone after it.
            let progress = frame.saturating_sub(before).min(length) as f32 / length as f32;
            let (fall, rise) = ((progress * FRAC_PI_2).cos(), (progress * FRAC_PI_2).sin());
            for channel in 0..channels {
                let index = frame * channels + channel;
                let from_outgoing = outgoing.get(index).map_or(0.0, |sample| sample * fall);
                let from_incoming = frame
                    .checked_sub(before)
                    .and_then(|offset| incoming.get(offset * channels + channel))
                    .map_or(0.0, |sample| sample * rise);
                samples[index] = from_outgoing + from_incoming;
            }
        }
        Ok(Self { samples, fusion })
    }

    /// Interleaved stereo samples at [`FUSION_RATE`].
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    pub fn fusion(&self) -> Fusion {
        self.fusion
    }

    /// The clip from the model's `output` (the whole canvas rendered again,
    /// same rate and channels): the fusion with one seam on each side, its
    /// level matched to the canvas's. Fails for an output shorter than the
    /// canvas or with a sample that is not finite.
    pub fn clip(&self, output: &[f32]) -> Result<Vec<f32>, FusionError> {
        let channels = usize::from(FUSION_CHANNELS);
        let canvas = self.samples.len();
        if output.len() < canvas || output.iter().any(|sample| !sample.is_finite()) {
            return Err(FusionError::Model(
                "the output does not cover the canvas".into(),
            ));
        }
        let energy = |samples: &[f32]| {
            samples
                .iter()
                .map(|sample| f64::from(*sample).powi(2))
                .sum::<f64>()
        };
        let (songs, model) = (energy(&self.samples), energy(&output[..canvas]));
        let gain = if model > 0.0 && songs > 0.0 {
            ((songs / model).sqrt() as f32).clamp(1.0 / MAX_LEVEL_GAIN, MAX_LEVEL_GAIN)
        } else {
            1.0
        };
        let start = (frames(self.fusion.before) - frames(self.fusion.seam)) * channels;
        let end = start + self.fusion.clip_frames(FUSION_RATE) as usize * channels;
        Ok(output[start..end]
            .iter()
            .map(|sample| sample * gain)
            .collect())
    }
}

/// The prompt that describes a fusion at `tempo_bpm` in `key` to the model.
pub fn prompt(tempo_bpm: f32, key: Option<KeyEstimate>) -> String {
    let mut prompt = format!("seamless DJ mashup of two songs, {:.0} BPM", tempo_bpm);
    if let Some(key) = key {
        let note = NOTES[usize::from(u8::from(key.tonic)) % NOTES.len()];
        let mode = match key.mode {
            KeyMode::Major => "major",
            KeyMode::Minor => "minor",
        };
        prompt.push_str(&format!(", {note} {mode}"));
    }
    prompt
}

/// The model that renders canvases: sa3.cpp's `sa3-generate` program running
/// Stable Audio 3 Medium from a folder of its GGUF weights, on the GPU.
#[derive(Debug, Clone)]
pub struct Generator {
    program: PathBuf,
    models: PathBuf,
}

impl Generator {
    /// Model weights of Stable Audio 3 Medium the folder must hold.
    pub const MODEL_FILES: [&'static str; 5] = [
        "stable-audio-3-medium-dit-1.5B-v1.0-F16.gguf",
        "stable-audio-3-medium-same-l-v1.0-F16.gguf",
        "stable-audio-3-medium-conditioner-v1.0-F32.gguf",
        "t5gemma-b-b-ul2-encoder-0.3B-v1.0-F16.gguf",
        "t5gemma-b-b-ul2-v1.0-vocab.gguf",
    ];

    /// The generator at `program` with the weights in `models`, or `None`
    /// when either is missing.
    pub fn find(program: PathBuf, models: PathBuf) -> Option<Self> {
        let complete = program.is_file()
            && Self::MODEL_FILES
                .iter()
                .all(|file| models.join(file).is_file());
        complete.then_some(Self { program, models })
    }

    /// The program the generator runs.
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// The folder of the model's weights.
    pub fn models(&self) -> &Path {
        &self.models
    }

    /// Render `canvas` again from the noise of its fusion's strength and
    /// return its clip ([`Canvas::clip`]). `scratch` is an existing folder for
    /// the model's input and output files, removed before returning.
    /// `keep_going` is asked every 50 ms while the model runs; `false` stops
    /// it.
    pub fn render(
        &self,
        canvas: &Canvas,
        prompt: &str,
        seed: u32,
        steps: u32,
        scratch: &Path,
        mut keep_going: impl FnMut() -> bool,
    ) -> Result<Vec<f32>, FusionError> {
        let files = Files::new(scratch, seed)?;
        write_wav(&files.canvas, canvas.samples())?;
        let log =
            fs::File::create(&files.log).map_err(|error| FusionError::Audio(error.to_string()))?;
        let mut child = Command::new(&self.program)
            .arg("--model")
            .arg("medium")
            .arg("--models-dir")
            .arg(&self.models)
            .arg("--init")
            .arg(&files.canvas)
            .arg("--init-noise-level")
            .arg(format!("{:.3}", canvas.fusion.strength))
            // The clip's level is matched to the songs afterwards.
            .args(["--no-peak-normalize", "--no-limiter"])
            .arg("--steps")
            .arg(steps.to_string())
            .arg("--seed")
            .arg(seed.to_string())
            .arg("--prompt")
            .arg(prompt)
            .arg("--out")
            .arg(&files.output)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .map_err(|error| {
                FusionError::Model(format!("the model cannot start: {}", error.kind()))
            })?;
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(error) => return Err(FusionError::Model(error.to_string())),
            }
            let late = started.elapsed() > MAX_RUN;
            if late || !keep_going() {
                let _ = child.kill();
                let _ = child.wait();
                return Err(if late {
                    FusionError::Model("the model took too long".into())
                } else {
                    FusionError::Cancelled
                });
            }
            thread::sleep(POLL);
        };
        if !status.success() {
            return Err(FusionError::Model(files.log_tail()));
        }
        canvas.clip(&read_wav(&files.output)?)
    }
}

/// The model's files for one run, removed when dropped.
struct Files {
    canvas: PathBuf,
    output: PathBuf,
    log: PathBuf,
}

impl Files {
    fn new(scratch: &Path, seed: u32) -> Result<Self, FusionError> {
        if !scratch.is_dir() {
            return Err(FusionError::Audio(
                "the scratch folder does not exist".into(),
            ));
        }
        let name =
            |suffix: &str| scratch.join(format!("fusion-{}-{seed}.{suffix}", std::process::id()));
        Ok(Self {
            canvas: name("canvas.wav"),
            output: name("output.wav"),
            log: name("log"),
        })
    }

    /// The last line of the model's log, without folders.
    fn log_tail(&self) -> String {
        let mut text = String::new();
        let _ = fs::File::open(&self.log).and_then(|mut file| file.read_to_string(&mut text));
        let line = text
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("no message");
        let line: String = line
            .split_whitespace()
            .map(|word| {
                if word.contains('/') || word.contains('\\') {
                    "<path>"
                } else {
                    word
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        line.chars().take(MAX_LOG_CHARS).collect()
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        for path in [&self.canvas, &self.output, &self.log] {
            let _ = fs::remove_file(path);
        }
    }
}

/// Canvas frames of `seconds`.
fn frames(seconds: f32) -> usize {
    (f64::from(seconds) * f64::from(FUSION_RATE)).round() as usize
}

/// Source frames of `seconds` at `rate`.
fn source_frames(seconds: f32, rate: u32) -> u64 {
    (f64::from(seconds) * f64::from(rate)).round() as u64
}

/// The samples of source frames `first..end` of `audio`, or `Err` when the
/// window starts before the song or ends after it.
fn source_window(audio: Audio<'_>, first: Option<u64>, end: u64) -> Result<&[f32], FusionError> {
    let channels = u64::from(audio.channels);
    let first = first.ok_or(FusionError::Layout("the window starts before the song"))?;
    let (start, end) = (first * channels, end * channels);
    usize::try_from(start)
        .ok()
        .zip(usize::try_from(end).ok())
        .and_then(|(start, end)| audio.samples.get(start..end))
        .ok_or(FusionError::Layout("the window runs past the song"))
}

/// `samples` converted to [`FUSION_CHANNELS`] at [`FUSION_RATE`].
fn to_canvas(samples: &[f32], channels: u16, rate: u32) -> Result<Vec<f32>, FusionError> {
    if channels == 0 || rate == 0 {
        return Err(FusionError::Audio(
            "the audio has no channels or no rate".into(),
        ));
    }
    let source = SamplesBuffer::new(channels, rate, samples.to_vec());
    Ok(UniformSourceIterator::new(source, FUSION_CHANNELS, FUSION_RATE).collect())
}

fn write_wav(path: &Path, samples: &[f32]) -> Result<(), FusionError> {
    let spec = hound::WavSpec {
        channels: FUSION_CHANNELS,
        sample_rate: FUSION_RATE,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let audio = |error: hound::Error| FusionError::Audio(error.to_string());
    let mut writer = hound::WavWriter::create(path, spec).map_err(audio)?;
    for sample in samples {
        writer.write_sample(*sample).map_err(audio)?;
    }
    writer.finalize().map_err(audio)
}

/// The model's output: [`FUSION_CHANNELS`] at [`FUSION_RATE`], 16-bit or
/// float, as interleaved `f32`.
fn read_wav(path: &Path) -> Result<Vec<f32>, FusionError> {
    let model = |error: hound::Error| FusionError::Model(error.to_string());
    let mut reader = hound::WavReader::open(path).map_err(model)?;
    let spec = reader.spec();
    if spec.channels != FUSION_CHANNELS || spec.sample_rate != FUSION_RATE {
        return Err(FusionError::Model("the output has another format".into()));
    }
    match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Float, 32) => reader
            .samples::<f32>()
            .collect::<Result<_, _>>()
            .map_err(model),
        (hound::SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .map(|sample| sample.map(|value| f32::from(value) / 32_768.0))
            .collect::<Result<_, _>>()
            .map_err(model),
        _ => Err(FusionError::Model(
            "the output has another sample format".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FUSION: Fusion = Fusion {
        before: 2.0,
        length: 1.0,
        after: 2.0,
        seam: 0.25,
        strength: 0.5,
    };

    fn tone(seconds: f32, rate: u32, level: f32) -> Vec<f32> {
        (0..(seconds * rate as f32) as usize)
            .flat_map(|n| {
                let value = (std::f32::consts::TAU * 220.0 * n as f32 / rate as f32).sin() * level;
                [value, value]
            })
            .collect()
    }

    fn peak(samples: &[f32]) -> f32 {
        samples
            .iter()
            .fold(0.0_f32, |peak, sample| peak.max(sample.abs()))
    }

    #[test]
    fn the_canvas_blends_both_songs_with_equal_power_over_the_fusion() {
        let outgoing = tone(10.0, 48_000, 0.5);
        let incoming = tone(10.0, 44_100, 0.25);
        let canvas = Canvas::new(
            Audio {
                samples: &outgoing,
                channels: 2,
                rate: 48_000,
            },
            6 * 48_000,
            Audio {
                samples: &incoming,
                channels: 2,
                rate: 44_100,
            },
            2 * 44_100,
            1.0,
            FUSION,
        )
        .unwrap();
        let second = 44_100 * 2;
        assert_eq!(canvas.samples().len(), 5 * second);
        assert!((peak(&canvas.samples()[..2 * second]) - 0.5).abs() < 0.02);
        assert!((peak(&canvas.samples()[3 * second..]) - 0.25).abs() < 0.02);
        let middle = &canvas.samples()[2 * second + second / 2 - 2_000..2 * second + second / 2];
        assert!(
            peak(middle) > 0.3,
            "both songs sound in the middle: {}",
            peak(middle)
        );
    }

    #[test]
    fn a_fusion_needs_room_on_both_sides_and_bounded_values() {
        let audio = tone(4.0, 44_100, 0.5);
        let song = Audio {
            samples: &audio,
            channels: 2,
            rate: 44_100,
        };
        assert!(matches!(
            Canvas::new(song, 44_100, song, 2 * 44_100, 1.0, FUSION),
            Err(FusionError::Layout(_))
        ));
        assert!(matches!(
            Canvas::new(song, 2 * 44_100, song, 3 * 44_100, 1.0, FUSION),
            Err(FusionError::Layout(_))
        ));
        assert!(matches!(
            Canvas::new(song, 2 * 44_100, song, 44_100, 3.0, FUSION),
            Err(FusionError::Layout(_))
        ));
        let thin = Fusion {
            before: 0.1,
            ..FUSION
        };
        assert!(Canvas::new(song, 2 * 44_100, song, 44_100, 1.0, thin).is_err());
    }

    #[test]
    fn the_clip_is_the_fusion_with_its_seams_at_the_songs_level() {
        let audio = tone(10.0, 44_100, 0.5);
        let song = Audio {
            samples: &audio,
            channels: 2,
            rate: 44_100,
        };
        let canvas = Canvas::new(song, 4 * 44_100, song, 3 * 44_100, 1.0, FUSION).unwrap();
        let quiet: Vec<f32> = canvas.samples().iter().map(|sample| sample * 0.5).collect();
        let clip = canvas.clip(&quiet).unwrap();
        assert_eq!(clip.len() as u64, FUSION.clip_frames(FUSION_RATE) * 2);
        let restored = peak(&clip[..2 * 11_025]);
        assert!(
            (restored - 0.5).abs() < 0.02,
            "the level is restored: {restored}"
        );
        assert!(canvas.clip(&quiet[..100]).is_err());
        let mut broken = quiet.clone();
        broken[10] = f32::NAN;
        assert!(canvas.clip(&broken).is_err());
    }

    #[test]
    fn the_prompt_names_tempo_and_key() {
        let key = KeyEstimate {
            tonic: yt_core::PitchClass::new(9).unwrap(),
            mode: KeyMode::Minor,
            confidence: 0.8,
        };
        assert_eq!(
            prompt(127.6, Some(key)),
            "seamless DJ mashup of two songs, 128 BPM, A minor"
        );
        assert_eq!(
            prompt(90.0, None),
            "seamless DJ mashup of two songs, 90 BPM"
        );
    }
}
