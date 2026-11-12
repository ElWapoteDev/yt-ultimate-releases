//! Song embeddings learned from the user's own listening sessions.
//!
//! Songs heard close together in a session are treated like words in a
//! sentence, as Item2Vec does (Barkan & Koenigstein 2016). Instead of training
//! skip-gram with negative sampling, the matrix that training implicitly
//! factorizes is built and factorized directly (Levy & Goldberg 2014): a
//! distance-weighted co-occurrence matrix becomes a shifted positive PMI
//! matrix, and its truncated SVD, computed by randomized subspace iteration
//! (Halko, Martinsson & Tropp 2011), gives one unit vector per song.

use crate::{artist_key, rng::SeededRng, ListeningSession, Profile};
use std::collections::{HashMap, HashSet};

/// Gram–Schmidt passes per column: two keep a basis orthonormal to working
/// precision («twice is enough»).
const GRAM_SCHMIDT_PASSES: usize = 2;
/// A column keeping less than this share of its norm after projection
/// depends on the previous ones and is dropped.
const RANK_TOLERANCE: f64 = 1.0e-10;
/// Jacobi rotations stop once the off-diagonal norm falls below this share of
/// the matrix norm.
const JACOBI_TOLERANCE: f64 = 1.0e-12;
/// Most Jacobi sweeps; cyclic Jacobi converges quadratically, usually within
/// ten.
const JACOBI_MAX_SWEEPS: usize = 64;
/// Vectors shorter than this carry no direction and are not embedded.
const MIN_VECTOR_NORM: f64 = 1.0e-9;
/// Independent partial sums per dot product, so the loop vectorizes.
const DOT_LANES: usize = 4;

/// How sessions become embeddings.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddingConfig {
    /// Listens on each side of a song that count as its context; a neighbor
    /// at distance d adds 1/d to the pair's co-occurrence.
    pub window: usize,
    /// Dimensions of each vector: the rank of the truncated SVD.
    pub dimensions: usize,
    /// k of shifted PPMI: entries keep max(PMI − ln k, 0). Values below 1
    /// (and NaN) count as 1, plain PPMI.
    pub shift: f64,
    /// Most frequently heard songs embedded; the rest are ignored.
    pub max_items: usize,
    /// Subspace (power) iterations of the randomized SVD; each costs two
    /// sparse products and two orthonormalizations.
    pub iterations: usize,
    /// Embeddings stay empty when fewer songs than this co-occur.
    pub min_items: usize,
    /// Random directions sampled beyond `dimensions` (Halko's p); 5–10 makes
    /// the captured subspace accurate.
    pub oversampling: usize,
    /// Exponent α in [0, 1] smoothing song frequencies in PMI (Levy, Goldberg
    /// & Dagan 2015); below 1 it tempers PMI's bias towards rarely heard
    /// songs. It smooths both marginals, so the matrix stays symmetric.
    pub context_smoothing: f64,
    /// Songs heard fewer times than this are not embedded: one listen is no
    /// evidence of what the song goes with.
    pub min_count: usize,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            window: 5,
            dimensions: 32,
            shift: 1.0,
            max_items: 2_000,
            iterations: 3,
            min_items: 8,
            oversampling: 8,
            context_smoothing: 0.75,
            min_count: 2,
        }
    }
}

/// Unit vectors for the songs of the user's sessions; songs heard in similar
/// contexts point in similar directions.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Embeddings {
    ids: Vec<String>,
    index: HashMap<String, usize>,
    /// Row-major, `ids.len()` × `dimensions`, every row of unit length.
    vectors: Vec<f64>,
    dimensions: usize,
}

