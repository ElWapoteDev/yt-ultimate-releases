//! The device's position from the operating system's location service: Core
//! Location on macOS, `Windows.Devices.Geolocation` on Windows. The first
//! request shows the system's permission prompt (macOS, and Windows 11 24H2
//! and later); the answer is the system's to keep. Positions are returned to
//! the caller only, never stored or logged. See `docs/systems/Region.md`.

use crate::state::AppState;
use std::time::Duration;
use yt_geo::Coordinates;

/// Longest wait for the user's answer to the permission prompt and for the
/// position after it.
const LOCATION_TIMEOUT: Duration = Duration::from_secs(120);

/// Why the device could not say where it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocationError {
    /// The user or a policy refused location access to the application.
    Denied,
    /// Location services are off, no position came in time, or the platform
    /// has no location service.
    Unavailable,
}

/// The device's current position, about a kilometre accurate. Poll it on the
/// UI thread: the system's location objects deliver their answers to the
/// thread that created them.
pub(crate) async fn device_position() -> Result<Coordinates, LocationError> {
    tokio::time::timeout(LOCATION_TIMEOUT, platform::position())
        .await
        .unwrap_or(Err(LocationError::Unavailable))
}

/// Open the system's location privacy settings, where the user grants or
/// withdraws the permission.
pub(crate) fn open_location_settings(state: &AppState) {
    super::run_native_action(state, || {
        super::open_native(std::ffi::OsStr::new(platform::LOCATION_SETTINGS))
    });
}

