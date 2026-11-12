# yt-discovery

Pure listening affinity, taste modelling and recommendation ranking for YT Ultimate. It turns the listening facts
stored by `yt-core` (events, favorites, locally measured audio features) into a profile of what the user likes, learns
which songs go together from the user's own sessions, and builds the «made for you» lists: Home shelves, autoplay,
stations, a weekly discovery list, taste and mood mixes, time-of-day mixes, «Replay» and «En loop». There is a single
user and no data from other users, so nothing here is collaborative filtering across people: the related-media graph
comes from the candidates the application fetches from YouTube, and everything else from the user's own history.

It performs no file, network, or database access, never reads the clock (time is a parameter), draws randomness only
from seeds the caller passes, and returns equal output for equal input.

## Modules

| Module | Role |
|---|---|
| `listen` | `ListenMeter`, `ListenPolicy`, `ListenOutcome`: what was heard and how a listen ended. |
| `affinity` | `Profile`: long- and short-term affinity per song and artist. |
| `ranking` | `rank` and its presets, plus the filters, base score and duplicate rules that weekly lists and stations reuse. |
| `sessions` | Listening sessions split by pauses. |
| `embedding` | Song embeddings from sessions (shifted PPMI + randomized SVD). |
| `clusters` | Taste clusters (k-means++ over the embeddings). |
| `moods` | Valence/arousal of measured audio, mood memberships and mood mixes. |
| `context` | Parts of the day and time-of-day mixes. |
| `explore` | Beta beliefs per artist for Thompson sampling. |
| `diversity` | Maximal marginal relevance. |
| `replay` | «Replay» summaries and «En loop». |
| `weekly` | The weekly discovery list. |
| `station` | Station batches. |
| `rng` | Seeded generator, stable hashing, week seeds and ISO weeks. |
| `calendar` (private) | Local time at a UTC offset, Gregorian years, half-life decay. |

## API

