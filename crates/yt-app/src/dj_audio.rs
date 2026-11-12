//! Audio a planned mix needs besides the songs, made in `ytu-worker`, read
//! back into memory and added to the mix, its files removed at once: the
//! vocal stems of a mix that plays the songs' vocals apart (a mashup or a
//! vocal ride; between clashing keys the outgoing vocals are also moved to
//! the incoming key here), and the model's rendering of an AI fusion. The
//! planner asks for them only when both sources allow separating or
//! generating. See `docs/systems/Dj.md`.

use crate::state::AppState;
use std::{
    hash::{BuildHasher, RandomState},
    path::{Path, PathBuf},
    sync::Arc,
};
use yt_core::{KeyEstimate, MediaOrigin};
use yt_media::{CancellationToken, Mix, Stem};
use yt_worker::{
    fusion::{FUSION_CHANNELS, FUSION_RATE},
    protocol::{FusionRequest, FusionSide},
    WorkerClient,
};

/// Folders of the profile's `dj` folder where the worker writes stems and
/// fusions.
const STEMS_FOLDER: &str = "stems";
const FUSIONS_FOLDER: &str = "fusions";
/// Bytes per stored sample (little-endian `f32`).
const SAMPLE_BYTES: usize = 4;

impl AppState {
    /// Separate the vocal stems `mix` needs from the songs' audio files
    /// (outgoing, incoming, each with its origin) and add them to the planned
    /// mix of `generation`; mark that plan as failed when they cannot be had.
    /// `cancel` stops it.
    pub(crate) fn separate_stems(
        &self,
        generation: u64,
        mix: Mix,
        sources: [(MediaOrigin, PathBuf); 2],
        cancel: CancellationToken,
    ) {
        let Some(client) = self
            .dj_analysis
            .client(&self.services.profile)
            .filter(|client| client.measures_vocals())
        else {
            self.extra_ready(generation, None);
            return;
        };
        let folder = self.services.profile.join("dj").join(STEMS_FOLDER);
        let state = self.clone();
        dioxus::core::spawn_forever(async move {
            let with_stems = separate_both(&client, &folder, mix, sources, &cancel).await;
            if !cancel.is_cancelled() {
                state.extra_ready(generation, with_stems);
            }
        });
    }

    /// Render the AI fusion `mix` plans from the songs' audio files
    /// (outgoing, incoming, each with its origin), prompted with the outgoing
    /// `tempo_bpm` and the incoming `key`, and add it to the planned mix of
    /// `generation`; mark that plan as failed when it cannot be had. `cancel`
    /// stops it.
    pub(crate) fn render_fusion(
        &self,
        generation: u64,
        mix: Mix,
        sources: [(MediaOrigin, PathBuf); 2],
        prompt: (f32, Option<KeyEstimate>),
        cancel: CancellationToken,
    ) {
        let Some(client) = self
            .dj_analysis
            .client(&self.services.profile)
            .filter(|client| client.renders_fusions())
        else {
            self.extra_ready(generation, None);
            return;
        };
        let folder = self.services.profile.join("dj").join(FUSIONS_FOLDER);
        let state = self.clone();
        dioxus::core::spawn_forever(async move {
            let fused = fuse(&client, &folder, mix, sources, prompt, &cancel).await;
            if !cancel.is_cancelled() {
                state.extra_ready(generation, fused);
            }
        });
    }
}

