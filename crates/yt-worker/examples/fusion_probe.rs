//! Render one AI fusion of two audio files and write previews of the mix
//! around it, for listening and timing.
//!
//! cargo run --release -p yt-worker --example fusion_probe -- \
//!     <outgoing> <incoming> <ONNX Runtime library> <beat model> \
//!     <sa3-generate> <Stable Audio 3 folder> <output folder> \
//!     [--bars N] [--seed S] [--strength X] [--speed X]
//!
//! Analyzes both files with the beat model, starts the fusion on the outgoing
//! downbeat nearest 70 % of its music and lands on the incoming song's bar
//! `N + 4`, fusing over `N` bars (8 by default) at the outgoing tempo. Writes
//! `blend.wav` (the songs blended with no model) and `fusion.wav` (the songs
//! crossfaded into the model's rendering and out of it), each with 12 s of
//! the songs on either side. Reads only the given files.

use rodio::Source;
use std::{path::PathBuf, sync::atomic::AtomicBool, time::Instant};
use yt_core::TrackAnalysis;
use yt_mir::{Analyzer, BeatModel, Device, Pcm};
use yt_mix::{Fusion, FUSION_STRENGTH, MAX_FUSION_CONTEXT_SECONDS};
use yt_worker::fusion::{self, Audio, Canvas, Generator, FUSION_CHANNELS, FUSION_RATE};

const CONTEXT_BARS: f32 = 4.0;
const SIDE_SECONDS: f32 = 12.0;
const STEPS: u32 = 8;

struct Song {
    samples: Vec<f32>,
    channels: u16,
    rate: u32,
    analysis: TrackAnalysis,
}