| Item | Contract |
|---|---|
| `ListenMeter` | Accumulates media seconds actually heard from periodic position samples. An advance larger than the expected one (wall time × rate × 1.5 + 0.5 s) is a seek and is not counted; backward jumps, pauses and invalid positions never count. |
| `ListenPolicy::classify` | Turns one `yt_core::ListenSignal` into `ListenOutcome::{Completed, Partial(share), Skipped, Played, Ongoing(share)}`. Defaults: complete at 80 % heard, or a natural end after hearing at least half (seeking to the last seconds is a partial listen); a skip is an interruption under 30 s and under 50 %; imported history without lengths is `Played`. |
| `Profile::build(signals, favorites, now, &AffinityConfig)` | Per-media and per-artist affinity. Each listen scores by outcome (complete 1.0, partial 0.6 × share, skip −0.6, unknown 0.5), positive autoplay picks count 0.7×, and every listen decays with a 30-day half-life. A favorite adds 3.0 with a 365-day half-life. `MediaAffinity::short_term` / `ArtistAffinity::short_term` hold the same scores decayed with a 3-day half-life (`short_term_half_life_days`). Artists group by `artist_key`. |
| `AffinityConfig::listen_score(&signal)` | The undecayed score one listen adds, shared by every engine that weighs listens. |
| `Profile::{media, artists, media_affinity, artist_affinity, artist_score, config}` | Media and artists by descending long-term score; lookups by id and by artist display name (`artist_affinity` is `None` and `artist_score` 0 for unknown artists); the configuration the profile was built with. |
| `Profile::seeds(n)` / `top_artists(n)` | Liked media with a positive score, at most one per artist; liked artists. |
| `rank(candidates, &RankContext, &RankingConfig, limit)` | Orders `Candidate`s. Never returns an excluded id, a repeated id, a repeated song (`title_key`), more than `per_artist_cap` items per artist, media heard within `recent_hours`, or media skipped `skip_limit` times. Score = provider order (1 / (1 + rank × decay)) + artist affinity (tanh) + own affinity (tanh) + seed-artist continuity. Ties break with FNV-1a over the session salt, stable within a session. |
| `RankingConfig::{online, library_autoplay, rediscover}` | Presets: music-length online lists (60 s–15 min, 2 per artist, 12 h recency), library autoplay (any length, seed continuity) and «Redescubre» (14 days since last heard, own affinity first). Weekly lists and stations apply `online` to remote ids (`yt_core::REMOTE_ID_PREFIX`) and `library_autoplay` to library ids. |
| `artist_key`, `title_key` | Re-exported from `yt-core`, which also uses them to match lyrics. Comparison keys: channel decorations («VEVO», « - Topic», «official») and bracketed packaging («(Official Video)», «[Letra]», «[4K]»), featured artists and an «Artist - » (or «Artist – ») prefix are ignored; meaningful brackets such as «(Live)» or «(Remix)» stay. |
| `sessions(&signals, gap_seconds, &ListenPolicy) -> Vec<ListeningSession>` | Chronological sessions of non-skipped listens, any input order; a listen joins a session when it starts at most `gap_seconds` (`SESSION_GAP_SECONDS` = 30 min) after the session's latest end. |
| `Embeddings::build(&sessions, &EmbeddingConfig, seed)` | Unit song vectors; `is_empty`, `len`, `dimensions`, `contains`, `vector`, `similarity(a, b)` (cosine), `nearest(id, k, &exclude)` (never `id` or an excluded id), `centroid_similarity(&ids, candidate)`. Empty when fewer than `min_items` songs co-occur. |
| `taste_clusters(&profile, &embeddings, &ClusterConfig, seed) -> Vec<TasteCluster>` | Liked media grouped by listening context, heaviest first: `media_ids` by affinity, up to three `artists`, summed affinity `weight`. Falls back to artist groups without embeddings. |
| `Mood`, `MoodPoint`, `mood_point`, `mood_membership`, `mood_mix(mood, &profile, &features, limit, min_membership)` | Six moods with stable `key`s; a song's valence/arousal; memberships summing to 1; a mood mix of liked songs first, never songs skipped twice. |
| `DayPart`, `day_part(now, utc_offset)`, `day_part_mix(&signals, &profile, part, utc_offset, limit)` | Morning 05–12, afternoon 12–18, evening 18–22, night 22–05, local time; liked songs whose listening concentrates in a part. |
| `ArtistBeliefs::build(&signals, now, &ExploreConfig)` | Beta posterior per artist key; `posterior`, `mean`, `evidence`, `sample(artist, &mut rng)`; unknown artists keep the prior (uniform by default). |
| `mmr(&items, relevance, similarity, lambda, limit)` | Greedy maximal-marginal-relevance selection, deterministic, O(n · limit). |
| `ReplayEntry`, `replay(&signals, since, until, limit)`, `top_artists_by_time(&signals, since, until, limit)`, `on_repeat(&signals, now, window_days, min_plays, limit)` | Period summaries by heard seconds then plays; artists by heard seconds; songs played at least `min_plays` times in the last `window_days`. Plays exclude skips under `ListenPolicy::default()`. |
| `weekly_discovery(candidates, &profile, &beliefs, &embeddings, &context, week_seed, &WeeklyConfig)` | Up to 30 never-heard songs, one per artist, at most 30 % from known artists unless new ones run out; identical all week for one `week_seed`. |
| `station_picks(candidates, &profile, &beliefs, &embeddings, &seed_ids, &context, rng_seed, limit)` | The next station batch: continuity with the seeds, at most a quarter exploration, never a seed, a seed's song, an excluded id or a repeat. |
| `SeededRng`, `week_seed(now, utc_offset, salt)`, `iso_week(now, utc_offset)` | xoshiro256** with `next_u64`, `next_f64` in [0, 1), `normal`, `gamma(shape)`, `beta(a, b)` (NaN for shapes that are not finite or are below `MIN_SHAPE`); a seed constant from Monday 00:00 to Sunday 24:00 local time; the ISO 8601 week-numbering year and week. |

## Algorithms

### Affinity on two horizons

Each listen contributes `AffinityConfig::listen_score` (the outcome weights above) times a half-life decay
0.5^(age / half-life). The long-term `score` uses 30 days for listens and 365 days for favorites, unchanged from
earlier versions; `short_term` uses 3 days for both, so it follows what the user plays this week («lately») while
`score` keeps durable taste. Artist values are the sums over their media.

### Sessions

