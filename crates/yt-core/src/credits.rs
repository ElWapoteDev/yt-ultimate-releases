//! Song credits: who wrote, produced and released a recording. Library
//! tracks read them from their tags; online media from the provider.

use crate::{AppCore, CoreError, Result};
use lofty::{
    file::TaggedFileExt,
    prelude::{Accessor, ItemKey},
    probe::Probe,
};
use serde::{Deserialize, Serialize};

/// Longest accepted credit text (role, name, label), in characters.
pub const MAX_CREDIT_CHARS: usize = 300;
/// Most roles kept per recording.
pub const MAX_CREDIT_ROLES: usize = 32;
/// Most names kept per role.
pub const MAX_CREDIT_NAMES: usize = 24;

/// One role and the people credited with it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreditRole {
    /// Role as given by the source, e.g. «Composer» or «Producer».
    pub role: String,
    pub names: Vec<String>,
}

/// Credits of one recording.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Credits {
    /// Distributor that provided the recording, when known.
    pub provider: Option<String>,
    /// Label or copyright line.
    pub label: Option<String>,
    /// Release date as given (usually YYYY-MM-DD or YYYY).
    pub released: Option<String>,
    pub roles: Vec<CreditRole>,
}

fn bounded(text: &str) -> bool {
    !text.trim().is_empty()
        && text.chars().count() <= MAX_CREDIT_CHARS
        && !text.chars().any(char::is_control)
}

impl Credits {
    pub fn is_empty(&self) -> bool {
        self.provider.is_none()
            && self.label.is_none()
            && self.released.is_none()
            && self.roles.is_empty()
    }

    /// Check lengths, control characters and counts.
    pub fn validate(&self) -> Result<()> {
        let optional_ok = [&self.provider, &self.label, &self.released]
            .into_iter()
            .flatten()
            .all(|text| bounded(text));
        let roles_ok = self.roles.len() <= MAX_CREDIT_ROLES
            && self.roles.iter().all(|role| {
                bounded(&role.role)
                    && !role.names.is_empty()
                    && role.names.len() <= MAX_CREDIT_NAMES
                    && role.names.iter().all(|name| bounded(name))
            });
        if optional_ok && roles_ok {
            Ok(())
        } else {
            Err(CoreError::Invalid("Invalid credits".into()))
        }
    }
}

/// Tag keys read as credit roles, with the role name shown for each.
const TAG_ROLES: &[(ItemKey, &str)] = &[
    (ItemKey::Composer, "Composer"),
    (ItemKey::Lyricist, "Lyricist"),
    (ItemKey::Writer, "Writer"),
    (ItemKey::Producer, "Producer"),
    (ItemKey::Arranger, "Arranger"),
    (ItemKey::Conductor, "Conductor"),
    (ItemKey::Engineer, "Engineer"),
    (ItemKey::MixEngineer, "Mixing Engineer"),
    (ItemKey::Remixer, "Remixer"),
];

/// Split a tag value that lists several people.
fn names(value: &str) -> Vec<String> {
    value
        .split([';', '/', '\0'])
        .flat_map(|part| part.split(" & "))
        .map(str::trim)
        .filter(|name| bounded(name))
        .map(str::to_owned)
        .take(MAX_CREDIT_NAMES)
        .collect()
}

impl AppCore {
    /// Credits of a library track read from its file tags. Missing tags read
    /// as empty credits; values that do not validate are dropped.
    pub fn track_credits(&self, id: &str) -> Result<Credits> {
        let track = self.track(id)?;
        if track.missing {
            return Err(CoreError::NotFound(id.into()));
        }
        let Ok(tagged) = Probe::open(&track.path).and_then(|probe| probe.read()) else {
            return Ok(Credits::default());
        };
        let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
            return Ok(Credits::default());
        };
        let mut credits = Credits {
            provider: None,
            label: tag
                .get_string(&ItemKey::Label)
                .or_else(|| tag.get_string(&ItemKey::Publisher))
                .or_else(|| tag.get_string(&ItemKey::CopyrightMessage))
                .filter(|text| bounded(text))
                .map(str::to_owned),
            released: tag
                .get_string(&ItemKey::ReleaseDate)
                .or_else(|| tag.get_string(&ItemKey::RecordingDate))
                .map(str::to_owned)
                .or_else(|| tag.year().map(|year| year.to_string()))
                .filter(|text| bounded(text)),
            roles: Vec::new(),
        };
        for (key, role) in TAG_ROLES {
            let people: Vec<String> = tag.get_strings(key).flat_map(names).collect();
            if !people.is_empty() && credits.roles.len() < MAX_CREDIT_ROLES {
                credits.roles.push(CreditRole {
                    role: (*role).to_owned(),
                    names: people,
                });
            }
        }
        Ok(credits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_split_common_separators_and_drop_empty_parts() {
        assert_eq!(names("A; B / C & D;;"), ["A", "B", "C", "D"]);
        assert!(names("  ").is_empty());
    }

    #[test]
    fn credits_validate_lengths_and_counts() {
        let valid = Credits {
            provider: Some("Distributor".into()),
            label: Some("℗ 2024 Label".into()),
            released: Some("2024-05-05".into()),
            roles: vec![CreditRole {
                role: "Composer".into(),
                names: vec!["Someone".into()],
            }],
        };
        assert!(valid.validate().is_ok());
        assert!(!valid.is_empty());
        let mut empty_names = valid.clone();
        empty_names.roles[0].names.clear();
        assert!(empty_names.validate().is_err());
        let mut control = valid.clone();
        control.label = Some("bad\u{7}".into());
        assert!(control.validate().is_err());
        assert!(Credits::default().is_empty());
    }

    #[test]
    fn untagged_library_files_have_empty_credits() {
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        let track = core.import_bytes("song.wav", &[1, 2, 3, 4]).unwrap();
        assert!(core.track_credits(&track.id).unwrap().is_empty());
        assert!(core.track_credits("missing").is_err());
    }
}
