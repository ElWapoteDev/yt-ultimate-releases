//! Region controller: the zone that decides which concerts are near the user.
//! While «Detectar mi zona» is on, the device's position is asked once per
//! session, when concerts are shown, and whenever the user asks; it is turned
//! into the zone's city and country on the device (`yt_geo::zone_at`), saved
//! as the region and dropped. See `docs/systems/Region.md`.

use crate::{
    native::LocationError,
    state::{use_app_signal, AppState},
};
use dioxus::prelude::*;
use std::{cell::Cell, rc::Rc};
use yt_core::RegionPreferences;

/// How the latest detection of this session went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Detection {
    /// Not asked this session.
    Idle,
    /// Waiting for the permission or for the position.
    Detecting,
    /// The zone around the device was saved.
    Detected,
    /// The user or a policy refused location access.
    Denied,
    /// Location is off, no position came in time, or no town is near.
    Unavailable,
}

impl Detection {
    /// What the interface says about this state; `None` when it shows the
    /// zone itself.
    pub(crate) fn message(self) -> Option<&'static str> {
        match self {
            Self::Idle | Self::Detected => None,
            Self::Detecting => Some("Buscando tu zona…"),
            Self::Denied => Some("YT Ultimate no tiene permiso para usar tu ubicación."),
            Self::Unavailable => Some("No se pudo saber en qué zona estás."),
        }
    }
}

/// Region detection shared with the views; cloned with `AppState`.
#[derive(Clone)]
pub(crate) struct Region {
    pub detection: Signal<Detection>,
    /// Bumped by every detection and every change of the setting, so an
    /// older answer is dropped.
    generation: Rc<Cell<u64>>,
}

pub(crate) fn use_region() -> Region {
    Region {
        detection: use_app_signal(|| Detection::Idle),
        generation: use_hook(|| Rc::new(Cell::new(0))),
    }
}

impl Region {
    fn next_generation(&self) -> u64 {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        generation
    }
}

impl AppState {
    /// Detect the zone the first time this session something needs it, when
    /// detection is on.
    pub(crate) fn detect_region_once(&self) {
        if self.settings.peek().region.detect && *self.region.detection.peek() == Detection::Idle {
            self.detect_region();
        }
    }

    /// Ask the device where it is and save the zone around it. The operating
    /// system asks the user for permission the first time.
    pub(crate) fn detect_region(&self) {
        let generation = self.region.next_generation();
        let mut detection = self.region.detection;
        detection.set(Detection::Detecting);
        let state = self.clone();
        dioxus::core::spawn_forever(async move {
            let zone = match crate::native::device_position().await {
                Ok(position) => tokio::task::spawn_blocking(move || yt_geo::zone_at(position))
                    .await
                    .ok()
                    .flatten()
                    .ok_or(LocationError::Unavailable),
                Err(error) => Err(error),
            };
            if state.region.generation.get() == generation {
                state.apply_zone(zone);
            }
        });
    }

    /// Save a detected zone, or record why there is none, unless detection
    /// was turned off meanwhile.
    fn apply_zone(&self, zone: Result<yt_geo::Zone, LocationError>) {
        if !self.settings.peek().region.detect {
            return;
        }
        let mut detection = self.region.detection;
        match zone {
            Ok(zone) => {
                let region = RegionPreferences {
                    city: zone.city.to_owned(),
                    country: zone.country.to_owned(),
                    detect: true,
                };
                if self.settings.peek().region != region {
                    self.edit_settings(|settings| settings.region = region.clone());
                }
                let saved = self.settings.peek().region == region;
                detection.set(if saved {
                    Detection::Detected
                } else {
                    Detection::Unavailable
                });
            }
            Err(LocationError::Denied) => detection.set(Detection::Denied),
            Err(LocationError::Unavailable) => detection.set(Detection::Unavailable),
        }
    }

    /// Turn detection on, which detects now, or off. The last zone stays and
    /// the user can edit it.
    pub(crate) fn set_region_detection(&self, detect: bool) {
        self.region.next_generation();
        self.edit_settings(|settings| settings.region.detect = detect);
        if detect {
            self.detect_region();
        } else {
            let mut detection = self.region.detection;
            detection.set(Detection::Idle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestApp;

    const ZONE: yt_geo::Zone = yt_geo::Zone {
        city: "Monterrey",
        country: "México",
    };

    #[test]
    fn a_detected_zone_becomes_the_saved_region() {
        let mut app = TestApp::new();
        let core = app.services.core.clone();
        app.run(|state| {
            state.apply_zone(Ok(ZONE));
            assert_eq!(*state.region.detection.peek(), Detection::Detected);
            let region = core.settings().unwrap().region;
            assert_eq!(
                (region.city.as_str(), region.country.as_str()),
                ("Monterrey", "México")
            );
            assert!(region.detect);
            state.apply_zone(Err(LocationError::Denied));
            assert_eq!(*state.region.detection.peek(), Detection::Denied);
            assert_eq!(state.settings.peek().region.city, "Monterrey");
        });
    }

    #[test]
    fn turning_detection_off_keeps_the_zone_and_ignores_late_answers() {
        let mut app = TestApp::new();
        app.run(|state| {
            state.apply_zone(Ok(ZONE));
            state.set_region_detection(false);
            assert_eq!(*state.region.detection.peek(), Detection::Idle);
            state.apply_zone(Ok(yt_geo::Zone {
                city: "Lima",
                country: "Perú",
            }));
            let region = state.settings.peek().region.clone();
            assert!(!region.detect);
            assert_eq!(region.city, "Monterrey");
            state.detect_region_once();
            assert_eq!(*state.region.detection.peek(), Detection::Idle);
        });
    }
}