impl Song {
    fn audio(&self) -> Audio<'_> {
        Audio {
            samples: &self.samples,
            channels: self.channels,
            rate: self.rate,
        }
    }

    fn downbeat(&self, bar: usize) -> Option<u64> {
        let beat = *self.analysis.downbeats.get(bar)? as usize;
        self.analysis.beats.get(beat).copied()
    }

    fn beat_seconds(&self) -> f32 {
        60.0 / self.analysis.tempo_bpm.unwrap_or(120.0)
    }

    fn bar_seconds(&self) -> f32 {
        let beats = self.analysis.meter.map_or(4, |meter| meter.beats_per_bar);
        f32::from(beats) * self.beat_seconds()
    }

    /// Frames `first..end` at `speed`, in stereo at the fusion rate.
    fn rendered(
        &self,
        first: u64,
        end: u64,
        speed: f32,
    ) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
        let channels = u64::from(self.channels);
        let end = end.min(self.samples.len() as u64 / channels);
        let window = self.samples[(first * channels) as usize..(end * channels) as usize].to_vec();
        let stretched = yt_media::stretch(window, self.channels, self.rate, speed)?;
        let source = rodio::buffer::SamplesBuffer::new(self.channels, self.rate, stretched);
        Ok(
            rodio::source::UniformSourceIterator::new(source, FUSION_CHANNELS, FUSION_RATE)
                .collect(),
        )
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let usage = "usage: fusion_probe <outgoing> <incoming> <libonnxruntime> <beat model> <sa3-generate> <models> <out> [--bars N] [--seed S] [--strength X] [--speed X]";
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 7 {
        return Err(usage.into());
    }
    let path = |index: usize| PathBuf::from(&args[index]);
    let (mut bars, mut seed, mut strength, mut forced_speed) =
        (8.0_f32, 7_u32, FUSION_STRENGTH, None);
    for pair in args[7..].chunks(2) {
        match (pair[0].as_str(), pair.get(1)) {
            ("--bars", Some(value)) => bars = value.parse()?,
            ("--seed", Some(value)) => seed = value.parse()?,
            ("--strength", Some(value)) => strength = value.parse()?,
            ("--speed", Some(value)) => forced_speed = Some(value.parse::<f32>()?),
            _ => return Err(usage.into()),
        }
    }
    let out = path(6);
    std::fs::create_dir_all(&out)?;
    yt_mir::init_runtime(&path(2))?;
    let mut analyzer = Analyzer::new(BeatModel::load(&path(3), Device::Gpu, None)?);
    let mut load = |path: PathBuf| -> Result<Song, Box<dyn std::error::Error>> {
        let source = yt_media::decode_file(&path)?;
        let (channels, rate) = (source.channels(), source.sample_rate());
        let samples: Vec<f32> = source.collect();
        let analysis = analyzer.analyze(
            &Pcm::new(&samples, channels, rate)?,
            &AtomicBool::new(false),
        )?;
        Ok(Song {
            samples,
            channels,
            rate,
            analysis,
        })
    };
    let (a, b) = (load(path(0))?, load(path(1))?);
    let (tempo_a, tempo_b) = (
        a.analysis
            .tempo_bpm
            .ok_or("the outgoing song has no tempo")?,
        b.analysis
            .tempo_bpm
            .ok_or("the incoming song has no tempo")?,
    );
    let speed = [1.0_f32, 2.0, 0.5]
        .into_iter()
        .map(|ratio| tempo_a / (tempo_b * ratio))
        .min_by(|x, y| (x - 1.0).abs().total_cmp(&(y - 1.0).abs()))
        .unwrap_or(1.0);
    // Songs whose tempos do not meet fuse at their own speeds.
    let speed = forced_speed.unwrap_or(speed);
    println!("tempos {tempo_a:.1} and {tempo_b:.1} BPM, incoming speed {speed:.3}");
    let target = a.analysis.audible_end() as f64 * 0.7;
    let exit_bar = (4..a.analysis.downbeats.len())
        .min_by_key(|bar| (a.downbeat(*bar).unwrap_or(0) as f64 - target).abs() as u64)
        .ok_or("no exit bar")?;
    let exit = a.downbeat(exit_bar).ok_or("no exit downbeat")?;
    let entry_bar = bars as usize + 4;
    let entry = b.downbeat(entry_bar).ok_or("no entry downbeat")?;
    let context = (CONTEXT_BARS * a.bar_seconds()).min(MAX_FUSION_CONTEXT_SECONDS);
    let layout = Fusion {
        before: context,
        length: bars * a.bar_seconds(),
        after: context,
        seam: a.beat_seconds(),
        strength,
    };
    println!(
        "fusion from outgoing bar {exit_bar} ({:.2} s) into incoming bar {entry_bar} ({:.2} s): {layout:?}",
        exit as f64 / f64::from(a.rate),
        entry as f64 / f64::from(b.rate)
    );
    let canvas = Canvas::new(a.audio(), exit, b.audio(), entry, speed, layout)?;
    let generator =
        Generator::find(path(4), path(5)).ok_or("sa3-generate or its weights are missing")?;
    let began = Instant::now();
    let prompt = fusion::prompt(tempo_a, b.analysis.key);
    let clip = generator.render(&canvas, &prompt, seed, STEPS, &out, || true)?;
    println!(
        "rendered in {:.1} s ({prompt})",
        began.elapsed().as_secs_f32()
    );

    // The previews on one timeline: the outgoing song until the fusion, the
    // incoming song (at the mix's speed) from its start under the fusion.
    let channels = usize::from(FUSION_CHANNELS);
    let frames = |seconds: f32| (seconds * FUSION_RATE as f32).round() as usize;
    let side = |seconds: f32, rate: u32| (seconds * rate as f32) as u64;
    let head = a.rendered(
        exit - side(SIDE_SECONDS, a.rate),
        exit + side(layout.length, a.rate),
        1.0,
    )?;
    let under = side(layout.length * speed, b.rate);
    let tail = b.rendered(
        entry - under,
        entry + side(SIDE_SECONDS * speed, b.rate),
        speed,
    )?;
    let (start, length, seam) = (
        frames(SIDE_SECONDS),
        frames(layout.length),
        frames(layout.seam),
    );
    let total = start + length + frames(SIDE_SECONDS);
    let equal_power = |progress: f32| {
        let angle = progress.clamp(0.0, 1.0) * std::f32::consts::FRAC_PI_2;
        (angle.cos(), angle.sin())
    };
    let at = |samples: &[f32], frame: Option<usize>, channel: usize| {
        frame
            .and_then(|frame| samples.get(frame * channels + channel))
            .copied()
            .unwrap_or(0.0)
    };
    let mut blend = vec![0.0_f32; total * channels];
    let mut fused = vec![0.0_f32; total * channels];
    for frame in 0..total {
        let (fall, rise) = equal_power((frame as f32 - start as f32) / length as f32);
        // The rendering takes over over the seam before the fusion and gives
        // way over the seam after it.
        let model = if frame + seam < start || frame >= start + length + seam {
            0.0
        } else if frame < start {
            equal_power((frame + seam - start) as f32 / seam as f32).1
        } else if frame < start + length {
            1.0
        } else {
            equal_power((frame - start - length) as f32 / seam as f32).0
        };
        let (songs, rendering) = equal_power(model_progress(model));
        for channel in 0..channels {
            let index = frame * channels + channel;
            let mixed = at(&head, Some(frame), channel) * fall
                + at(&tail, frame.checked_sub(start), channel) * rise;
            blend[index] = mixed;
            let clip_frame = (frame + seam).checked_sub(start);
            fused[index] = mixed * songs + at(&clip, clip_frame, channel) * rendering;
        }
    }
    write(&out.join("blend.wav"), &blend)?;
    write(&out.join("fusion.wav"), &fused)?;
    println!(
        "previews: the fusion runs from {:.2} s to {:.2} s",
        SIDE_SECONDS,
        SIDE_SECONDS + layout.length
    );
    Ok(())
}

/// The crossfade position whose equal-power gain of the rendering is `gain`.
fn model_progress(gain: f32) -> f32 {
    gain.clamp(0.0, 1.0).asin() / std::f32::consts::FRAC_PI_2
}

fn write(path: &std::path::Path, samples: &[f32]) -> Result<(), hound::Error> {
    let spec = hound::WavSpec {
        channels: FUSION_CHANNELS,
        sample_rate: FUSION_RATE,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for sample in samples {
        writer.write_sample(*sample)?;
    }
    writer.finalize()
}