impl Embeddings {
    /// Learn embeddings from `sessions`, deterministically for a `seed`.
    ///
    /// Within each session, every pair of songs at most `window` listens
    /// apart co-occurs with weight 1/distance (both directions). Only the
    /// `max_items` most frequent songs heard at least `min_count` times take
    /// part. Co-occurrences become shifted PPMI, a sparse symmetric matrix;
    /// its rank-`dimensions` randomized SVD U·Σ gives vectors U·Σ^½,
    /// normalized to unit length. Returns empty embeddings when fewer than
    /// `min_items` songs co-occur or `dimensions` is 0. Cost is linear in the
    /// listens for the matrix and O(n · (dimensions + oversampling)²) per
    /// iteration for n embedded songs.
    pub fn build(sessions: &[ListeningSession], config: &EmbeddingConfig, seed: u64) -> Self {
        let vocabulary = vocabulary(sessions, config);
        let index: HashMap<&str, usize> = vocabulary
            .iter()
            .enumerate()
            .map(|(position, id)| (*id, position))
            .collect();
        let entries = shifted_ppmi(
            cooccurrence(sessions, &index, config.window),
            vocabulary.len(),
            config,
        );
        let mut compact = vec![None; vocabulary.len()];
        let mut active = Vec::new();
        for &(row, _, _) in &entries {
            if compact[row].is_none() {
                compact[row] = Some(active.len());
                active.push(row);
            }
        }
        if active.len() < config.min_items.max(1) || config.dimensions == 0 {
            return Self::default();
        }
        // Rows appear in increasing order, so compacting keeps entries sorted.
        let matrix = SparseSymmetric::from_sorted(
            active.len(),
            entries
                .iter()
                .filter_map(|&(row, column, value)| Some((compact[row]?, compact[column]?, value))),
        );
        let rank = config.dimensions.min(active.len());
        let raw = factorize(
            &matrix,
            rank,
            config.oversampling,
            config.iterations,
            &mut SeededRng::new(seed),
        );
        let mut ids = Vec::new();
        let mut vectors = Vec::new();
        for (row, original) in raw.chunks_exact(rank).zip(&active) {
            let length = norm(row);
            if length > MIN_VECTOR_NORM {
                ids.push(vocabulary[*original].to_owned());
                vectors.extend(row.iter().map(|value| value / length));
            }
        }
        if ids.len() < config.min_items.max(1) {
            return Self::default();
        }
        let index = ids
            .iter()
            .enumerate()
            .map(|(position, id)| (id.clone(), position))
            .collect();
        Self {
            ids,
            index,
            vectors,
            dimensions: rank,
        }
    }

    /// Whether no song is embedded.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Songs embedded.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Length of every vector; 0 when empty.
    pub fn dimensions(&self) -> usize {
        self.dimensions
    }

    /// Whether `id` has a vector.
    pub fn contains(&self, id: &str) -> bool {
        self.index.contains_key(id)
    }

    /// The unit vector of `id`, if embedded.
    pub fn vector(&self, id: &str) -> Option<&[f64]> {
        self.index.get(id).map(|row| self.row(*row))
    }

    /// Cosine similarity in [-1, 1] of two embedded songs; `None` when either
    /// is not embedded.
    pub fn similarity(&self, a: &str, b: &str) -> Option<f64> {
        Some(cosine(self.vector(a)?, self.vector(b)?))
    }

    /// Up to `k` embedded songs most similar to `id`, best first, with their
    /// cosine similarity; never `id` itself or an id in `exclude`. Ties break
    /// by id. Empty when `id` is not embedded.
    pub fn nearest(&self, id: &str, k: usize, exclude: &HashSet<String>) -> Vec<(String, f64)> {
        let Some(query) = self.vector(id) else {
            return Vec::new();
        };
        let mut scored: Vec<(f64, usize)> = self
            .ids
            .iter()
            .enumerate()
            .filter(|(_, other)| other.as_str() != id && !exclude.contains(other.as_str()))
            .map(|(row, _)| (cosine(query, self.row(row)), row))
            .collect();
        scored.sort_by(|left, right| {
            right
                .0
                .total_cmp(&left.0)
                .then_with(|| self.ids[left.1].cmp(&self.ids[right.1]))
        });
        scored
            .into_iter()
            .take(k)
            .map(|(similarity, row)| (self.ids[row].clone(), similarity))
            .collect()
    }