#[cfg(target_os = "macos")]
mod platform {
    use super::LocationError;
    use objc2::{
        define_class, msg_send, rc::Retained, runtime::ProtocolObject, DefinedClass,
        MainThreadMarker, MainThreadOnly,
    };
    use objc2_core_location::{
        kCLLocationAccuracyKilometer, CLAuthorizationStatus, CLError, CLLocation,
        CLLocationManager, CLLocationManagerDelegate,
    };
    use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol};
    use std::cell::RefCell;
    use tokio::sync::oneshot;
    use yt_geo::Coordinates;

    pub(super) const LOCATION_SETTINGS: &str =
        "x-apple.systempreferences:com.apple.preference.security?Privacy_LocationServices";

    type Answer = Result<(f64, f64), LocationError>;

    struct Ivars {
        answer: RefCell<Option<oneshot::Sender<Answer>>>,
    }

    define_class!(
        // SAFETY: NSObject has no subclassing requirements, and the delegate
        // does not implement Drop.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "YTULocationDelegate"]
        #[ivars = Ivars]
        struct LocationDelegate;

        unsafe impl NSObjectProtocol for LocationDelegate {}

        unsafe impl CLLocationManagerDelegate for LocationDelegate {
            #[unsafe(method(locationManagerDidChangeAuthorization:))]
            fn did_change_authorization(&self, manager: &CLLocationManager) {
                // SAFETY: the manager calls its delegate on the thread that
                // created it, where it may be asked for its state and position.
                match unsafe { manager.authorizationStatus() } {
                    CLAuthorizationStatus::NotDetermined => unsafe {
                        manager.requestWhenInUseAuthorization()
                    },
                    CLAuthorizationStatus::AuthorizedAlways
                    | CLAuthorizationStatus::AuthorizedWhenInUse => unsafe {
                        manager.requestLocation()
                    },
                    _ => self.answer(Err(LocationError::Denied)),
                }
            }

            #[unsafe(method(locationManager:didUpdateLocations:))]
            fn did_update_locations(
                &self,
                _manager: &CLLocationManager,
                locations: &NSArray<CLLocation>,
            ) {
                let position = locations.lastObject().map(|location| {
                    // SAFETY: a location the manager delivered is a valid CLLocation.
                    let coordinate = unsafe { location.coordinate() };
                    (coordinate.latitude, coordinate.longitude)
                });
                self.answer(position.ok_or(LocationError::Unavailable));
            }

            #[unsafe(method(locationManager:didFailWithError:))]
            fn did_fail(&self, _manager: &CLLocationManager, error: &NSError) {
                let refused =
                    [CLError::Denied.0, CLError::PromptDeclined.0].contains(&error.code());
                self.answer(Err(if refused {
                    LocationError::Denied
                } else {
                    LocationError::Unavailable
                }));
            }
        }
    );

    impl LocationDelegate {
        fn new(main: MainThreadMarker, answer: oneshot::Sender<Answer>) -> Retained<Self> {
            let this = Self::alloc(main).set_ivars(Ivars {
                answer: RefCell::new(Some(answer)),
            });
            // SAFETY: NSObject's designated initializer, called once.
            unsafe { msg_send![super(this), init] }
        }

        /// Deliver the first answer; later callbacks change nothing.
        fn answer(&self, answer: Answer) {
            if let Some(sender) = self.ivars().answer.borrow_mut().take() {
                let _ = sender.send(answer);
            }
        }
    }

    pub(super) async fn position() -> Result<Coordinates, LocationError> {
        let main = MainThreadMarker::new().ok_or(LocationError::Unavailable)?;
        let (sender, answer) = oneshot::channel();
        let delegate = LocationDelegate::new(main, sender);
        // SAFETY: the manager is created, used and released on the main
        // thread, whose run loop delivers its callbacks. It holds its delegate
        // weakly, so dropping both when a timeout cancels this future is sound.
        let manager = unsafe { CLLocationManager::new() };
        unsafe {
            manager.setDesiredAccuracy(kCLLocationAccuracyKilometer);
            manager.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        }
        let answer = answer.await;
        // SAFETY: as above; no callback reaches the delegate after this.
        unsafe { manager.setDelegate(None) };
        let (latitude, longitude) = answer.unwrap_or(Err(LocationError::Unavailable))?;
        Coordinates::new(latitude, longitude).map_err(|_| LocationError::Unavailable)
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::LocationError;
    use std::time::Duration;
    use windows::{
        Devices::Geolocation::{GeolocationAccessStatus, Geolocator, PositionAccuracy},
        Foundation::TimeSpan,
        Win32::Foundation::E_ACCESSDENIED,
    };
    use yt_geo::Coordinates;

    pub(super) const LOCATION_SETTINGS: &str = "ms-settings:privacy-location";
    /// A position measured this recently is still where the user is.
    const MAXIMUM_AGE: Duration = Duration::from_secs(10 * 60);
    /// Longest wait for a new position once access is allowed.
    const POSITION_TIMEOUT: Duration = Duration::from_secs(30);

    pub(super) async fn position() -> Result<Coordinates, LocationError> {
        // Windows 11 24H2 and later show their one-time consent prompt here.
        // Earlier versions may refuse the request outside packaged apps; the
        // position request below still follows the location settings.
        if let Ok(request) = Geolocator::RequestAccessAsync() {
            if let Ok(GeolocationAccessStatus::Denied) = request.await {
                return Err(LocationError::Denied);
            }
        }
        let refused = |error: windows::core::Error| {
            if error.code() == E_ACCESSDENIED {
                LocationError::Denied
            } else {
                LocationError::Unavailable
            }
        };
        let locator = Geolocator::new().map_err(refused)?;
        locator
            .SetDesiredAccuracy(PositionAccuracy::Default)
            .map_err(refused)?;
        let position = locator
            .GetGeopositionAsyncWithAgeAndTimeout(
                TimeSpan::from(MAXIMUM_AGE),
                TimeSpan::from(POSITION_TIMEOUT),
            )
            .map_err(refused)?
            .await
            .map_err(refused)?;
        let point = position
            .Coordinate()
            .and_then(|coordinate| coordinate.Point())
            .and_then(|point| point.Position())
            .map_err(refused)?;
        Coordinates::new(point.Latitude, point.Longitude).map_err(|_| LocationError::Unavailable)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod platform {
    use super::LocationError;
    use yt_geo::Coordinates;

    pub(super) const LOCATION_SETTINGS: &str = "";

    pub(super) async fn position() -> Result<Coordinates, LocationError> {
        Err(LocationError::Unavailable)
    }
}