Listens are sorted by start; skips (by the given policy) are dropped because a rejected song says nothing about what
goes with what. A listen continues the current session when it starts at most the gap after the session's latest end
(start + seconds heard). The default gap, 30 minutes, is the usual inactivity timeout for splitting listening logs.

### Song embeddings: Item2Vec as an explicit factorization

Item2Vec (Barkan & Koenigstein 2016) applies word2vec's skip-gram with negative sampling (SGNS) to sequences of items.
Levy & Goldberg (2014) showed that SGNS with k negative samples implicitly factorizes the word-context PMI matrix
shifted by ln k. `Embeddings::build` therefore computes that factorization directly, which is deterministic, needs no
training epochs, and is fast for one user's history:

1. Vocabulary: songs heard at least `min_count` (2) times in sessions, the `max_items` (2 000) most frequent.
2. Co-occurrence: within a session, two songs `d` ≤ `window` (5) listens apart add 1/d in both directions, the
   harmonic distance weighting of GloVe (Pennington, Socher & Manning 2014). Songs outside the vocabulary keep their
   positions, so distances stay true.
3. PMI(a, b) = ln P(a, b) − ln P_α(a) − ln P_α(b) with smoothed marginals P_α(x) = C(x)^α / Σ C^α,
   α = `context_smoothing` (0.75), the context-distribution smoothing of Levy, Goldberg & Dagan (2015) that tempers
   PMI's bias towards rarely heard songs. It is applied to both marginals so the matrix stays symmetric (songs are
   their own contexts). Entries keep max(PMI − ln k, 0) with k = `shift` (1, plain PPMI: shifting mostly helps SGNS,
   while explicit factorizations do best with little or no shift, Levy, Goldberg & Dagan 2015).
4. Truncated SVD by randomized subspace iteration (Halko, Martinsson & Tropp 2011, algorithms 4.4 and 5.1): a Gaussian
   test matrix with `dimensions` (32) + `oversampling` (8) columns, `iterations` (3) power iterations with the
   basis re-orthonormalized after every product (modified Gram–Schmidt, two passes; Giraud, Langou & Rozložník 2005),
   then B = QᵀA and the SVD of B from the eigendecomposition of BBᵀ by cyclic Jacobi rotations (Golub & Van Loan 2013,
   §8.5).
5. Vectors are U·Σ^½ (the symmetric eigenvalue weighting Levy, Goldberg & Dagan 2015 recommend), normalized to unit
   length; similarity is the cosine.

Limits: a song needs two listens in sessions to get a vector, so new candidates rely on artist continuity instead;
only cosines are meaningful (axes and signs are arbitrary and change with the data); with little history the vectors
are noisy, which the low rank partly regularizes; embeddings are empty below `min_items` (8) co-occurring songs.

### Taste clusters

Points are liked media (positive long-term score) with a vector, weighted by their affinity. The cluster count is the
rule of thumb k ≈ √(n / 2) (Mardia, Kent & Bibby 1979), at most `max_clusters` (6) and at most n / `min_cluster_size`
(4). Centers are seeded by k-means++ (Arthur & Vassilvitskii 2007) in its greedy form, which draws 2 + ⌊ln k⌋
candidates per step with probability ∝ weight × squared distance and keeps the one that lowers the potential most
(Grunau, Özüdoğru, Rozhoň & Tětek 2023), followed by weighted Lloyd iterations (at most `iterations`, 25) until the
assignments settle. Liked media without a vector (favorites never heard in a session, songs heard once) join the
cluster holding most of their artist's affinity. Clusters smaller than `min_cluster_size` are dropped; the rest are
ordered by summed affinity and named after their three most liked artists. When fewer than `min_cluster_size` liked
media are embedded, clusters are the liked media of each artist instead. Deterministic for a seed.

### Moods on the circumplex

Russell's (1980) circumplex places affect on two axes, valence (unpleasant–pleasant) and arousal (calm–activated).
Following the music-emotion literature (Gabrielsson & Lindström 2010; Yang & Chen 2012), loudness, onset density and
tempo carry arousal, while mode is the strongest structural cue for valence, with fast tempo and bright timbre leaning
pleasant. Each `AudioFeatures` field is placed on [−1, 1] within a typical span, converted through the scale
constants `AudioFeatures` declares: loudness −30 to −9 dBFS, tempo 60 to 180 BPM (unknown tempo is neutral), 1 to 6
onsets per second, spectral centroid 700 Hz to 3.3 kHz, tonality ±0.25. Then