    /// Cosine similarity of `candidate` to the normalized centroid of the
    /// embedded songs among `ids` (unknown ids are ignored); `None` when the
    /// candidate or every id is unknown, or the vectors cancel out.
    pub fn centroid_similarity(&self, ids: &[&str], candidate: &str) -> Option<f64> {
        let target = self.vector(candidate)?;
        let center = centroid(ids.iter().filter_map(|id| self.vector(id)), self.dimensions)?;
        Some(cosine(&center, target))
    }

    fn row(&self, row: usize) -> &[f64] {
        &self.vectors[row * self.dimensions..(row + 1) * self.dimensions]
    }
}

/// The normalized sum of unit `vectors` of length `dimensions`; `None` when
/// there are none or they cancel out.
pub(crate) fn centroid<'a>(
    vectors: impl IntoIterator<Item = &'a [f64]>,
    dimensions: usize,
) -> Option<Vec<f64>> {
    let mut sum = vec![0.0; dimensions];
    for vector in vectors {
        for (total, value) in sum.iter_mut().zip(vector) {
            *total += value;
        }
    }
    normalized(sum)
}

/// Each artist's direction (by `artist_key`): the normalized sum of the
/// vectors of their songs in `profile` that are embedded. Artists without
/// embedded songs are absent.
pub(crate) fn artist_vectors(
    profile: &Profile,
    embeddings: &Embeddings,
) -> HashMap<String, Vec<f64>> {
    let mut sums: HashMap<String, Vec<f64>> = HashMap::new();
    for item in profile.media() {
        let Some(vector) = embeddings.vector(&item.media_id) else {
            continue;
        };
        let key = artist_key(&item.artist);
        if key.is_empty() {
            continue;
        }
        let sum = sums
            .entry(key)
            .or_insert_with(|| vec![0.0; embeddings.dimensions()]);
        for (total, value) in sum.iter_mut().zip(vector) {
            *total += value;
        }
    }
    sums.into_iter()
        .filter_map(|(key, sum)| Some((key, normalized(sum)?)))
        .collect()
}

/// Dot product of two equally long slices (the shorter length otherwise).
pub(crate) fn dot(left: &[f64], right: &[f64]) -> f64 {
    let length = left.len().min(right.len());
    let split = length - length % DOT_LANES;
    let mut lanes = [0.0; DOT_LANES];
    for (left, right) in left[..split]
        .as_chunks::<DOT_LANES>()
        .0
        .iter()
        .zip(right[..split].as_chunks::<DOT_LANES>().0)
    {
        for ((lane, a), b) in lanes.iter_mut().zip(left).zip(right) {
            *lane += a * b;
        }
    }
    let tail: f64 = left[split..length]
        .iter()
        .zip(&right[split..length])
        .map(|(a, b)| a * b)
        .sum();
    lanes.iter().sum::<f64>() + tail
}

/// Cosine of two unit vectors, clamped against rounding.
fn cosine(left: &[f64], right: &[f64]) -> f64 {
    dot(left, right).clamp(-1.0, 1.0)
}

fn norm(values: &[f64]) -> f64 {
    dot(values, values).sqrt()
}

fn normalized(mut values: Vec<f64>) -> Option<Vec<f64>> {
    let length = norm(&values);
    if length <= MIN_VECTOR_NORM {
        return None;
    }
    values.iter_mut().for_each(|value| *value /= length);
    Some(values)
}

/// Songs heard at least `min_count` times, most frequent first (ties by id),
/// at most `max_items`.
fn vocabulary<'a>(sessions: &'a [ListeningSession], config: &EmbeddingConfig) -> Vec<&'a str> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for id in sessions.iter().flat_map(|session| &session.media_ids) {
        *counts.entry(id.as_str()).or_default() += 1;
    }
    let mut items: Vec<(&str, usize)> = counts
        .into_iter()
        .filter(|(_, count)| *count >= config.min_count)
        .collect();
    items.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));
    items.truncate(config.max_items);
    items.into_iter().map(|(id, _)| id).collect()
}

