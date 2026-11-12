# Region

The zone that decides which concerts are «Cerca de ti». By default the application finds it from the device's
location, with the operating system's permission: the position is turned into the main city of the area and its
country on the device, and only those names are kept. The user can turn detection off and type a city and country
instead.

## Files

- `crates/yt-core/src/settings.rs`: `RegionPreferences { city, country, detect }`, its bounds (`MAX_CHARS`),
  `is_set`, `matches`, and the reading of older profiles (`StoredRegion`).
- `crates/yt-geo/`: `Coordinates`, `Zone` and `zone_at`, the zone around a position from the embedded GeoNames table
  (its `README.md` owns the algorithm, the data and the limits); `crates/xtask/src/places.rs` rebuilds the table.
- `crates/yt-app/src/native_location.rs`: `device_position` (Core Location on macOS, `Windows.Devices.Geolocation` on
  Windows), `LocationError` and `open_location_settings`.
- `crates/yt-app/src/region.rs`: the controller (`Region`, `Detection`, `detect_region_once`, `detect_region`,
  `set_region_detection`) and the detection status texts.
- `crates/yt-app/src/settings_ui.rs`: `RegionSection` (Ajustes → «Conciertos cerca de ti») and `TypedRegion`.
- `crates/yt-app/src/concerts_ui.rs`: `ConcertsSection` starts the session's detection and explains its state.
- `packaging/macos/Info.plist` (`NSLocationUsageDescription`, `NSLocationWhenInUseUsageDescription`, the text of the
  permission prompt) and `packaging/macos/entitlements.plist` (`com.apple.security.personal-information.location`,
  required by the hardened runtime of Developer ID builds); `crates/xtask/src/main.rs` passes the entitlements to the
  packager and checks the usage description of every packaged `.app`.

## Lifecycle

1. A new profile starts with «Detectar mi zona» on and no zone. A profile that typed a city or country before the
   preference existed reads as off and keeps its place (`StoredRegion` resolves the missing `detect`).
2. The first time concerts are shown in a session (an artist page) and detection is on, `detect_region_once` asks for
   the position. «Detectar ahora» in Ajustes asks again at any time, and turning detection on asks at once.
3. The first request shows the system's prompt with the text of the usage description: Core Location's on macOS, and
   the one-time consent prompt of Windows 11 24H2 and later. Older Windows versions answer from Settings → Privacy →
   Location («Permitir que las aplicaciones de escritorio accedan a tu ubicación»), without a prompt. Later requests
   use the answer the system keeps.
4. The position (about 1 km accurate: `kCLLocationAccuracyKilometer` on macOS, `PositionAccuracy::Default` with a
   position at most 10 minutes old on Windows) goes to `yt_geo::zone_at` on a blocking thread. The zone is saved as the
   region when it differs from the saved one; the position is dropped.
5. A refusal shows «YT Ultimate no tiene permiso para usar tu ubicación.» with «Abrir ajustes de ubicación» (Ajustes)
   or «Permitir ubicación» (concerts), which open the system's location privacy settings
   (`x-apple.systempreferences:…Privacy_LocationServices`, `ms-settings:privacy-location`). No position in time, or
   no place within 200 km, shows «No se pudo saber en qué zona estás.» A saved zone keeps being used in both cases.
6. Turning detection off keeps the last zone, which the city and country fields then show for editing; typing saves
   with detection off.

## Invariants

- The position is never stored, logged, rendered or sent anywhere; only the zone's names are saved, in the `region`
  preference group, like a typed city.
- Detection runs at most once per session by itself, and otherwise only when the user asks, so the system's
  location-in-use indicator does not keep appearing.
- With detection off the application never asks the operating system for the position.
- Every detection takes a generation; an answer that arrives after another detection started, or after the setting
  changed, is dropped (`apply_zone` also drops it while detection is off).
- The system's location objects are created, used and released on the UI thread, which delivers their callbacks;
  requests are asynchronous and never block it. The whole request, permission prompt included, gives up after 120
  seconds (`LOCATION_TIMEOUT`).

## Trust boundary

The operating system's answer is outside data: coordinates enter Rust through `yt_geo::Coordinates::new`, which
rejects anything outside the Earth's range, and errors become `LocationError::Denied` or `Unavailable`. The zone's
names come from the application's own table and pass `RegionPreferences::validate` before they are saved. The settings
links are fixed constants, never built from data.

## Performance

A detection costs one location request of the system (Wi-Fi based, no GPS) and one lookup in the place table on a
blocking thread: about 10 ms the first time in a session (parsing the table) and about 1 ms after. Nothing runs per
render or per poll tick.

## Tests

`crates/yt-core/src/settings.rs` (older profiles keep a typed place, explicit choices round-trip, invalid groups are
refused), `crates/yt-geo/src/lib.rs` (bounds, distances, the table, zones of metropolitan areas and borders),
`crates/xtask/src/places.rs` (Spanish name selection), `crates/yt-app/src/region.rs` (a zone becomes the region,
refusals keep it, late answers are dropped while detection is off), `crates/yt-app/src/settings_ui.rs` and
`crates/yt-app/src/concerts_ui.rs` (what each detection state shows). The system prompt itself is checked by running
the application, since tests have no location service.
