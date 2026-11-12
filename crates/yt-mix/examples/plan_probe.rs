//! Plans mixes between real analyses, by hand: `cargo run --release -p
//! yt-mix --example plan_probe -- [--mashup [--no-fusion] [--always-fuse]]
//! [--assume-vocals] [--all-pairs] [--quiet] a.json b.json …`.
//! Each file holds one `TrackAnalysis` as JSON (the `analysis` column of
//! `rust_dj_analysis`). Every consecutive pair, or with `--all-pairs` every
//! ordered pair, is planned with several seeds, as the application would plan
//! it with a new seed per transition. `--mashup` plans with the mashup style,
//! stems and fusions available (`--no-fusion` for an installation without the
//! fusion model, `--always-fuse` to fuse every pair it can), between YouTube
//! songs; otherwise with the professional style between library songs.
//! `--assume-vocals` gives songs whose vocals were not measured a voice that
//! sings from the first tenth of the song to the last, as most songs do, to
//! estimate what the vocal model would allow. `--quiet` prints only the
//! closing count of the techniques, with mashups that move their vocals
//! counted apart.

use std::{collections::BTreeMap, time::Instant};
use yt_core::{DjStyle, MixLength, SourceRights, TrackAnalysis};
use yt_mix::{plan, MixRequest};

const SEEDS: u64 = 8;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mashup = args.iter().any(|arg| arg == "--mashup");
    let all_pairs = args.iter().any(|arg| arg == "--all-pairs");
    let always = args.iter().any(|arg| arg == "--always-fuse");
    let fusing = mashup && !args.iter().any(|arg| arg == "--no-fusion");
    let assume_vocals = args.iter().any(|arg| arg == "--assume-vocals");
    let quiet = args.iter().any(|arg| arg == "--quiet");
    let analyses: Vec<(String, TrackAnalysis)> = args
        .iter()
        .filter(|arg| !arg.starts_with("--"))
        .filter_map(|path| {
            let text = std::fs::read_to_string(path).ok()?;
            let mut analysis: TrackAnalysis = serde_json::from_str(&text).ok()?;
            analysis.validate().ok()?;
            if assume_vocals && analysis.vocal_db.is_empty() {
                analysis.vocal_db = assumed_vocals(&analysis);
            }
            Some((path.rsplit('/').next().unwrap_or(path).to_owned(), analysis))
        })
        .collect();
    let pairs: Vec<(usize, usize)> = if all_pairs {
        (0..analyses.len())
            .flat_map(|from| (0..analyses.len()).map(move |to| (from, to)))
            .filter(|(from, to)| from != to)
            .collect()
    } else {
        (1..analyses.len()).map(|to| (to - 1, to)).collect()
    };
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for (from_index, to_index) in pairs {
        let (from, outgoing) = &analyses[from_index];
        let (to, incoming) = &analyses[to_index];
        if !quiet {
            println!("{from} → {to}");
        }
        let mut previous = None;
        for seed in 0..SEEDS {
            let request = MixRequest {
                outgoing,
                incoming,
                length: MixLength::Medium,
                intensity: 2,
                rights: if mashup {
                    SourceRights::YOUTUBE
                } else {
                    SourceRights::LIBRARY
                },
                earliest_exit: 0,
                previous,
                seed,
                separation: mashup,
                style: if mashup {
                    DjStyle::Mashup
                } else {
                    DjStyle::Professional
                },
                fusing,
                always_fuse: always,
            };
            let started = Instant::now();
            let found = plan(&request);
            let micros = started.elapsed().as_micros();
            match found {
                Some(mix) => {
                    previous = Some(mix.technique);
                    let name = match mix.vocal_shift {
                        Some(_) => format!("{:?} (moved vocals)", mix.technique),
                        None => format!("{:?}", mix.technique),
                    };
                    *counts.entry(name).or_default() += 1;
                    if quiet {
                        continue;
                    }
                    let rate = f64::from(mix.outgoing_rate);
                    let exit = mix.exit_frame as f64 / rate;
                    let bar = outgoing
                        .downbeats
                        .iter()
                        .position(|index| outgoing.beats[*index as usize] == mix.exit_frame);
                    println!(
                        "  seed {seed}: {:?} at {exit:.1} s (bar {bar:?}) into {:.1} s, overlap {:.1} s, speed {:.3}, vocals moved {:?}, effects {:?}, report {:?} [{micros} µs]",
                        mix.technique,
                        mix.entry_frame as f64 / f64::from(mix.incoming_rate),
                        mix.overlap,
                        mix.speed,
                        mix.vocal_shift.map(|shift| shift.semitones),
                        mix.outgoing.effects,
                        mix.report,
                    );
                }
                None => {
                    *counts.entry("crossfade".into()).or_default() += 1;
                    if !quiet {
                        println!("  seed {seed}: no mix [{micros} µs]");
                    }
                }
            }
        }
    }
    let total: usize = counts.values().sum();
    println!("techniques over {total} plans:");
    for (name, count) in &counts {
        let share = 100.0 * *count as f64 / total.max(1) as f64;
        println!("  {name}: {count} ({share:.1} %)");
    }
}

/// A voice at a level like measured ones (−15 dBFS) from the first tenth of
/// `analysis` to its last, silent around it.
fn assumed_vocals(analysis: &TrackAnalysis) -> Vec<f32> {
    let seconds = analysis.duration_frames as f64 / f64::from(analysis.sample_rate);
    let steps = (seconds / f64::from(TrackAnalysis::LOUDNESS_STEP_SECONDS)) as usize;
    (0..steps)
        .map(|step| {
            if (steps / 10..steps - steps / 10).contains(&step) {
                -15.0
            } else {
                -90.0
            }
        })
        .collect()
}
