//! The zone a device position belongs to: the main city of the area and its
//! country, named in Spanish, from an embedded table of populated places
//! (GeoNames, rebuilt with `cargo run -p xtask -- places`). Pure: no file,
//! network or clock access, and equal positions always give equal zones.

use std::{collections::HashMap, sync::LazyLock};
use yt_core::RegionPreferences;

/// Places this close to each other belong to one metropolitan area, in km.
pub const ZONE_RADIUS_KM: f64 = 30.0;
/// A nearest place farther than this is not the user's area (open sea,
/// polar regions), in km.
pub const MAX_PLACE_DISTANCE_KM: f64 = 200.0;
/// How many times its population a seat of a country or of a state counts
/// when the main city of an area is chosen. GeoNames counts suburban
/// municipalities on their own, and one can outnumber the core city it
/// belongs to (Zapopan, 1.48 million, beside Guadalajara, 1.39 million);
/// concerts are announced under the core city, which is the seat.
pub const SEAT_WEIGHT: u64 = 2;
/// Mean radius of the Earth (IUGG), in km.
const EARTH_RADIUS_KM: f64 = 6_371.008_8;

const PLACES: &str = include_str!("../data/places.tsv");
const COUNTRIES: &str = include_str!("../data/countries.tsv");

static TABLE: LazyLock<Vec<Place>> = LazyLock::new(|| parse(PLACES, COUNTRIES));

/// Why a value could not become [`Coordinates`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GeoError {
    /// Not finite, or a latitude outside −90…90 or a longitude outside
    /// −180…180 degrees.
    #[error("coordinates outside the range of the Earth")]
    InvalidCoordinates,
}

/// A position on the Earth's surface in decimal degrees (WGS 84).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coordinates {
    latitude: f64,
    longitude: f64,
}

impl Coordinates {
    /// Accepts finite degrees, latitude in −90…90 and longitude in −180…180;
    /// anything else is [`GeoError::InvalidCoordinates`].
    pub fn new(latitude: f64, longitude: f64) -> Result<Self, GeoError> {
        if (-90.0..=90.0).contains(&latitude) && (-180.0..=180.0).contains(&longitude) {
            Ok(Self {
                latitude,
                longitude,
            })
        } else {
            Err(GeoError::InvalidCoordinates)
        }
    }

    /// Degrees north of the equator (negative south).
    pub fn latitude(self) -> f64 {
        self.latitude
    }

    /// Degrees east of Greenwich (negative west).
    pub fn longitude(self) -> f64 {
        self.longitude
    }

    /// Great-circle distance to `other` on a spherical Earth (haversine), in
    /// km; within 0.5 % of the ellipsoidal distance.
    pub fn distance_km(self, other: Self) -> f64 {
        let (from, to) = (self.latitude.to_radians(), other.latitude.to_radians());
        let half_latitude = ((to - from) / 2.0).sin();
        let half_longitude = ((other.longitude - self.longitude).to_radians() / 2.0).sin();
        let chord =
            half_latitude * half_latitude + from.cos() * to.cos() * half_longitude * half_longitude;
        2.0 * EARTH_RADIUS_KM * chord.sqrt().min(1.0).asin()
    }
}

/// The main city of an area and its country, written in Spanish the way
/// concert listings print them («Ciudad de México», «México»). Both fit a
/// [`RegionPreferences`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Zone {
    /// The area's main city («Guadalajara» for a position in Zapopan).
    pub city: &'static str,
    /// The city's country.
    pub country: &'static str,
}

/// The zone around `position`. Starting at the nearest place, the zone moves
/// to the place of the same country with the most people within
/// [`ZONE_RADIUS_KM`] (seats counting [`SEAT_WEIGHT`] times) for as long as
/// one outweighs it, so a suburb leads to the city its concerts are announced
/// under. `None` when no place lies within [`MAX_PLACE_DISTANCE_KM`].
pub fn zone_at(position: Coordinates) -> Option<Zone> {
    let places = TABLE.as_slice();
    let nearest = places.iter().min_by(|a, b| {
        position
            .distance_km(a.position)
            .total_cmp(&position.distance_km(b.position))
    })?;
    if position.distance_km(nearest.position) > MAX_PLACE_DISTANCE_KM {
        return None;
    }
    let mut zone = nearest;
    while let Some(heavier) = places
        .iter()
        .filter(|place| {
            place.weight > zone.weight
                && place.country == zone.country
                && zone.position.distance_km(place.position) <= ZONE_RADIUS_KM
        })
        .max_by_key(|place| place.weight)
    {
        zone = heavier;
    }
    Some(Zone {
        city: zone.city,
        country: zone.country,
    })
}

struct Place {
    city: &'static str,
    country: &'static str,
    position: Coordinates,
    /// Population, multiplied by [`SEAT_WEIGHT`] for seats.
    weight: u64,
}