/// Distance-weighted co-occurrence as (row, column, weight) entries sorted by
/// row then column, symmetric and without duplicates. Songs outside the
/// vocabulary still hold their position, so distances stay true.
fn cooccurrence(
    sessions: &[ListeningSession],
    index: &HashMap<&str, usize>,
    window: usize,
) -> Vec<(usize, usize, f64)> {
    let mut pairs = Vec::new();
    for session in sessions {
        let items: Vec<Option<usize>> = session
            .media_ids
            .iter()
            .map(|id| index.get(id.as_str()).copied())
            .collect();
        for (position, item) in items.iter().enumerate() {
            let Some(item) = *item else {
                continue;
            };
            let reach = window.min(items.len() - position - 1);
            for distance in 1..=reach {
                let Some(other) = items[position + distance] else {
                    continue;
                };
                if other != item {
                    let weight = 1.0 / distance as f64;
                    pairs.push((item, other, weight));
                    pairs.push((other, item, weight));
                }
            }
        }
    }
    // A stable sort keeps each pair's additions in listening order, so both
    // directions sum identically and the matrix is exactly symmetric.
    pairs.sort_by_key(|&(row, column, _)| (row, column));
    let mut merged: Vec<(usize, usize, f64)> = Vec::with_capacity(pairs.len());
    for (row, column, weight) in pairs {
        match merged.last_mut() {
            Some(last) if last.0 == row && last.1 == column => last.2 += weight,
            _ => merged.push((row, column, weight)),
        }
    }
    merged
}

/// Shifted positive PMI of sorted co-occurrence entries over `size` songs:
/// PMI(a, b) = ln P(a, b) − ln P_α(a) − ln P_α(b) with marginals smoothed by
/// α; entries keep PMI − ln k when positive.
fn shifted_ppmi(
    entries: Vec<(usize, usize, f64)>,
    size: usize,
    config: &EmbeddingConfig,
) -> Vec<(usize, usize, f64)> {
    let mut marginal = vec![0.0; size];
    for &(row, _, weight) in &entries {
        marginal[row] += weight;
    }
    let total: f64 = marginal.iter().sum();
    if total <= 0.0 {
        return Vec::new();
    }
    let alpha = if config.context_smoothing.is_finite() {
        config.context_smoothing.clamp(0.0, 1.0)
    } else {
        1.0
    };
    let smoothed_total: f64 = marginal
        .iter()
        .filter(|weight| **weight > 0.0)
        .map(|weight| weight.powf(alpha))
        .sum();
    let log_marginal: Vec<f64> = marginal
        .iter()
        .map(|weight| if *weight > 0.0 { weight.ln() } else { 0.0 })
        .collect();
    let offset = 2.0 * smoothed_total.ln() - total.ln() - config.shift.max(1.0).ln();
    entries
        .into_iter()
        .filter_map(|(row, column, weight)| {
            let value = weight.ln() + offset - alpha * (log_marginal[row] + log_marginal[column]);
            (value > 0.0).then_some((row, column, value))
        })
        .collect()
}

/// A symmetric sparse matrix in compressed rows.
struct SparseSymmetric {
    size: usize,
    row_starts: Vec<usize>,
    columns: Vec<usize>,
    values: Vec<f64>,
}

impl SparseSymmetric {
    /// From entries sorted by row (then column), all within `size`.
    fn from_sorted(size: usize, entries: impl Iterator<Item = (usize, usize, f64)>) -> Self {
        let mut row_starts = vec![0; size + 1];
        let mut columns = Vec::new();
        let mut values = Vec::new();
        for (row, column, value) in entries {
            row_starts[row + 1] += 1;
            columns.push(column);
            values.push(value);
        }
        let mut running = 0;
        for start in &mut row_starts {
            running += *start;
            *start = running;
        }
        Self {
            size,
            row_starts,
            columns,
            values,
        }
    }