- arousal = 0.35 loudness + 0.3 onset density + 0.25 tempo + 0.1 brightness,
- valence = 0.6 mode + 0.25 tempo + 0.15 brightness.

| Mood | Prototype (valence, arousal) | Affect words near it (Russell 1980) |
|---|---|---|
| `energetic` | (0.25, 0.9) | aroused, alert |
| `party` | (0.75, 0.65) | excited, delighted |
| `calm` | (0.5, −0.65) | relaxed, serene |
| `focus` | (0.05, −0.2) | near-neutral, slightly low arousal |
| `melancholic` | (−0.65, −0.55) | sad, gloomy |
| `intense` | (−0.6, 0.75) | tense, angry |

Memberships are a softmax of −d² / (2σ²) over the squared distances to the prototypes, σ = 0.35. `mood_mix` keeps
songs whose membership reaches `min_membership`, drops songs skipped twice or more, ranks liked songs first by
membership × tanh(affinity) and the rest (including analyzed songs the profile does not know) by membership ×
(1 + tanh(affinity)), and spreads artists within each group with MMR (λ 0.7). Limits: spans, weights and prototypes
are set from the literature, not trained on labelled data; tonality is a weak cue for atonal or percussive music;
lyrics are not considered.

### Time of day

Each listen's local hour (at the caller's UTC offset, clamped to ±18 h) is an angle on the 24-hour circle. A von Mises
kernel exp(κ (cos Δθ − 1)) with κ = 2 (it halves about 3.3 h away) weighs the listen against the center of a part
(08:30, 15:00, 20:00, 01:30; Mardia & Jupp 2000). A song's share in the part is Σ score × kernel over its total
absolute score plus a one-listen prior, so a single listen is weak evidence and skips count against it. Songs qualify
when their share beats that of a listener without time habits, the kernel's mean over a uniform day e^(−κ) I₀(κ) ≈
0.31; only liked songs skipped fewer than twice qualify, ranked by share × tanh(affinity).

### Exploration by Thompson sampling

`ArtistBeliefs` keeps a Beta(α₀ + S, β₀ + F) posterior of how often each artist's songs are enjoyed: a complete
listen adds one success, a partial one its share heard, an imported play 0.5, and a skip one failure. Evidence decays
with a 90-day half-life, a discounted Thompson sampler that follows changing taste (Raj & Kalyani 2017). Sampling the
posterior (Chapelle & Li 2011) balances exploiting liked artists and exploring uncertain ones, the explore/exploit
balance that BaRT brought to music recommendation (McInerney et al. 2018); unknown artists draw from the prior,
uniform Beta(1, 1) by default. Weekly lists and stations draw each artist's sample from a generator seeded by the list
seed and the artist key, so one artist's draw never depends on which other candidates were fetched.

### Diversity by maximal marginal relevance

`mmr` (Carbonell & Goldstein 1998) repeatedly takes the item maximizing λ · relevance − (1 − λ) · max similarity to
the items already taken. λ = 1 is plain ranking, λ = 0 pure novelty; ties go to the earlier item. Cost O(n · limit).

### Replay and «En loop»

`replay` sums media seconds heard per song over a half-open period and counts plays (listens that are not skips under
`ListenPolicy::default()`), ordered by seconds, then plays; songs that were only skipped are left out.
`top_artists_by_time` sums seconds per `artist_key`, named after the latest listen. `on_repeat` counts plays in the
last `window_days` and keeps songs with at least `min_plays`, most plays first, then the most recent.

### Weekly discovery

Candidates are the related media the application fetched (for example the provider's lists for the user's seeds).
Only media the profile does not know (never heard nor favorited) pass, never another upload of a song the profile
holds (same `artist_key` and `title_key`), never an excluded id, and only lengths the source's preset accepts.
Relevance = provider order + artist affinity (as `rank` scores them) + `exploration_weight` (0.5) × the artist's
Thompson sample + `novelty_weight` (0.2) for artists never heard. The best `per_artist_cap` (1) songs per artist and
one upload per song survive; MMR (λ `mmr_lambda`, 0.7) orders them with similarity 1 within an artist and the cosine
of the artists' embedding directions otherwise (an artist's direction is the normalized sum of their embedded songs).
At most `known_artist_share` (0.3) × `limit` (30) picks come from artists already heard; they fill past that share
only when new artists run out. Every draw and tie-break derives from `week_seed`, so the list holds all week.