/// The rows of `places` whose country is in `countries` and whose names fit
/// a region preference; any other row is left out.
fn parse(places: &'static str, countries: &'static str) -> Vec<Place> {
    let names: HashMap<&str, &str> = rows(countries)
        .filter_map(|fields| match fields[..] {
            [code, name] => Some((code, name)),
            _ => None,
        })
        .collect();
    rows(places)
        .filter_map(|fields| {
            let [city, code, latitude, longitude, population, seat] = fields[..] else {
                return None;
            };
            let country = *names.get(code)?;
            let position =
                Coordinates::new(latitude.parse().ok()?, longitude.parse().ok()?).ok()?;
            let population: u64 = population.parse().ok()?;
            let weight = match seat {
                "1" => population.saturating_mul(SEAT_WEIGHT),
                "0" => population,
                _ => return None,
            };
            let region = RegionPreferences {
                city: city.to_owned(),
                country: country.to_owned(),
                ..RegionPreferences::default()
            };
            region.validate().is_ok().then_some(Place {
                city,
                country,
                position,
                weight,
            })
        })
        .collect()
}

/// Tab-separated fields of each data line; `#` lines are comments.
fn rows(table: &'static str) -> impl Iterator<Item = Vec<&'static str>> {
    table
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.split('\t').collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zone(latitude: f64, longitude: f64) -> Option<(&'static str, &'static str)> {
        zone_at(Coordinates::new(latitude, longitude).unwrap())
            .map(|zone| (zone.city, zone.country))
    }

    #[test]
    fn coordinates_reject_values_outside_the_earth() {
        assert!(Coordinates::new(90.0, -180.0).is_ok());
        assert!(Coordinates::new(-90.0, 180.0).is_ok());
        for (latitude, longitude) in [
            (90.1, 0.0),
            (0.0, -180.1),
            (f64::NAN, 0.0),
            (0.0, f64::INFINITY),
            (f64::NEG_INFINITY, 0.0),
        ] {
            assert_eq!(
                Coordinates::new(latitude, longitude),
                Err(GeoError::InvalidCoordinates)
            );
        }
    }

    #[test]
    fn distances_follow_the_great_circle() {
        let madrid = Coordinates::new(40.4168, -3.7038).unwrap();
        let barcelona = Coordinates::new(41.3851, 2.1734).unwrap();
        assert!((madrid.distance_km(barcelona) - 505.0).abs() < 3.0);
        assert_eq!(madrid.distance_km(madrid), 0.0);
        let east = Coordinates::new(0.0, 180.0).unwrap();
        let west = Coordinates::new(0.0, -180.0).unwrap();
        assert!(east.distance_km(west) < 1e-6);
    }

    #[test]
    fn every_row_of_the_embedded_table_is_used() {
        let places = rows(PLACES).count();
        assert!(places > 30_000, "{places} places");
        assert_eq!(TABLE.len(), places);
        assert_eq!(rows(COUNTRIES).count(), 252);
    }

    #[test]
    fn suburbs_lead_to_the_city_their_concerts_are_announced_under() {
        let cases = [
            ((19.4326, -99.1332), ("Ciudad de México", "México")),
            ((19.3553, -99.0622), ("Ciudad de México", "México")),
            ((19.6012, -99.0505), ("Ciudad de México", "México")),
            ((19.6469, -99.2466), ("Ciudad de México", "México")),
            ((19.2647, -98.8975), ("Ciudad de México", "México")),
            ((19.2826, -99.6557), ("Toluca", "México")),
            ((25.6866, -100.3161), ("Monterrey", "México")),
            ((25.6573, -100.4029), ("Monterrey", "México")),
            ((25.6775, -100.2597), ("Monterrey", "México")),
            ((20.6597, -103.3496), ("Guadalajara", "México")),
            ((20.7214, -103.3918), ("Guadalajara", "México")),
            ((20.4736, -103.4430), ("Guadalajara", "México")),
            ((19.0414, -98.2063), ("Puebla", "México")),
            ((21.1250, -101.6860), ("León", "México")),
            ((40.7831, -73.9712), ("Nueva York", "Estados Unidos")),
            ((40.6782, -73.9442), ("Nueva York", "Estados Unidos")),
            ((40.4168, -3.7038), ("Madrid", "España")),
            ((40.3057, -3.7329), ("Madrid", "España")),
            ((41.4500, 2.2474), ("Barcelona", "España")),
            ((4.5794, -74.2168), ("Bogotá", "Colombia")),
            ((6.1759, -75.5917), ("Medellín", "Colombia")),
            ((-12.0508, -77.1260), ("Lima", "Perú")),
            ((-33.6117, -70.5758), ("Santiago", "Chile")),
            ((-34.7206, -58.2546), ("Buenos Aires", "Argentina")),
            ((51.3762, -0.0982), ("Londres", "Reino Unido")),
        ];
        for ((latitude, longitude), expected) in cases {
            assert_eq!(
                zone(latitude, longitude),
                Some(expected),
                "{latitude}, {longitude}"
            );
        }
    }

    #[test]
    fn zones_stay_in_the_country_of_the_nearest_place() {
        assert_eq!(zone(32.5149, -117.0382), Some(("Tijuana", "México")));
        assert_eq!(
            zone(32.7157, -117.1611),
            Some(("San Diego", "Estados Unidos"))
        );
        assert_eq!(zone(31.6904, -106.4245), Some(("Ciudad Juárez", "México")));
        assert_eq!(
            zone(31.7619, -106.4850),
            Some(("El Paso", "Estados Unidos"))
        );
    }

    #[test]
    fn open_sea_has_no_zone() {
        assert_eq!(zone(-30.0, -120.0), None);
        assert_eq!(zone(0.0, -150.0), None);
    }
}
