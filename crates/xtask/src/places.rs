//! `cargo run -p xtask -- places` rebuilds the offline place table of `yt-geo`
//! (`crates/yt-geo/data`) from the GeoNames export dump: every place of more
//! than 15,000 people and every capital, named in Spanish, and the Spanish
//! names of the countries. Downloads stay in `target/geonames`; delete that
//! folder to rebuild from a newer dump.

use crate::Result;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, File},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

const DUMP: &str = "https://download.geonames.org/export/dump/";
/// Places of more than 15,000 people, and capitals.
const CITIES: &str = "cities15000";
const COUNTRIES: &str = "countryInfo.txt";
const NAMES: &str = "alternateNamesV2";
/// ISO 639 language of the names, the language of the interface.
const LANGUAGE: &str = "es";
const DOWNLOADS: &str = "target/geonames";
const OUTPUT: &str = "crates/yt-geo/data";
/// Sections of a city, and historical, abandoned or destroyed places.
const SKIPPED_FEATURES: [&str; 5] = ["PPLX", "PPLH", "PPLQ", "PPLW", "PPLCH"];
/// Seats of a country or of a first-level division (state, province).
const SEAT_FEATURES: [&str; 2] = ["PPLC", "PPLA"];
const ATTRIBUTION: &str = "Contains data from GeoNames (https://www.geonames.org), licensed under CC BY 4.0 (https://creativecommons.org/licenses/by/4.0/); Spanish names and fields selected by `cargo run -p xtask -- places`.";

/// A Spanish name of a place, in dump order.
struct Name {
    text: String,
    preferred: bool,
}

struct City {
    id: u32,
    name: String,
    country: String,
    latitude: f64,
    longitude: f64,
    population: u64,
    seat: bool,
}

pub fn generate(root: &Path) -> Result {
    let downloads = root.join(DOWNLOADS);
    fs::create_dir_all(&downloads)?;
    let (cities, dumped) = read_cities(&fetch(&downloads, &format!("{CITIES}.zip"))?)?;
    let countries = read_countries(&fetch(&downloads, COUNTRIES)?)?;
    let wanted: HashSet<u32> = cities
        .iter()
        .map(|city| city.id)
        .chain(countries.keys().copied())
        .collect();
    let names = read_names(&fetch(&downloads, &format!("{NAMES}.zip"))?, &wanted)?;
    let country_names: BTreeMap<&str, String> = countries
        .iter()
        .filter_map(|(id, (code, english))| {
            let spanish = names
                .get(id)
                .and_then(|names| names.iter().find(|name| name.preferred).or(names.first()));
            clean(spanish.map_or(english.as_str(), |name| name.text.as_str()))
                .map(|name| (code.as_str(), name))
        })
        .collect();
    let mut rows: Vec<(String, &City)> = cities
        .iter()
        .filter(|city| country_names.contains_key(city.country.as_str()))
        .filter_map(|city| {
            let spanish = names.get(&city.id).map(Vec::as_slice).unwrap_or_default();
            clean(&city_name(&city.name, spanish)).map(|name| (name, city))
        })
        .collect();
    rows.sort_by(|(a_name, a), (b_name, b)| {
        (a.country.as_str(), a_name.as_str())
            .cmp(&(b.country.as_str(), b_name.as_str()))
            .then(a.latitude.total_cmp(&b.latitude))
            .then(a.longitude.total_cmp(&b.longitude))
    });
    let mut places = format!(
        "# Places of more than 15,000 people and capitals, named in Spanish, from the GeoNames dump of {dumped}.\n# {ATTRIBUTION}\n# name\tcountry\tlatitude\tlongitude\tpopulation\tseat\n"
    );
    for (name, city) in &rows {
        places.push_str(&format!(
            "{name}\t{}\t{:.3}\t{:.3}\t{}\t{}\n",
            city.country,
            city.latitude,
            city.longitude,
            city.population,
            u8::from(city.seat)
        ));
    }
    let mut table = format!(
        "# Country names in Spanish by ISO 3166-1 code, from the GeoNames dump of {dumped}.\n# {ATTRIBUTION}\n# code\tname\n"
    );
    for (code, name) in &country_names {
        table.push_str(&format!("{code}\t{name}\n"));
    }
    let output = root.join(OUTPUT);
    fs::create_dir_all(&output)?;
    fs::write(output.join("places.tsv"), places)?;
    fs::write(output.join("countries.tsv"), table)?;
    let named = cities
        .iter()
        .filter(|city| names.contains_key(&city.id))
        .count();
    println!(
        "Wrote {} places ({named} with a Spanish name, {} left out) and {} countries from the GeoNames dump of {dumped} into {OUTPUT}.",
        rows.len(),
        cities.len() - rows.len(),
        country_names.len()
    );
    Ok(())
}

fn fetch(directory: &Path, file: &str) -> Result<PathBuf> {
    let path = directory.join(file);
    if !path.is_file() {
        let partial = directory.join(format!("{file}.download"));
        crate::download(&format!("{DUMP}{file}"), &partial)?;
        fs::rename(&partial, &path)?;
    }
    Ok(path)
}

