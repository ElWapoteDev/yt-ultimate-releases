//! Population-weighted swatches: a coarse sRGB histogram whose buckets are
//! merged in OKLab.

use crate::color::{Oklab, Rgb, CHANNEL_MAX};
use std::cmp::Reverse;

/// Bits kept per sRGB channel when bucketing pixels: 16 levels per channel,
/// 4,096 buckets.
const QUANTIZATION_BITS: u32 = 4;
/// OKLab distance below which buckets join one swatch, about four
/// just-noticeable differences: gradients and compression noise collapse into
/// the color a person would name.
const SWATCH_MERGE_DISTANCE: f64 = 0.08;

/// A perceptual color cluster and the number of sampled pixels it covers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Swatch {
    pub color: Oklab,
    pub population: u64,
}

/// Swatches ordered by population, largest first. Ties keep the order in which
/// swatches formed, so equal input always yields equal output.
pub(crate) fn swatches(pixels: &[Rgb]) -> Vec<Swatch> {
    let mut bins = vec![Bin::default(); 1 << (3 * QUANTIZATION_BITS)];
    for &pixel in pixels {
        bins[bin_index(pixel)].add(pixel);
    }
    let mut buckets: Vec<Swatch> = bins
        .iter()
        .filter(|bin| bin.count > 0)
        .map(Bin::swatch)
        .collect();
    // Seeding clusters from the most populated buckets keeps their centers on
    // colors that are really present instead of drifting toward outliers.
    buckets.sort_by_key(|swatch| Reverse(swatch.population));
    let mut clusters: Vec<Cluster> = Vec::new();
    for bucket in buckets {
        let nearest = clusters
            .iter()
            .enumerate()
            .map(|(index, cluster)| (cluster.center.distance(bucket.color), index))
            .filter(|(distance, _)| *distance < SWATCH_MERGE_DISTANCE)
            .min_by(|first, second| first.0.total_cmp(&second.0))
            .map(|(_, index)| index);
        match nearest {
            Some(index) => clusters[index].absorb(bucket),
            None => clusters.push(Cluster::new(bucket)),
        }
    }
    let mut swatches: Vec<Swatch> = clusters.into_iter().map(Cluster::swatch).collect();
    swatches.sort_by_key(|swatch| Reverse(swatch.population));
    swatches
}

fn bin_index(pixel: Rgb) -> usize {
    let shift = u8::BITS - QUANTIZATION_BITS;
    (usize::from(pixel.r >> shift) << (2 * QUANTIZATION_BITS))
        | (usize::from(pixel.g >> shift) << QUANTIZATION_BITS)
        | usize::from(pixel.b >> shift)
}

#[derive(Debug, Clone, Copy, Default)]
struct Bin {
    count: u64,
    sums: [u64; 3],
}

impl Bin {
    fn add(&mut self, pixel: Rgb) {
        self.count += 1;
        for (sum, channel) in self.sums.iter_mut().zip([pixel.r, pixel.g, pixel.b]) {
            *sum += u64::from(channel);
        }
    }

    fn swatch(&self) -> Swatch {
        let count = self.count as f64;
        Swatch {
            color: Oklab::from_encoded(self.sums.map(|sum| sum as f64 / count / CHANNEL_MAX)),
            population: self.count,
        }
    }
}

struct Cluster {
    center: Oklab,
    weighted: [f64; 3],
    population: u64,
}

impl Cluster {
    fn new(seed: Swatch) -> Self {
        let weight = seed.population as f64;
        Self {
            center: seed.color,
            weighted: [seed.color.l, seed.color.a, seed.color.b].map(|value| value * weight),
            population: seed.population,
        }
    }

    fn absorb(&mut self, member: Swatch) {
        let weight = member.population as f64;
        for (sum, value) in
            self.weighted
                .iter_mut()
                .zip([member.color.l, member.color.a, member.color.b])
        {
            *sum += value * weight;
        }
        self.population += member.population;
        let total = self.population as f64;
        let [l, a, b] = self.weighted.map(|sum| sum / total);
        self.center = Oklab { l, a, b };
    }

    fn swatch(self) -> Swatch {
        Swatch {
            color: self.center,
            population: self.population,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn near_identical_shades_merge_and_distinct_colors_stay_apart() {
        let mut pixels = vec![Rgb::new(200, 30, 40); 60];
        pixels.extend([Rgb::new(206, 34, 44); 20]);
        pixels.extend([Rgb::new(20, 40, 200); 20]);
        let swatches = swatches(&pixels);
        assert_eq!(swatches.len(), 2);
        assert_eq!(swatches[0].population, 80);
        assert_eq!(swatches[1].population, 20);
        assert!(swatches[0].color.a > 0.1, "the red cluster comes first");
    }

    #[test]
    fn empty_input_has_no_swatches() {
        assert!(swatches(&[]).is_empty());
    }
}