    /// This matrix times the row-major `size` × `width` matrix `input`.
    fn multiply(&self, input: &[f64], width: usize) -> Vec<f64> {
        let mut output = vec![0.0; self.size * width];
        for (row, sums) in output.chunks_exact_mut(width).enumerate() {
            for entry in self.row_starts[row]..self.row_starts[row + 1] {
                let value = self.values[entry];
                let column = self.columns[entry];
                for (sum, other) in sums
                    .iter_mut()
                    .zip(&input[column * width..(column + 1) * width])
                {
                    *sum += value * other;
                }
            }
        }
        output
    }
}

/// Rows U·Σ^½ (row-major, `size` × `rank`) of the rank-`rank` SVD of the
/// symmetric `matrix`, by randomized subspace iteration (Halko, Martinsson &
/// Tropp 2011, algorithms 4.4 and 5.1). `rank` is between 1 and the matrix
/// size.
fn factorize(
    matrix: &SparseSymmetric,
    rank: usize,
    oversampling: usize,
    iterations: usize,
    rng: &mut SeededRng,
) -> Vec<f64> {
    let size = matrix.size;
    let width = rank.saturating_add(oversampling).min(size);
    let probe: Vec<f64> = (0..size * width).map(|_| rng.normal()).collect();
    let mut basis = orthonormalize(&matrix.multiply(&probe, width), size, width);
    for _ in 0..iterations {
        // The matrix is symmetric, so its transpose is itself.
        let transposed = orthonormalize(&matrix.multiply(&basis, width), size, width);
        basis = orthonormalize(&matrix.multiply(&transposed, width), size, width);
    }
    // B = Qᵀ·A is the transpose of A·Q; the eigenvectors of B·Bᵀ are the
    // left singular vectors of B and its eigenvalues the squared singular
    // values.
    let projected = matrix.multiply(&basis, width);
    let (energies, rotation) = symmetric_eigen(gram(&projected, width), width);
    let scales: Vec<f64> = energies
        .iter()
        .take(rank)
        .map(|energy| energy.max(0.0).sqrt().sqrt())
        .collect();
    let mut vectors = vec![0.0; size * rank];
    for (output, coefficients) in vectors
        .chunks_exact_mut(rank)
        .zip(basis.chunks_exact(width))
    {
        for (coefficient, rotation_row) in coefficients.iter().zip(rotation.chunks_exact(width)) {
            for (value, factor) in output.iter_mut().zip(rotation_row) {
                *value += coefficient * factor;
            }
        }
        for (value, scale) in output.iter_mut().zip(&scales) {
            *value *= scale;
        }
    }
    vectors
}

/// The row-major `rows` × `width` matrix with orthonormal columns spanning
/// the columns of `matrix`; dependent columns become zero.
fn orthonormalize(matrix: &[f64], rows: usize, width: usize) -> Vec<f64> {
    let mut columns = transpose(matrix, rows, width);
    for column in 0..width {
        let (previous, rest) = columns.split_at_mut(column * rows);
        let current = &mut rest[..rows];
        let original = norm(current);
        for _ in 0..GRAM_SCHMIDT_PASSES {
            for basis in previous.chunks_exact(rows) {
                let projection = dot(basis, current);
                for (value, direction) in current.iter_mut().zip(basis) {
                    *value -= projection * direction;
                }
            }
        }
        let remaining = norm(current);
        if remaining > original * RANK_TOLERANCE && remaining > 0.0 {
            current.iter_mut().for_each(|value| *value /= remaining);
        } else {
            current.fill(0.0);
        }
    }
    transpose(&columns, width, rows)
}

/// The transpose of the row-major `rows` × `columns` matrix.
fn transpose(matrix: &[f64], rows: usize, columns: usize) -> Vec<f64> {
    let mut result = vec![0.0; rows * columns];
    for (row, values) in matrix.chunks_exact(columns).enumerate() {
        for (column, value) in values.iter().enumerate() {
            result[column * rows + row] = *value;
        }
    }
    result
}

