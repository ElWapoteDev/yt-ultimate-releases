//! Harmonic compatibility on the Camelot wheel.

use yt_core::{KeyEstimate, KeyMode, PitchClass};

/// Largest [`key_distance`] at which two songs' harmony may overlap: the same
/// key, a neighbour on the wheel, or the relative major or minor.
pub const COMPATIBLE_KEYS: u8 = 1;
/// Largest move of a song's separated vocals that lets them ride over another
/// song's instrumental, in semitones: up to a whole tone a voice still sounds
/// like itself.
pub const MAX_VOCAL_SHIFT: i8 = 2;
/// Positions on the wheel.
const WHEEL: u8 = 12;

/// The key's position on the Camelot wheel: its number (1–12) and whether it
/// is a minor key ("A") or major ("B"). C major is 8B and A minor 8A.
pub fn camelot(key: KeyEstimate) -> (u8, KeyMode) {
    let tonic = key.tonic.value();
    let major = match key.mode {
        KeyMode::Major => tonic,
        KeyMode::Minor => (tonic + 3) % PitchClass::COUNT,
    };
    ((major * 7 % WHEEL + 7) % WHEEL + 1, key.mode)
}

/// Steps between two keys on the Camelot wheel: 0 for the same key, 1 for a
/// neighbour or the relative major or minor, more for keys that clash.
pub fn key_distance(a: KeyEstimate, b: KeyEstimate) -> u8 {
    let ((first, first_mode), (second, second_mode)) = (camelot(a), camelot(b));
    let apart = first.abs_diff(second);
    apart.min(WHEEL - apart) + u8::from(first_mode != second_mode)
}

/// `key` moved by `semitones` (negative: down), its mode unchanged.
pub fn transposed(key: KeyEstimate, semitones: i8) -> KeyEstimate {
    let count = i16::from(PitchClass::COUNT);
    let tonic = (i16::from(key.tonic.value()) + i16::from(semitones)).rem_euclid(count);
    KeyEstimate {
        // `rem_euclid` keeps the tonic below `PitchClass::COUNT`.
        tonic: PitchClass::new(tonic as u8).unwrap_or(key.tonic),
        ..key
    }
}

/// How many semitones a song's vocals in key `vocals` must move to ride over
/// an instrumental in key `instrumental`: 0 when the keys are compatible
/// already, otherwise the smallest move within [`MAX_VOCAL_SHIFT`] that makes
/// them so (the closest key first, then downwards, where a moved voice sounds
/// more natural). `None` when no such move does.
pub fn vocal_shift(vocals: KeyEstimate, instrumental: KeyEstimate) -> Option<i8> {
    (0..=MAX_VOCAL_SHIFT)
        .flat_map(|size| [-size, size])
        .map(|semitones| {
            (
                semitones,
                key_distance(transposed(vocals, semitones), instrumental),
            )
        })
        .filter(|(_, distance)| *distance <= COMPATIBLE_KEYS)
        .min_by_key(|(semitones, distance)| (semitones.abs(), *distance, *semitones))
        .map(|(semitones, _)| semitones)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(tonic: u8, mode: KeyMode) -> KeyEstimate {
        KeyEstimate {
            tonic: PitchClass::new(tonic).unwrap(),
            mode,
            confidence: 1.0,
        }
    }

    #[test]
    fn keys_sit_on_the_wheel_where_djs_expect() {
        assert_eq!(camelot(key(0, KeyMode::Major)), (8, KeyMode::Major));
        assert_eq!(camelot(key(9, KeyMode::Minor)), (8, KeyMode::Minor));
        assert_eq!(camelot(key(7, KeyMode::Major)), (9, KeyMode::Major));
        assert_eq!(camelot(key(11, KeyMode::Major)), (1, KeyMode::Major));
        assert_eq!(camelot(key(5, KeyMode::Major)), (7, KeyMode::Major));
        assert_eq!(camelot(key(4, KeyMode::Minor)), (9, KeyMode::Minor));
    }

    #[test]
    fn distances_follow_the_wheel() {
        let c = key(0, KeyMode::Major);
        assert_eq!(key_distance(c, c), 0);
        assert_eq!(key_distance(c, key(9, KeyMode::Minor)), 1, "relative minor");
        assert_eq!(key_distance(c, key(7, KeyMode::Major)), 1, "a fifth up");
        assert_eq!(key_distance(c, key(5, KeyMode::Major)), 1, "a fifth down");
        assert_eq!(key_distance(c, key(2, KeyMode::Major)), 2);
        assert_eq!(
            key_distance(c, key(6, KeyMode::Major)),
            6,
            "the tritone clashes most"
        );
        assert_eq!(
            key_distance(key(11, KeyMode::Major), key(4, KeyMode::Major)),
            1
        );
    }

    #[test]
    fn vocals_move_the_fewest_semitones_that_fit_the_other_key() {
        let c = key(0, KeyMode::Major);
        assert_eq!(vocal_shift(c, c), Some(0));
        assert_eq!(vocal_shift(c, key(7, KeyMode::Major)), Some(0), "a fifth");
        // C over D: only two up, D itself, fits.
        assert_eq!(vocal_shift(c, key(2, KeyMode::Major)), Some(2));
        // C over C sharp: one up is the same key.
        assert_eq!(vocal_shift(c, key(1, KeyMode::Major)), Some(1));
        // C over B: one down is the same key.
        assert_eq!(vocal_shift(c, key(11, KeyMode::Major)), Some(-1));
        // C over F sharp: C sharp and B are its neighbours on the wheel; down wins the tie.
        assert_eq!(vocal_shift(c, key(6, KeyMode::Major)), Some(-1));
        // C major over E flat minor: nothing within a whole tone fits.
        assert_eq!(vocal_shift(c, key(3, KeyMode::Minor)), None);
        assert_eq!(transposed(key(11, KeyMode::Minor), 2).tonic.value(), 1);
        assert_eq!(transposed(key(0, KeyMode::Minor), -1).tonic.value(), 11);
        for tonic in 0..12 {
            for other in 0..12 {
                let (a, b) = (key(tonic, KeyMode::Minor), key(other, KeyMode::Major));
                if let Some(semitones) = vocal_shift(a, b) {
                    assert!(semitones.abs() <= MAX_VOCAL_SHIFT);
                    assert!(key_distance(transposed(a, semitones), b) <= COMPATIBLE_KEYS);
                }
            }
        }
    }
}