/// `mix` with the model's rendering of its fusion, or `None` when it cannot
/// be had.
async fn fuse(
    client: &Arc<WorkerClient>,
    folder: &Path,
    mix: Mix,
    [(out_origin, out_path), (in_origin, in_path)]: [(MediaOrigin, PathBuf); 2],
    (tempo_bpm, key): (f32, Option<KeyEstimate>),
    cancel: &CancellationToken,
) -> Option<Mix> {
    let plan = mix.plan();
    let fusion = plan.fusion?;
    fresh_folder(folder).await?;
    let output = folder.join(format!(
        "{:016x}.f32",
        RandomState::new().hash_one(plan.exit_frame)
    ));
    let request = FusionRequest {
        outgoing: FusionSide {
            origin: out_origin,
            path: out_path,
            frame: plan.exit_frame,
        },
        incoming: FusionSide {
            origin: in_origin,
            path: in_path,
            frame: plan.entry_frame,
        },
        speed: plan.speed,
        fusion,
        tempo_bpm,
        key,
        // Every `RandomState` has fresh random keys, so each fusion is new.
        seed: RandomState::new().hash_one(plan.entry_frame) as u32,
        output: output.clone(),
    };
    let Ok(frames) = client.render_fusion(request, cancel).await else {
        let _ = take_samples(output, 0).await;
        return None;
    };
    let frames = usize::try_from(frames).ok()?;
    let samples = take_samples(output, frames * usize::from(FUSION_CHANNELS)).await?;
    mix.with_rendering(samples, FUSION_CHANNELS, FUSION_RATE)
        .ok()
}

/// `folder` emptied of files an earlier run left half-way, and created.
async fn fresh_folder(folder: &Path) -> Option<()> {
    let folder = folder.to_owned();
    tokio::task::spawn_blocking(move || {
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder)
    })
    .await
    .ok()?
    .ok()
}

/// The little-endian `f32` samples of the worker's file at `path`, removed
/// at once, or `None` unless it holds exactly `count` of them.
async fn take_samples(path: PathBuf, count: usize) -> Option<Vec<f32>> {
    let bytes = tokio::task::spawn_blocking(move || {
        let bytes = std::fs::read(&path);
        let _ = std::fs::remove_file(&path);
        bytes
    })
    .await
    .ok()?
    .ok()?;
    if bytes.len() != count * SAMPLE_BYTES {
        return None;
    }
    Some(
        bytes
            .as_chunks::<SAMPLE_BYTES>()
            .0
            .iter()
            .map(|word| f32::from_le_bytes(*word))
            .collect(),
    )
}

/// Both stems of `mix` added to it, or `None` when either cannot be had.
async fn separate_both(
    client: &Arc<WorkerClient>,
    folder: &Path,
    mix: Mix,
    sources: [(MediaOrigin, PathBuf); 2],
    cancel: &CancellationToken,
) -> Option<Mix> {
    let windows = mix.plan().stem_frames()?;
    fresh_folder(folder).await?;
    let mut stems = Vec::with_capacity(2);
    for ((origin, path), window) in sources.into_iter().zip(windows) {
        let name = RandomState::new().hash_one(window.start);
        let output = folder.join(format!("{name:016x}.f32"));
        let separated = client
            .separate_vocals(origin, path, window.clone(), output.clone(), cancel)
            .await;
        let Ok(separated) = separated else {
            let _ = take_samples(output, 0).await;
            return None;
        };
        let count = usize::try_from(separated.frames).ok()? * usize::from(separated.channels);
        let samples = take_samples(output, count).await?;
        stems.push(
            Stem::new(
                window.start,
                separated.channels,
                separated.sample_rate,
                samples,
            )
            .ok()?,
        );
    }
    let [outgoing, incoming] = <[Stem; 2]>::try_from(stems).ok()?;
    // Between clashing keys the outgoing vocals ride in the incoming key:
    // they are moved here, ahead of the mix and off the interface thread.
    let plan = mix.plan();
    let outgoing = match plan.vocal_shift.zip(plan.vocal_shift_frame()) {
        Some((shift, from)) => {
            tokio::task::spawn_blocking(move || outgoing.moved(shift.semitones, from))
                .await
                .ok()?
                .ok()?
        }
        None => outgoing,
    };
    if cancel.is_cancelled() {
        return None;
    }
    mix.with_stems(outgoing, incoming).ok()
}