/// Mᵀ·M (`width` × `width`) of the row-major matrix M with `width` columns.
fn gram(matrix: &[f64], width: usize) -> Vec<f64> {
    let mut result = vec![0.0; width * width];
    for values in matrix.chunks_exact(width) {
        for (sums, value) in result.chunks_exact_mut(width).zip(values) {
            for (sum, other) in sums.iter_mut().zip(values) {
                *sum += value * other;
            }
        }
    }
    result
}

/// Eigenvalues, descending, and eigenvectors (column k of the row-major
/// result belongs to eigenvalue k) of a symmetric `size` × `size` matrix, by
/// cyclic Jacobi rotations (Golub & Van Loan, «Matrix Computations», §8.5).
fn symmetric_eigen(mut matrix: Vec<f64>, size: usize) -> (Vec<f64>, Vec<f64>) {
    let mut vectors = vec![0.0; size * size];
    for diagonal in 0..size {
        vectors[diagonal * size + diagonal] = 1.0;
    }
    let total: f64 = matrix.iter().map(|value| value * value).sum();
    for _ in 0..JACOBI_MAX_SWEEPS {
        let mut off_diagonal = 0.0;
        for p in 0..size {
            for q in (0..size).filter(|q| *q != p) {
                off_diagonal += matrix[p * size + q] * matrix[p * size + q];
            }
        }
        if off_diagonal <= total * JACOBI_TOLERANCE * JACOBI_TOLERANCE {
            break;
        }
        for p in 0..size {
            for q in p + 1..size {
                rotate(&mut matrix, &mut vectors, size, p, q);
            }
        }
    }
    let mut order: Vec<usize> = (0..size).collect();
    order.sort_by(|left, right| {
        matrix[right * size + right]
            .total_cmp(&matrix[left * size + left])
            .then(left.cmp(right))
    });
    let values = order
        .iter()
        .map(|index| matrix[index * size + index])
        .collect();
    let mut sorted = vec![0.0; size * size];
    for (target, source) in order.iter().enumerate() {
        for row in 0..size {
            sorted[row * size + target] = vectors[row * size + source];
        }
    }
    (values, sorted)
}