/// Living places of the cities dump, and the date of its newest change.
fn read_cities(path: &Path) -> Result<(Vec<City>, String)> {
    let mut archive = zip::ZipArchive::new(File::open(path)?)?;
    let entry = archive.by_name(&format!("{CITIES}.txt"))?;
    let mut cities = Vec::new();
    let mut dumped = String::new();
    for line in BufReader::new(entry).lines() {
        let line = line?;
        let fields: Vec<&str> = line.split('\t').collect();
        let [id, name, _, _, latitude, longitude, _, feature, country, _, _, _, _, _, population, _, _, _, modified] =
            fields[..]
        else {
            return Err(format!("Unexpected {CITIES} row: {line}").into());
        };
        if SKIPPED_FEATURES.contains(&feature) {
            continue;
        }
        if modified > dumped.as_str() {
            dumped = modified.to_owned();
        }
        cities.push(City {
            id: id.parse()?,
            name: name.to_owned(),
            country: country.to_owned(),
            latitude: latitude.parse()?,
            longitude: longitude.parse()?,
            population: if population.is_empty() {
                0
            } else {
                population.parse()?
            },
            seat: SEAT_FEATURES.contains(&feature),
        });
    }
    Ok((cities, dumped))
}

/// ISO code and English name of each country, by GeoNames id.
fn read_countries(path: &Path) -> Result<HashMap<u32, (String, String)>> {
    let mut countries = HashMap::new();
    for line in fs::read_to_string(path)?
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        let fields: Vec<&str> = line.split('\t').collect();
        let (Some(code), Some(english), Some(id)) = (fields.first(), fields.get(4), fields.get(16))
        else {
            return Err(format!("Unexpected {COUNTRIES} row: {line}").into());
        };
        if !id.is_empty() {
            countries.insert(id.parse()?, ((*code).to_owned(), (*english).to_owned()));
        }
    }
    Ok(countries)
}

/// Current Spanish names of the `wanted` places; historic names, names of a
/// past period and colloquial names are left out. A colloquial name that is
/// also the preferred one stays: GeoNames flags common names such as «Cali»
/// and «León» both ways.
fn read_names(path: &Path, wanted: &HashSet<u32>) -> Result<HashMap<u32, Vec<Name>>> {
    let mut archive = zip::ZipArchive::new(File::open(path)?)?;
    let entry = archive.by_name(&format!("{NAMES}.txt"))?;
    let mut names: HashMap<u32, Vec<Name>> = HashMap::new();
    for line in BufReader::new(entry).lines() {
        let line = line?;
        let mut fields = line.split('\t');
        let (Some(_), Some(id), Some(LANGUAGE), Some(text)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let flags: Vec<&str> = fields.collect();
        let flag = |index: usize| flags.get(index).copied().unwrap_or_default();
        let preferred = flag(0) == "1";
        if (flag(2) == "1" && !preferred) || flag(3) == "1" || !flag(5).is_empty() {
            continue;
        }
        let id = id.parse()?;
        if wanted.contains(&id) {
            names.entry(id).or_default().push(Name {
                text: text.to_owned(),
                preferred,
            });
        }
    }
    Ok(names)
}

/// How a city is written in Spanish: its preferred Spanish name (else the one
/// equal to its GeoNames name, else the first), cut to the shortest known name
/// it starts with («Toluca» for «Toluca de Lerdo»), because concert listings
/// often print the short form and a short name also matches the long one.
fn city_name(geonames: &str, names: &[Name]) -> String {
    let primary = names
        .iter()
        .find(|name| name.preferred)
        .or_else(|| {
            names
                .iter()
                .find(|name| name.text.to_lowercase() == geonames.to_lowercase())
        })
        .or(names.first())
        .map_or(geonames, |name| name.text.as_str());
    let primary_lower = primary.to_lowercase();
    names
        .iter()
        .map(|name| name.text.as_str())
        .chain([geonames])
        .filter(|candidate| starts_with_words(&primary_lower, &candidate.to_lowercase()))
        .min_by_key(|candidate| candidate.chars().count())
        .unwrap_or(primary)
        .to_owned()
}

/// Whether `text` begins with the whole words of `prefix`.
fn starts_with_words(text: &str, prefix: &str) -> bool {
    !prefix.is_empty()
        && text
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
}

/// `text` trimmed, or `None` when it is empty or holds control characters
/// (a tab or a line break would break the table).
fn clean(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty() && !text.chars().any(char::is_control)).then(|| text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[(&str, bool)]) -> Vec<Name> {
        list.iter()
            .map(|(text, preferred)| Name {
                text: (*text).to_owned(),
                preferred: *preferred,
            })
            .collect()
    }

    #[test]
    fn cities_take_the_short_spanish_form_people_write() {
        assert_eq!(
            city_name(
                "Mexico City",
                &names(&[("Ciudad de México", true), ("México", false)])
            ),
            "Ciudad de México"
        );
        assert_eq!(
            city_name("Toluca", &names(&[("Toluca de Lerdo", false)])),
            "Toluca"
        );
        assert_eq!(
            city_name(
                "Pachuca de Soto",
                &names(&[("Pachuca", false), ("Pachuca de Soto", false)])
            ),
            "Pachuca"
        );
        assert_eq!(city_name("Munich", &names(&[("Múnich", false)])), "Múnich");
        assert_eq!(
            city_name(
                "Ciudad Juárez",
                &names(&[("Ciudad Juárez", false), ("Juárez", false)])
            ),
            "Ciudad Juárez"
        );
        assert_eq!(
            city_name(
                "Tuxtla",
                &names(&[("Tuxtla Gutiérrez", false), ("Tuxtla Gtz.", false)])
            ),
            "Tuxtla"
        );
        assert_eq!(city_name("Monterrey", &[]), "Monterrey");
    }

    #[test]
    fn table_text_rejects_empty_names_and_control_characters() {
        assert_eq!(clean("  Lima "), Some("Lima".to_owned()));
        assert_eq!(clean("   "), None);
        assert_eq!(clean("Li\tma"), None);
        assert!(starts_with_words("toluca de lerdo", "toluca"));
        assert!(!starts_with_words("tolucan", "toluca"));
        assert!(!starts_with_words("toluca", ""));
    }
}