### Stations

Candidates pass their source preset's filters and are never a seed, another upload of a seed's song, or a repeat.
Continuity relevance is the preset's `rank` score with every seed's artist (and the context's seed artist) earning
the seed-artist bonus, plus 0.6 × the cosine between the candidate (its own vector, or its artist's direction) and
the normalized centroid of the seeds' vectors. Every fourth position, at most a quarter of the batch, is an
exploration slot taken by the unheard song of an artist with under two decayed listens of evidence, ranked by its
Thompson sample plus provider order. Both orders are diversified by MMR (λ 0.75), each artist stays under its
preset's cap, and draws and tie-breaks derive from `rng_seed`.

### Seeded randomness and weeks

`SeededRng` is xoshiro256** (Blackman & Vigna 2021) with its state filled by SplitMix64 (Steele, Lea & Flood 2014).
Normal draws use Box–Muller; gamma draws use Marsaglia & Tsang (2000), with the U^(1/a) boost for shapes below 1, kept
in logarithms so tiny shapes do not underflow; Beta draws are X / (X + Y) of two gamma draws, also in logarithms.
Tie-breaks hash with FNV-1a, fixed across builds. `week_seed` mixes the salt with the day number of the local Monday;
`iso_week` takes the year and week of that week's Thursday (ISO 8601); calendar dates follow Hinnant's
`civil_from_days` and `days_from_civil`.

## References

- Arthur, D. & Vassilvitskii, S. (2007). k-means++: The Advantages of Careful Seeding. *Proceedings of the 18th Annual
  ACM-SIAM Symposium on Discrete Algorithms (SODA)*, 1027–1035.
- Barkan, O. & Koenigstein, N. (2016). Item2Vec: Neural Item Embedding for Collaborative Filtering. *IEEE 26th
  International Workshop on Machine Learning for Signal Processing (MLSP)*.
- Blackman, D. & Vigna, S. (2021). Scrambled Linear Pseudorandom Number Generators. *ACM Transactions on Mathematical
  Software* 47(4), 36.
- Carbonell, J. & Goldstein, J. (1998). The Use of MMR, Diversity-Based Reranking for Reordering Documents and
  Producing Summaries. *Proceedings of SIGIR '98*, 335–336.
- Chapelle, O. & Li, L. (2011). An Empirical Evaluation of Thompson Sampling. *Advances in Neural Information
  Processing Systems 24*.
- Gabrielsson, A. & Lindström, E. (2010). The Role of Structure in the Musical Expression of Emotions. In P. N. Juslin
  & J. A. Sloboda (Eds.), *Handbook of Music and Emotion: Theory, Research, Applications*. Oxford University Press.
- Giraud, L., Langou, J. & Rozložník, M. (2005). The Loss of Orthogonality in the Gram–Schmidt Orthogonalization
  Process. *Computers & Mathematics with Applications* 50(7), 1069–1075.
- Golub, G. H. & Van Loan, C. F. (2013). *Matrix Computations* (4th ed.). Johns Hopkins University Press.
- Grunau, C., Özüdoğru, A. A., Rozhoň, V. & Tětek, J. (2023). A Nearly Tight Analysis of Greedy k-means++.
  *Proceedings of the 2023 ACM-SIAM Symposium on Discrete Algorithms (SODA)*.
- Halko, N., Martinsson, P.-G. & Tropp, J. A. (2011). Finding Structure with Randomness: Probabilistic Algorithms for
  Constructing Approximate Matrix Decompositions. *SIAM Review* 53(2), 217–288.
- Hinnant, H. *chrono-Compatible Low-Level Date Algorithms*. https://howardhinnant.github.io/date_algorithms.html
- Levy, O. & Goldberg, Y. (2014). Neural Word Embedding as Implicit Matrix Factorization. *Advances in Neural
  Information Processing Systems 27*.
- Levy, O., Goldberg, Y. & Dagan, I. (2015). Improving Distributional Similarity with Lessons Learned from Word
  Embeddings. *Transactions of the Association for Computational Linguistics* 3, 211–225.
- Mardia, K. V. & Jupp, P. E. (2000). *Directional Statistics*. Wiley.
- Mardia, K. V., Kent, J. T. & Bibby, J. M. (1979). *Multivariate Analysis*. Academic Press.
- Marsaglia, G. & Tsang, W. W. (2000). A Simple Method for Generating Gamma Variables. *ACM Transactions on
  Mathematical Software* 26(3), 363–372.
- McInerney, J., Lacker, B., Hansen, S., Higley, K., Bouchard, H., Gruson, A. & Mehrotra, R. (2018). Explore, Exploit,
  and Explain: Personalizing Explainable Recommendations with Bandits. *Proceedings of the 12th ACM Conference on
  Recommender Systems (RecSys '18)*, 31–39.
- Pennington, J., Socher, R. & Manning, C. D. (2014). GloVe: Global Vectors for Word Representation. *Proceedings of
  EMNLP 2014*, 1532–1543.
- Raj, V. & Kalyani, S. (2017). Taming Non-stationary Bandits: A Bayesian Approach. arXiv:1707.09727.
- Russell, J. A. (1980). A Circumplex Model of Affect. *Journal of Personality and Social Psychology* 39(6),
  1161–1178.
- Steele, G. L., Lea, D. & Flood, C. H. (2014). Fast Splittable Pseudorandom Number Generators. *Proceedings of
  OOPSLA 2014*.
- Yang, Y.-H. & Chen, H. H. (2012). Machine Recognition of Music Emotion: A Review. *ACM Transactions on Intelligent
  Systems and Technology* 3(3), 40.

## Trust boundary

Inputs are typed values already validated by `yt-core` (database rows, `AudioFeatures::validate`) or by the
application (provider results converted to `RemoteMedia`). Engines only reorder, filter and summarize; they never
create identities and never fetch anything. Invalid numbers degrade gracefully: non-finite instants count as the epoch,
UTC offsets are clamped to ±18 h, NaN relevance never wins a selection, invalid gamma or Beta shapes return NaN instead
of looping, and invalid priors fall back to the uniform prior.

## Performance

Building a profile from 2 000 events and ranking 100 candidates is linear in the inputs and allocation-light. The
embedding is linear in listens × window for the sparse matrix and O(n · (dimensions + oversampling)²) per power
iteration for n songs; clusters cost O(n · k · dimensions) per Lloyd iteration. Measured in a release build on Apple
Silicon (`a_full_history_embeds_and_clusters_in_well_under_100_ms`): 5 000 events over 1 500 songs, all embedded,
take 12–23 ms for sessions and embeddings and 14–26 ms with the six taste clusters (three runs, varying with machine
load). Peak memory stays in the low megabytes (a few n × 40 dense matrices of `f64`). The application runs these
engines off the UI thread, with the database reads, and caches their results.

## Limits

- Bitwise equal output on one platform. `ln`, `exp` and `powf` come from the platform's math library, so macOS and
  Windows may differ in the last bits, which can reorder near-ties between machines.
- Similar-song knowledge comes only from the user's own sessions; songs the user never heard are related through the
  candidates the application fetched and through their artists.
- `replay` and `on_repeat` judge skips with `ListenPolicy::default()`.

## Verification

`cargo test -p yt-discovery` covers the meter's seek handling, outcome thresholds, decay, favorites and the short-term
horizon, seed diversity, every ranking filter, duplicate and per-artist limits, continuity and the stability of
tie-breaks; generator determinism, gamma and Beta means and invalid shapes; ISO weeks across year boundaries and week
seeds with an offset; session splitting; embeddings that place co-listened songs closer than songs that never meet,
`nearest` exclusions and determinism; Jacobi eigenpairs; clusters that separate two listening groups across shared
artists and the artist fallback; circumplex orderings, memberships summing to 1 and mood-mix tiers; night mixes;
Thompson sampling favoring completed over skipped artists; MMR replacing near-duplicates; replay and «En loop»
ordering; weekly lists that skip heard music, cap artists and hold for a week seed; and station exclusions,
continuity and bounded exploration. The timing check is ignored by default:
`cargo test --release -p yt-discovery -- --ignored --nocapture`.