/// One Jacobi rotation zeroing entry (p, q) of the symmetric `matrix`,
/// accumulated into `vectors`.
fn rotate(matrix: &mut [f64], vectors: &mut [f64], size: usize, p: usize, q: usize) {
    let off = matrix[p * size + q];
    if off == 0.0 {
        return;
    }
    let theta = (matrix[q * size + q] - matrix[p * size + p]) / (2.0 * off);
    // The smaller root of t² + 2θt − 1 = 0 keeps the rotation under 45°.
    let tangent = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
    let cosine = 1.0 / (tangent * tangent + 1.0).sqrt();
    let sine = tangent * cosine;
    for k in 0..size {
        let (left, right) = (matrix[k * size + p], matrix[k * size + q]);
        matrix[k * size + p] = cosine * left - sine * right;
        matrix[k * size + q] = sine * left + cosine * right;
    }
    for k in 0..size {
        let (left, right) = (matrix[p * size + k], matrix[q * size + k]);
        matrix[p * size + k] = cosine * left - sine * right;
        matrix[q * size + k] = sine * left + cosine * right;
    }
    for k in 0..size {
        let (left, right) = (vectors[k * size + p], vectors[k * size + q]);
        vectors[k * size + p] = cosine * left - sine * right;
        vectors[k * size + q] = sine * left + cosine * right;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        sessions,
        test_support::{group, grouped_history, NOW},
        ListenPolicy, SESSION_GAP_SECONDS,
    };

    fn two_groups(seed: u64) -> Vec<ListeningSession> {
        let groups = [
            group("a", 8, |index| format!("Artist {}", index % 3)),
            group("b", 8, |index| format!("Artist {}", index % 3)),
        ];
        let history = grouped_history(&groups, 12, NOW, seed);
        sessions(&history, SESSION_GAP_SECONDS, &ListenPolicy::default())
    }

    #[test]
    fn songs_that_share_sessions_are_closer_than_songs_that_never_meet() {
        let embeddings = Embeddings::build(&two_groups(1), &EmbeddingConfig::default(), 7);
        assert_eq!(embeddings.len(), 16);
        assert_eq!(embeddings.dimensions(), 16);
        let together = embeddings.similarity("a0", "a1").unwrap();
        let apart = embeddings.similarity("a0", "b0").unwrap();
        assert!(together > apart + 0.2, "together {together}, apart {apart}");
        assert!(apart.abs() < 1e-6, "groups never meet: {apart}");
        let center = embeddings.centroid_similarity(&["a1", "a2", "unknown"], "a3");
        assert!(center.unwrap() > embeddings.centroid_similarity(&["a1", "a2"], "b3").unwrap());
        assert_eq!(embeddings.similarity("a0", "unknown"), None);
        assert_eq!(embeddings.centroid_similarity(&["unknown"], "a0"), None);
        let vector = embeddings.vector("b5").unwrap();
        assert!((norm(vector) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn nearest_neighbors_stay_in_the_group_and_respect_exclusions() {
        let embeddings = Embeddings::build(&two_groups(2), &EmbeddingConfig::default(), 3);
        let exclude = HashSet::from(["a1".to_owned()]);
        let nearest = embeddings.nearest("a0", 4, &exclude);
        assert_eq!(nearest.len(), 4);
        assert!(nearest
            .iter()
            .all(|(id, _)| id.starts_with('a') && id != "a0" && id != "a1"));
        assert!(nearest.windows(2).all(|pair| pair[0].1 >= pair[1].1));
        assert!(embeddings.nearest("missing", 4, &exclude).is_empty());
    }

    #[test]
    fn equal_inputs_and_seeds_give_equal_embeddings() {
        let sessions = two_groups(5);
        let config = EmbeddingConfig::default();
        assert_eq!(
            Embeddings::build(&sessions, &config, 11),
            Embeddings::build(&sessions, &config, 11)
        );
        let truncated = EmbeddingConfig {
            dimensions: 4,
            ..config
        };
        let small = Embeddings::build(&sessions, &truncated, 11);
        assert_eq!(small.dimensions(), 4);
        assert!(small.similarity("a0", "a1").unwrap() > small.similarity("a0", "b1").unwrap());
    }

    #[test]
    fn too_little_listening_gives_empty_embeddings() {
        let few = [ListeningSession {
            media_ids: vec!["x".into(), "y".into(), "x".into(), "y".into()],
            started_at: NOW,
            ended_at: NOW,
        }];
        let embeddings = Embeddings::build(&few, &EmbeddingConfig::default(), 1);
        assert!(embeddings.is_empty());
        assert_eq!(embeddings.dimensions(), 0);
        assert!(Embeddings::build(&[], &EmbeddingConfig::default(), 1).is_empty());
        let no_window = EmbeddingConfig {
            window: 0,
            ..EmbeddingConfig::default()
        };
        assert!(Embeddings::build(&two_groups(1), &no_window, 1).is_empty());
    }

    #[test]
    fn jacobi_recovers_eigenpairs_of_a_symmetric_matrix() {
        let matrix = vec![4.0, 1.0, 0.0, 1.0, 3.0, 1.0, 0.0, 1.0, 2.0];
        let (values, vectors) = symmetric_eigen(matrix.clone(), 3);
        assert!(values.windows(2).all(|pair| pair[0] >= pair[1]));
        for (k, value) in values.iter().enumerate() {
            for row in 0..3 {
                let product: f64 = (0..3)
                    .map(|column| matrix[row * 3 + column] * vectors[column * 3 + k])
                    .sum();
                assert!((product - value * vectors[row * 3 + k]).abs() < 1e-9);
            }
        }
        assert!((values.iter().sum::<f64>() - 9.0).abs() < 1e-9);
    }
}
