//! What the DJ may do with each source of audio. Everything is denied unless a
//! source's policy grants it: the planner never proposes what a source
//! forbids, and the worker refuses jobs that would break it. The policy and
//! its reasons are in `docs/AI_DJ.md` §7.

use crate::REMOTE_ID_PREFIX;
use serde::{Deserialize, Serialize};

/// Where a track's audio comes from, as far as its rights are concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaOrigin {
    /// A file in the user's library on this device.
    Library,
    /// Audio streamed from YouTube.
    YouTube,
}

impl MediaOrigin {
    /// The origin of the media identified by `media_id`: `youtube:<video id>`
    /// or a library track id.
    pub fn of_media_id(media_id: &str) -> Self {
        if media_id.starts_with(REMOTE_ID_PREFIX) {
            Self::YouTube
        } else {
            Self::Library
        }
    }
}

/// What may be done on this device with the audio of one origin. Exporting or
/// sharing anything derived from it is never allowed, so it has no flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRights {
    /// Measure its musical features (beats, bars, key, loudness): data about
    /// the music, not audio.
    pub analyze: bool,
    /// Play the original audio, in its original order, through the DJ's
    /// processing (tempo, EQ, filters, effects).
    pub mix: bool,
    /// Separate it into stems.
    pub separate: bool,
    /// Rearrange fragments of it into a new arrangement.
    pub rearrange: bool,
    /// Condition a generative model on it.
    pub generate: bool,
    /// Keep audio derived from it (stems, bridges, clips) in the persistent
    /// cache. Otherwise that audio lives in memory or in temporary files that
    /// are removed when the transition ends.
    pub keep_derived_audio: bool,
}

impl SourceRights {
    /// Nothing is allowed: the rights of any origin without a policy.
    pub const NONE: Self = Self {
        analyze: false,
        mix: false,
        separate: false,
        rearrange: false,
        generate: false,
        keep_derived_audio: false,
    };

    /// The user's own files, processed privately on this device.
    pub const LIBRARY: Self = Self {
        analyze: true,
        mix: true,
        separate: true,
        rearrange: true,
        generate: true,
        keep_derived_audio: true,
    };

    /// YouTube: analyzed, mixed, separated, rearranged and fused by a
    /// generative model on this device only while a transition plays, in
    /// memory or in temporary files removed as soon as they are read; never
    /// kept or shared. The owner's choices of 12 November 2026
    /// (`docs/AI_DJ.md` §7.3).
    pub const YOUTUBE: Self = Self {
        analyze: true,
        mix: true,
        separate: true,
        rearrange: true,
        generate: true,
        ..Self::NONE
    };

    /// The policy for audio of `origin`.
    pub const fn of(origin: MediaOrigin) -> Self {
        match origin {
            MediaOrigin::Library => Self::LIBRARY,
            MediaOrigin::YouTube => Self::YOUTUBE,
        }
    }

    /// What a transition between audio with these rights and audio with
    /// `other` may do: each action needs both.
    pub const fn with(self, other: Self) -> Self {
        Self {
            analyze: self.analyze && other.analyze,
            mix: self.mix && other.mix,
            separate: self.separate && other.separate,
            rearrange: self.rearrange && other.rearrange,
            generate: self.generate && other.generate,
            keep_derived_audio: self.keep_derived_audio && other.keep_derived_audio,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_follow_the_media_id() {
        assert_eq!(
            MediaOrigin::of_media_id("youtube:abcdefghijk"),
            MediaOrigin::YouTube
        );
        assert_eq!(
            MediaOrigin::of_media_id("3f2c9e5a-track"),
            MediaOrigin::Library
        );
    }

    #[test]
    fn youtube_audio_is_processed_only_while_it_plays_and_never_kept() {
        let youtube = SourceRights::of(MediaOrigin::YouTube);
        assert!(youtube.analyze && youtube.mix && youtube.separate && youtube.rearrange);
        assert!(youtube.generate && !youtube.keep_derived_audio);
        assert_eq!(
            SourceRights::of(MediaOrigin::Library),
            SourceRights::LIBRARY
        );
    }

    #[test]
    fn a_pair_allows_only_what_both_sources_allow() {
        let pair = SourceRights::LIBRARY.with(SourceRights::YOUTUBE);
        assert_eq!(pair, SourceRights::YOUTUBE);
        assert_eq!(
            SourceRights::LIBRARY.with(SourceRights::NONE),
            SourceRights::NONE
        );
    }
}
