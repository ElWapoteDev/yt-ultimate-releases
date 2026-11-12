//! Measures forced alignment of known lyrics with Whisper on the Metal GPU:
//! word onset errors against JamendoLyrics MultiLang and the real-time factor.
//! It reads a Hugging Face Whisper folder (`config.json`,
//! `generation_config.json`, `tokenizer.json`, `model.safetensors`) and a local
//! copy of the dataset, and sends nothing over the network.
//!
//! ```sh
//! cargo run --locked --release -p yt-media --features metal-align \
//!   --example lyrics_align_probe -- <whisper-dir> <jamendolyrics-dir> \
//!   [all|de,en,es,fr] [songs, 0 = all] [f32|f16] [variant]
//! ```
//!
//! The variant lists `open`, `pad`, `trim`, `clip[=factor]`, `shift[=seconds]`,
//! `lines=seconds` and `calibrate`, separated by commas, or `strict` for none;
//! `calibrate` also measures every correction of `calibration_grid` from the
//! same alignment.
//!
//! `YTU_ALIGN_TRACE` prints every window, `YTU_ALIGN_WORDS` every word,
//! `YTU_ALIGN_CPU` runs on the CPU instead of Metal, for comparison, and
//! `YTU_ALIGN_LRC_DIR` writes every aligned song there as enhanced LRC.
//!
//! See `docs/LYRICS_ALIGNMENT.md` for the method and the measured results.

#[cfg(all(feature = "metal-align", target_os = "macos"))]
fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    probe::run()
}

#[cfg(not(all(feature = "metal-align", target_os = "macos")))]
fn main() {
    eprintln!("This probe requires macOS and --features metal-align.");
}

#[cfg(all(feature = "metal-align", target_os = "macos"))]
mod probe {
    use candle_core::{DType, Device, IndexOp, Tensor};
    use candle_nn::{ops::softmax_last_dim, Conv1d, Conv1dConfig, LayerNorm, Module, VarBuilder};
    use rodio::Source;
    use rustfft::{num_complex::Complex, FftPlanner};
    use serde::Deserialize;
    use std::{
        error::Error,
        fs,
        path::{Path, PathBuf},
        time::{Duration, Instant},
    };
    use tokenizers::Tokenizer;

    type Res<T> = Result<T, Box<dyn Error + Send + Sync>>;

    const SAMPLE_RATE: u32 = 16_000;
    const N_FFT: usize = 400;
    const HOP: usize = 160;
    /// Mel frames in one 30-second Whisper window.
    const WINDOW_FRAMES: usize = 3_000;
    const MEL_FRAMES_PER_SECOND: f64 = 100.0;
    /// Encoder frames per second: the encoder halves the mel frame rate.
    const ENCODER_FRAMES_PER_SECOND: f64 = 50.0;
    const ENCODER_FRAMES: usize = 1_500;
    /// Most lyric tokens fed to one window, as in stable-ts.
    const TOKEN_BUDGET: usize = 100;
    const MEDIAN_WIDTH: usize = 7;
    /// A word longer than this spans a gap, as in stable-ts.
    const MAX_WORD_SECONDS: f64 = 3.0;
    const WORD_DURATION_FACTOR: f64 = 2.0;
    const MAX_RESEEKS: usize = 4;
    const PCO_TOLERANCE_SECONDS: f64 = 0.3;
    const GROSS_ERROR_SECONDS: f64 = 1.0;
    const RESAMPLE_ZERO_CROSSINGS: f64 = 16.0;
    const RESAMPLE_CUTOFF: f64 = 0.95;
    const LANGUAGES: [&str; 4] = ["de", "en", "es", "fr"];
    /// Text fed before the lyrics of each window, as stable-ts does, so the
    /// decoder's pull toward the window start lands on it instead of on the
    /// first word.
    const GAP_PADDING: &str = " ...";
    /// Below this probability a word at the end of a window is probably not
    /// sung in that window yet.
    const LOW_PROBABILITY: f32 = 0.05;
    /// A word this short (one encoder frame) was squeezed in by the path.
    const INSTANT_WORD_SECONDS: f64 = 0.02;
    /// This many squeezed words in a row mean the path lost the singing, so
    /// the window keeps only the words before them.
    const INSTANT_RUN: usize = 3;
    /// Whisper's cap on the typical word duration (`add_word_timestamps`).
    const TYPICAL_WORD_CAP_SECONDS: f64 = 0.7;
    /// Whisper's longest word after a sentence mark, in typical words.
    const LINE_START_CLIP_FACTOR: f64 = 2.0;
    /// Word delays `calibrate` measures, in seconds.
    const WORD_DELAYS: [f64; 4] = [0.0, 0.05, 0.1, 0.15];
    /// Line-start treatments `calibrate` measures: the clip factor, if any, and
    /// the extra delay of each line's first word in seconds.
    const LINE_TREATMENTS: [(Option<f64>, f64); 9] = [
        (None, 0.0),
        (Some(2.0), 0.0),
        (Some(1.0), 0.0),
        (Some(2.0), 0.1),
        (Some(2.0), 0.2),
        (Some(2.0), 0.3),
        (Some(1.0), 0.1),
        (None, 0.2),
        (None, 0.3),
    ];
    /// Summary groups besides the languages.
    const ALL_GROUP: &str = "all";
    const AGREEING_GROUP: &str = "agree";
    /// A median line start further off than this means the reference follows
    /// another timeline than the audio.
    const REFERENCE_MISMATCH_SECONDS: f64 = 1.0;
    /// The languages calibration chooses on; the others check the choice.
    const DEV_LANGUAGES: [&str; 2] = ["de", "en"];
    /// How early the path places words: the median signed onset error of
    /// Whisper small on the German and English songs with `trim,clip`
    /// (−0.105 s on 2026-11-11), rounded. `shift` delays every time by it.
    const EARLY_BIAS_SECONDS: f64 = 0.1;
    /// Word probability ranges whose errors are reported apart, to calibrate
    /// a confidence gate.
    const PROBABILITY_BUCKETS: [(f32, f32); 4] =
        [(0.0, 0.05), (0.05, 0.2), (0.2, 0.5), (0.5, 1.01)];
    /// A word at or above this probability counts as confidently placed.
    const CONFIDENT_PROBABILITY: f32 = 0.2;
    /// Song gates reported: the least share of confident words a song needs.
    const GATES: [f64; 5] = [0.0, 0.5, 0.6, 0.7, 0.8];

    /// Variants of the method, named in the last argument.
    #[derive(Clone, Copy, Debug, Default)]
    struct Options {
        open_ends: bool,
        gap_padding: bool,
        trim_unlikely: bool,
        calibrate: bool,
        /// Drop repeated lines from the text, as plain lyrics that write a
        /// chorus once do (owner sets only).
        dedupe: bool,
        post: PostProcess,
    }

    impl Options {
        fn parse(list: &str) -> Self {
            let items: Vec<&str> = list.split(',').map(str::trim).collect();
            let has = |name: &str| items.contains(&name);
            let value = |name: &str, default: f64| {
                items.iter().find_map(|item| match item.split_once('=') {
                    Some((key, value)) if key == name => value.parse().ok(),
                    _ if *item == name => Some(default),
                    _ => None,
                })
            };
            Self {
                open_ends: has("open"),
                gap_padding: has("pad"),
                trim_unlikely: has("trim"),
                calibrate: has("calibrate"),
                dedupe: has("dedupe"),
                post: PostProcess {
                    word_delay: value("shift", EARLY_BIAS_SECONDS).unwrap_or(0.0),
                    clip_factor: value("clip", LINE_START_CLIP_FACTOR),
                    line_delay: value("lines", 0.0).unwrap_or(0.0),
                },
            }
        }
    }

    /// Corrections of the aligned times. They never change what the windows
    /// decide, so `calibrate` measures many of them from one alignment.
    #[derive(Clone, Copy, Debug, Default)]
    struct PostProcess {
        /// Seconds added to every time.
        word_delay: f64,
        /// A line's first word lasts at most this many typical words.
        clip_factor: Option<f64>,
        /// Seconds added to the start of each line's first word, up to its end.
        line_delay: f64,
    }

    impl PostProcess {
        fn apply(
            &self,
            raw: &[Option<Aligned>],
            lines: &[std::ops::Range<usize>],
        ) -> Vec<Option<Aligned>> {
            let mut aligned = raw.to_vec();
            if let Some(factor) = self.clip_factor {
                clip_line_starts(&mut aligned, lines, factor);
            }
            for word in aligned.iter_mut().flatten() {
                word.start += self.word_delay;
                word.end += self.word_delay;
            }
            for line in lines {
                if let Some(Some(word)) = aligned.get_mut(line.start) {
                    word.start = (word.start + self.line_delay).min(word.end);
                }
            }
            aligned
        }
    }

    /// Every word delay of `WORD_DELAYS` with every treatment of line starts
    /// of `LINE_TREATMENTS`.
    fn calibration_grid() -> Vec<PostProcess> {
        WORD_DELAYS
            .iter()
            .flat_map(|&word_delay| {
                LINE_TREATMENTS
                    .iter()
                    .map(move |&(clip_factor, line_delay)| PostProcess {
                        word_delay,
                        clip_factor,
                        line_delay,
                    })
            })
            .collect()
    }
    #[derive(Deserialize)]
    struct Config {
        d_model: usize,
        encoder_layers: usize,
        encoder_attention_heads: usize,
        decoder_layers: usize,
        decoder_attention_heads: usize,
        num_mel_bins: usize,
        max_target_positions: usize,
    }

    #[derive(Deserialize)]
    struct GenerationConfig {
        alignment_heads: Vec<[usize; 2]>,
    }

    #[derive(Deserialize)]
    struct Preprocessor {
        #[serde(default)]
        mel_filters: Vec<Vec<f64>>,
    }

    pub fn run() -> Res<()> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let [model_dir, dataset, rest @ ..] = args.as_slice() else {
            return Err("usage: <whisper-dir> <jamendolyrics-dir> [all|de,en,es,fr] [songs] [f32|f16] [strict|open,pad,trim,clip[=factor],shift[=seconds],lines=seconds,calibrate]".into());
        };
        let subset = rest.first().map_or("all", String::as_str);
        let limit: usize = rest.get(1).map_or(Ok(0), |value| value.parse())?;
        let dtype = match rest.get(2).map_or("f32", String::as_str) {
            "f16" => DType::F16,
            _ => DType::F32,
        };
        let options = Options::parse(rest.get(3).map_or("", String::as_str));
        let started = Instant::now();
        let model = Whisper::load(Path::new(model_dir), dtype)?;
        println!(
            "Model {model_dir}: {} encoder and {} decoder layers, width {}, {} mel bins, {} alignment heads, {dtype:?} on {}, {options:?}. Loaded in {:.1} s.",
            model.config.encoder_layers,
            model.config.decoder_layers,
            model.config.d_model,
            model.config.num_mel_bins,
            model.heads.len(),
            if model.device.is_metal() { "Metal" } else { "the CPU" },
            started.elapsed().as_secs_f64()
        );
        let warm_up = Instant::now();
        let prompt = [
            model.sot,
            model.token("<|en|>")?,
            model.transcribe,
            model.no_timestamps,
        ];
        let silence = vec![0.0; model.config.num_mel_bins * WINDOW_FRAMES];
        let words = vec![model.encode(" la")?; 4];
        model.align_window(
            silence,
            WINDOW_FRAMES,
            &prompt,
            &words,
            options,
            &mut Timing::default(),
        )?;
        println!(
            "First window, including pipeline compilation: {:.2} s.",
            warm_up.elapsed().as_secs_f64()
        );
        let root = Path::new(dataset);
        let mut songs = if root.join(OWNER_MANIFEST).exists() {
            load_owner_set(root, options.dedupe)?
        } else {
            load_songs(root, subset)?
        };
        if limit > 0 {
            songs.truncate(limit);
        }
        let mut results = Vec::new();
        println!("lang  words  AAE_s  median_s  PCO%  line_AAE_s  line_bias_s  referenced_lines%  gross%  unaligned  windows  RTF    song");
        for song in &songs {
            match align_song(&model, song, options) {
                Ok(result) => {
                    println!(
                        "{:<4}  {:>5}  {:>5.2}  {:>8.2}  {:>4.0}  {:>10.2}  {:>11.2}  {:>17.0}  {:>6.1}  {:>9}  {:>7}  {:.3}  {}",
                        song.language,
                        song.words.len(),
                        mean(&result.errors),
                        median(&result.errors),
                        100.0 * within(&result.errors, PCO_TOLERANCE_SECONDS),
                        mean(&result.line_errors),
                        median(&result.line_signed),
                        100.0 * finite(&result.line_errors).len() as f64
                            / result.line_errors.len().max(1) as f64,
                        100.0 * (1.0 - within(&result.errors, GROSS_ERROR_SECONDS)),
                        result.unaligned,
                        result.timing.windows,
                        result.timing.total.as_secs_f64() / result.audio_seconds,
                        song.name
                    );
                    results.push((song.language.clone(), result));
                }
                Err(error) => println!("skipped {}: {error}", song.name),
            }
        }
        summarize(&results);
        summarize_calibration(&results);
        Ok(())
    }

    struct Song {
        name: String,
        language: String,
        audio: PathBuf,
        words: Vec<String>,
        lines: Vec<std::ops::Range<usize>>,
        reference: Vec<f64>,
    }

    /// The manifest `lyrics_set_probe` writes into an owner set.
    const OWNER_MANIFEST: &str = "manifest.tsv";

    /// An owner set built by `lyrics_set_probe`. The record's plain text is what
    /// gets aligned, as the application would; each of its lines takes as
    /// reference the start of the synced line it matches, and its words the
    /// synced words when that line has word timing for the same words. Words
    /// without a reference have a NaN onset. Section labels in square brackets
    /// are not sung, so they are left out.
    fn load_owner_set(root: &Path, dedupe: bool) -> Res<Vec<Song>> {
        let mut songs = Vec::new();
        for row in fs::read_to_string(root.join(OWNER_MANIFEST))?
            .lines()
            .skip(1)
        {
            let fields: Vec<&str> = row.split('\t').collect();
            let (Some(slug), Some(language), Some(audio)) =
                (fields.first(), fields.get(3), fields.get(9))
            else {
                continue;
            };
            let read = |suffix: &str| {
                fs::read_to_string(root.join("lyrics").join(format!("{slug}{suffix}")))
            };
            let plain =
                yt_core::Lyrics::parse(&read(".plain.txt")?).ok_or("unreadable plain lyrics")?;
            let synced = yt_core::Lyrics::parse(&read(".synced.lrc")?)
                .filter(yt_core::Lyrics::is_synced)
                .ok_or("unreadable synced lyrics")?;
            let mut seen = std::collections::HashSet::new();
            let sung: Vec<&str> = plain
                .lines()
                .iter()
                .map(|line| line.text.as_str())
                .filter(|text| !text.is_empty() && !is_section_label(text))
                .filter(|text| !dedupe || seen.insert(text_key(text)))
                .collect();
            let timed: Vec<&yt_core::LyricLine> = synced
                .lines()
                .iter()
                .filter(|line| !line.text.is_empty())
                .collect();
            let matches = match_lines(&sung, &timed);
            let mut words = Vec::new();
            let mut lines = Vec::new();
            let mut reference = Vec::new();
            for (text, matched) in sung.iter().zip(matches) {
                let start = words.len();
                let line_words: Vec<&str> = text.split_whitespace().collect();
                words.extend(line_words.iter().map(|word| (*word).to_owned()));
                let matched = matched.map(|index| timed[index]);
                let word_starts = matched.filter(|line| {
                    line.words.len() == line_words.len()
                        && line
                            .words
                            .iter()
                            .zip(&line_words)
                            .all(|(timed, word)| text_key(&timed.text) == text_key(word))
                });
                match (matched, word_starts) {
                    (_, Some(line)) => reference.extend(
                        line.words
                            .iter()
                            .map(|word| f64::from(word.start_ms) / 1_000.0),
                    ),
                    (Some(line), None) => {
                        reference.push(f64::from(line.start_ms) / 1_000.0);
                        reference.extend(std::iter::repeat_n(f64::NAN, line_words.len() - 1));
                    }
                    (None, _) => reference.extend(std::iter::repeat_n(f64::NAN, line_words.len())),
                }
                if words.len() > start {
                    lines.push(start..words.len());
                }
            }
            songs.push(Song {
                name: (*slug).to_owned(),
                language: (*language).to_owned(),
                audio: root.join("audio").join(audio),
                words,
                lines,
                reference,
            });
        }
        Ok(songs)
    }

    /// A whole line in square brackets, such as «[Coro]», names a section and
    /// is not sung.
    fn is_section_label(text: &str) -> bool {
        text.starts_with('[') && text.ends_with(']')
    }

    /// Lowercase letters and digits only, for matching texts.
    fn text_key(text: &str) -> String {
        text.chars()
            .flat_map(char::to_lowercase)
            .filter(|c| c.is_alphanumeric())
            .collect()
    }

    /// For each plain line, the synced line it matches in the longest common
    /// subsequence of equal texts, so repeated lines pair up in order.
    fn match_lines(plain: &[&str], timed: &[&yt_core::LyricLine]) -> Vec<Option<usize>> {
        let a: Vec<String> = plain.iter().map(|text| text_key(text)).collect();
        let b: Vec<String> = timed.iter().map(|line| text_key(&line.text)).collect();
        let (n, m) = (a.len(), b.len());
        let mut table = vec![vec![0u32; m + 1]; n + 1];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                table[i][j] = if !a[i].is_empty() && a[i] == b[j] {
                    table[i + 1][j + 1] + 1
                } else {
                    table[i + 1][j].max(table[i][j + 1])
                };
            }
        }
        let mut matches = vec![None; n];
        let (mut i, mut j) = (0, 0);
        while i < n && j < m {
            if !a[i].is_empty() && a[i] == b[j] {
                matches[i] = Some(j);
                i += 1;
                j += 1;
            } else if table[i + 1][j] >= table[i][j + 1] {
                i += 1;
            } else {
                j += 1;
            }
        }
        matches
    }

    /// Songs of `subset` in name order, with the words of their normalized
    /// lyrics and the annotated onset of every word.
    fn load_songs(root: &Path, subset: &str) -> Res<Vec<Song>> {
        let mut songs = Vec::new();
        for language in LANGUAGES
            .iter()
            .filter(|language| subset == "all" || subset.split(',').any(|item| item == **language))
        {
            let mut names: Vec<String> =
                fs::read_dir(root.join("subsets").join(language).join("mp3"))?
                    .filter_map(|entry| entry.ok())
                    .filter_map(|entry| {
                        let path = entry.path();
                        (path.extension()? == "mp3")
                            .then(|| {
                                path.file_stem()
                                    .map(|stem| stem.to_string_lossy().into_owned())
                            })
                            .flatten()
                    })
                    .collect();
            names.sort();
            for name in names {
                let text = fs::read_to_string(root.join("lyrics").join(format!("{name}.txt")))?;
                let csv =
                    fs::read_to_string(root.join("annotations/words").join(format!("{name}.csv")))?;
                let reference: Vec<f64> = csv
                    .lines()
                    .skip(1)
                    .filter_map(|row| row.split(',').next()?.trim().parse().ok())
                    .collect();
                let mut words = Vec::new();
                let mut lines = Vec::new();
                for line in text.lines() {
                    let start = words.len();
                    words.extend(line.split_whitespace().map(str::to_owned));
                    if words.len() > start {
                        lines.push(start..words.len());
                    }
                }
                if words.len() != reference.len() {
                    println!(
                        "skipped {name}: {} words in the lyrics, {} annotated",
                        words.len(),
                        reference.len()
                    );
                    continue;
                }
                songs.push(Song {
                    audio: root
                        .join("subsets")
                        .join(language)
                        .join("mp3")
                        .join(format!("{name}.mp3")),
                    lines,
                    name,
                    language: (*language).to_owned(),
                    words,
                    reference,
                });
            }
        }
        Ok(songs)
    }

    #[derive(Default)]
    struct Timing {
        decode: Duration,
        mel: Duration,
        encoder: Duration,
        decoder: Duration,
        dtw: Duration,
        total: Duration,
        windows: usize,
        /// Every encoder block's time, in milliseconds: one GPU submission each.
        block_ms: Vec<f64>,
        slowest_decoder_ms: f64,
    }

    struct SongResult {
        errors: Vec<f64>,
        /// Predicted minus annotated onset, in seconds.
        signed: Vec<f64>,
        /// `signed` for every correction of `calibration_grid`, when asked.
        calibration: Vec<Vec<f64>>,
        /// Index of each line's first word.
        line_starts: Vec<usize>,
        line_errors: Vec<f64>,
        /// Signed onset error of each line's first word.
        line_signed: Vec<f64>,
        probabilities: Vec<f32>,
        unaligned: usize,
        audio_seconds: f64,
        valid_lyrics: bool,
        timing: Timing,
    }

    fn align_song(model: &Whisper, song: &Song, options: Options) -> Res<SongResult> {
        let started = Instant::now();
        let mut timing = Timing::default();
        let samples = decode_mono_16k(&song.audio)?;
        timing.decode = started.elapsed();
        let audio_seconds = samples.len() as f64 / f64::from(SAMPLE_RATE);
        let mel_started = Instant::now();
        let (mel, audio_frames) = log_mel(&samples, &model.filters);
        timing.mel = mel_started.elapsed();
        let language = model.token(&format!("<|{}|>", song.language))?;
        let prompt = [model.sot, language, model.transcribe, model.no_timestamps];
        let tokens: Vec<Vec<u32>> = song
            .words
            .iter()
            .map(|word| model.encode(&format!(" {word}")))
            .collect::<Res<_>>()?;
        let raw = align_words(
            model,
            &mel,
            audio_frames,
            &prompt,
            &tokens,
            options,
            &mut timing,
        )?;
        let aligned = options.post.apply(&raw, &song.lines);
        let (onsets, unaligned) = word_onsets(&aligned);
        let signed = signed_errors(&onsets, &song.reference);
        let calibration = if options.calibrate {
            calibration_grid()
                .iter()
                .map(|post| {
                    signed_errors(
                        &word_onsets(&post.apply(&raw, &song.lines)).0,
                        &song.reference,
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
        let errors: Vec<f64> = signed.iter().map(|error| error.abs()).collect();
        if std::env::var_os("YTU_ALIGN_WORDS").is_some() {
            for (index, word) in song.words.iter().enumerate() {
                let probability = aligned[index].map_or(0.0, |word| word.probability);
                eprintln!(
                    "{index:>4} {word:<16} predicted {:>7.2} reference {:>7.2} error {:>6.2} p {probability:.2}",
                    onsets[index], song.reference[index], errors[index]
                );
            }
        }
        let line_errors = song.lines.iter().map(|line| errors[line.start]).collect();
        let line_signed = song.lines.iter().map(|line| signed[line.start]).collect();
        let probabilities = aligned
            .iter()
            .map(|word| word.map_or(0.0, |word| word.probability))
            .collect();
        let lrc = enhanced_lrc(song, &aligned);
        if let (Some(dir), Some(lrc)) = (std::env::var_os("YTU_ALIGN_LRC_DIR"), &lrc) {
            fs::write(Path::new(&dir).join(format!("{}.lrc", song.name)), lrc)?;
        }
        let valid_lyrics = lrc
            .as_deref()
            .and_then(yt_core::Lyrics::parse)
            .is_some_and(|lyrics| lyrics.is_synced() && lyrics.has_word_timing());
        timing.total = started.elapsed();
        Ok(SongResult {
            errors,
            signed,
            calibration,
            line_starts: song.lines.iter().map(|line| line.start).collect(),
            line_errors,
            line_signed,
            probabilities,
            unaligned,
            audio_seconds,
            valid_lyrics,
            timing,
        })
    }

    #[derive(Clone, Copy)]
    struct Aligned {
        start: f64,
        end: f64,
        probability: f32,
    }

    struct WordTiming {
        start: f64,
        end: f64,
        /// Mean probability the decoder gives the word's tokens.
        probability: f32,
    }

    /// Greedy long-form alignment in the manner of stable-ts: each 30-second
    /// window receives the next words up to the token budget, keeps those it
    /// placed before its last placed word, and the next window starts where
    /// the last kept word ends. A first word longer than `MAX_WORD_SECONDS`
    /// means the window opened in a gap, so the window moves to just before
    /// that word ends and is aligned again.
    fn align_words(
        model: &Whisper,
        mel: &[f32],
        audio_frames: usize,
        prompt: &[u32],
        words: &[Vec<u32>],
        options: Options,
        timing: &mut Timing,
    ) -> Res<Vec<Option<Aligned>>> {
        let n_mels = model.config.num_mel_bins;
        let total_frames = mel.len() / n_mels;
        let mut aligned = vec![None; words.len()];
        let mut cursor = 0;
        let mut seek = 0;
        let mut reseeks = 0;
        while cursor < words.len() && seek < audio_frames {
            let frames = (audio_frames - seek).min(WINDOW_FRAMES);
            let mut fed = 0;
            let mut count = 0;
            while cursor + fed < words.len()
                && (fed == 0 || count + words[cursor + fed].len() <= TOKEN_BUDGET)
            {
                count += words[cursor + fed].len();
                fed += 1;
            }
            let mut window = Vec::with_capacity(n_mels * WINDOW_FRAMES);
            for row in 0..n_mels {
                let start = row * total_frames + seek;
                window.extend_from_slice(&mel[start..start + WINDOW_FRAMES]);
            }
            let placed = model.align_window(
                window,
                frames,
                prompt,
                &words[cursor..cursor + fed],
                options,
                timing,
            )?;
            let offset = seek as f64 / MEL_FRAMES_PER_SECOND;
            let window_end = frames as f64 / MEL_FRAMES_PER_SECOND;
            if std::env::var_os("YTU_ALIGN_TRACE").is_some() {
                let shown: Vec<String> = placed
                    .iter()
                    .take(6)
                    .map(|word| {
                        format!(
                            "{:.2}+{:.2}@{:.2}",
                            offset + word.start,
                            word.end - word.start,
                            word.probability
                        )
                    })
                    .collect();
                eprintln!(
                    "window at {offset:.2} s ({:.1} s): words {cursor}..{} first {}",
                    window_end,
                    cursor + fed,
                    shown.join(" ")
                );
            }
            let durations: Vec<f64> = placed
                .iter()
                .map(|word| ((word.end - word.start) * 1_000.0).round() / 1_000.0)
                .collect();
            let positive: Vec<f64> = durations.iter().copied().filter(|d| *d > 0.0).collect();
            let typical = median(&positive).min(MAX_WORD_SECONDS);
            let local_max = (typical * WORD_DURATION_FACTOR).min(MAX_WORD_SECONDS);
            let first = durations.iter().position(|duration| *duration > 0.0);
            if let Some(first) = first {
                if durations[first] > MAX_WORD_SECONDS && reseeks < MAX_RESEEKS {
                    let word = &placed[first];
                    let new_start =
                        (word.end - (typical * first as f64 + local_max)).max(word.start);
                    let step = (new_start * MEL_FRAMES_PER_SECOND).round() as usize;
                    if step > 0 {
                        seek += step;
                        reseeks += 1;
                        continue;
                    }
                }
            }
            let mut keep = durations
                .iter()
                .rposition(|duration| *duration > 0.0)
                .map_or(0, |index| index + 1);
            if let Some(long) = first.and_then(|first| {
                durations[first + 1..]
                    .iter()
                    .position(|duration| *duration > MAX_WORD_SECONDS)
                    .map(|index| first + 1 + index)
            }) {
                keep = keep.min(long);
            }
            let more_text = cursor + fed < words.len() || keep < fed;
            if more_text
                && positive.len() >= 2
                && keep > 0
                && placed[keep - 1].end >= window_end.floor()
            {
                keep = durations[..keep - 1]
                    .iter()
                    .rposition(|duration| *duration > 0.0)
                    .map_or(0, |index| index + 1);
            }
            if options.trim_unlikely {
                if let Some(run) = durations[..keep]
                    .windows(INSTANT_RUN)
                    .position(|run| run.iter().all(|duration| *duration <= INSTANT_WORD_SECONDS))
                    .filter(|run| *run > 0)
                {
                    keep = run;
                }
                while keep > 1 && placed[keep - 1].probability < LOW_PROBABILITY {
                    keep -= 1;
                }
            }
            if keep == 0 {
                seek += WINDOW_FRAMES;
                reseeks = 0;
                continue;
            }
            for (index, word) in placed[..keep].iter().enumerate() {
                aligned[cursor + index] = Some(Aligned {
                    start: offset + word.start,
                    end: offset + word.end,
                    probability: word.probability,
                });
            }
            cursor += keep;
            reseeks = 0;
            let next = ((offset + placed[keep - 1].end) * MEL_FRAMES_PER_SECOND).round() as usize;
            seek = next.max(seek + 1);
        }
        Ok(aligned)
    }

    /// A line's first word that lasts more than `factor` typical words took
    /// in the pause before the line, so it starts that long before its end,
    /// as Whisper treats words after a sentence mark (twice the typical word).
    fn clip_line_starts(
        aligned: &mut [Option<Aligned>],
        lines: &[std::ops::Range<usize>],
        factor: f64,
    ) {
        let durations: Vec<f64> = aligned
            .iter()
            .flatten()
            .map(|word| word.end - word.start)
            .filter(|duration| *duration > 0.0)
            .collect();
        let longest = factor * median(&durations).min(TYPICAL_WORD_CAP_SECONDS);
        for line in lines {
            if let Some(Some(word)) = aligned.get_mut(line.start) {
                if word.end - word.start > longest {
                    word.start = word.end - longest;
                }
            }
        }
    }

    /// Every word's onset, and how many words were not placed: those take the
    /// end of the last placed word before them.
    fn word_onsets(aligned: &[Option<Aligned>]) -> (Vec<f64>, usize) {
        let mut last = 0.0;
        let mut unaligned = 0;
        let onsets = aligned
            .iter()
            .map(|word| match word {
                Some(word) => {
                    last = word.end;
                    word.start
                }
                None => {
                    unaligned += 1;
                    last
                }
            })
            .collect();
        (onsets, unaligned)
    }

    fn signed_errors(onsets: &[f64], reference: &[f64]) -> Vec<f64> {
        onsets
            .iter()
            .zip(reference)
            .map(|(predicted, reference)| predicted - reference)
            .collect()
    }

    /// The aligned song as enhanced LRC, the format `yt_core::Lyrics` parses.
    fn enhanced_lrc(song: &Song, aligned: &[Option<Aligned>]) -> Option<String> {
        let stamp = |seconds: f64| {
            let centis = (seconds * 100.0).round() as u64;
            format!(
                "{:02}:{:02}.{:02}",
                centis / 6_000,
                centis / 100 % 60,
                centis % 100
            )
        };
        let mut text = String::new();
        for line in &song.lines {
            let words: Vec<(&String, Aligned)> = line
                .clone()
                .map(|index| Some((&song.words[index], aligned[index]?)))
                .collect::<Option<_>>()?;
            let (_, first) = words.first()?;
            let (_, last) = words.last()?;
            text.push_str(&format!("[{}]", stamp(first.start)));
            for (word, timing) in &words {
                text.push_str(&format!("<{}>{word} ", stamp(timing.start)));
            }
            text.push_str(&format!("<{}>\n", stamp(last.end)));
        }
        Some(text)
    }

    struct Proj {
        weight: Tensor,
        bias: Option<Tensor>,
    }

    impl Proj {
        fn load(vb: VarBuilder, input: usize, output: usize, bias: bool) -> Res<Self> {
            Ok(Self {
                weight: vb.get((output, input), "weight")?.t()?.contiguous()?,
                bias: bias.then(|| vb.get(output, "bias")).transpose()?,
            })
        }

        /// `x` is (rows, input); flat matrices avoid broadcasted Metal products.
        fn forward(&self, x: &Tensor) -> candle_core::Result<Tensor> {
            let y = x.matmul(&self.weight)?;
            match &self.bias {
                Some(bias) => y.broadcast_add(bias),
                None => Ok(y),
            }
        }
    }

    struct Attention {
        query: Proj,
        key: Proj,
        value: Proj,
        out: Proj,
        heads: usize,
    }

    impl Attention {
        fn load(vb: VarBuilder, width: usize, heads: usize) -> Res<Self> {
            Ok(Self {
                query: Proj::load(vb.pp("q_proj"), width, width, true)?,
                key: Proj::load(vb.pp("k_proj"), width, width, false)?,
                value: Proj::load(vb.pp("v_proj"), width, width, true)?,
                out: Proj::load(vb.pp("out_proj"), width, width, true)?,
                heads,
            })
        }

        fn split(&self, x: &Tensor) -> candle_core::Result<Tensor> {
            let (rows, width) = x.dims2()?;
            x.reshape((rows, self.heads, width / self.heads))?
                .transpose(0, 1)?
                .contiguous()
        }

        /// The attention output and, when `scores`, the pre-softmax scores
        /// (heads, queries, keys) that Whisper's word timing reads.
        fn forward(
            &self,
            x: &Tensor,
            keys: &Tensor,
            mask: Option<&Tensor>,
            scores: bool,
        ) -> candle_core::Result<(Tensor, Option<Tensor>)> {
            let (rows, width) = x.dims2()?;
            let scale = ((width / self.heads) as f64).powf(-0.25);
            let q = (self.split(&self.query.forward(x)?)? * scale)?;
            let k = (self.split(&self.key.forward(keys)?)? * scale)?;
            let v = self.split(&self.value.forward(keys)?)?;
            let mut qk = q.matmul(&k.t()?)?;
            if let Some(mask) = mask {
                qk = qk.broadcast_add(mask)?;
            }
            let y = softmax_last_dim(&qk)?
                .matmul(&v)?
                .transpose(0, 1)?
                .contiguous()?
                .reshape((rows, width))?;
            Ok((self.out.forward(&y)?, scores.then_some(qk)))
        }
    }

    fn layer_norm(vb: VarBuilder, width: usize) -> Res<LayerNorm> {
        Ok(LayerNorm::new(
            vb.get(width, "weight")?,
            vb.get(width, "bias")?,
            1e-5,
        ))
    }

    struct Block {
        attn: Attention,
        attn_ln: LayerNorm,
        cross: Option<(Attention, LayerNorm)>,
        fc1: Proj,
        fc2: Proj,
        mlp_ln: LayerNorm,
    }

    impl Block {
        fn load(vb: VarBuilder, width: usize, heads: usize, cross: bool) -> Res<Self> {
            Ok(Self {
                attn: Attention::load(vb.pp("self_attn"), width, heads)?,
                attn_ln: layer_norm(vb.pp("self_attn_layer_norm"), width)?,
                cross: if cross {
                    Some((
                        Attention::load(vb.pp("encoder_attn"), width, heads)?,
                        layer_norm(vb.pp("encoder_attn_layer_norm"), width)?,
                    ))
                } else {
                    None
                },
                fc1: Proj::load(vb.pp("fc1"), width, width * 4, true)?,
                fc2: Proj::load(vb.pp("fc2"), width * 4, width, true)?,
                mlp_ln: layer_norm(vb.pp("final_layer_norm"), width)?,
            })
        }

        fn forward(
            &self,
            x: &Tensor,
            audio: Option<&Tensor>,
            mask: Option<&Tensor>,
            scores: bool,
        ) -> candle_core::Result<(Tensor, Option<Tensor>)> {
            let normed = self.attn_ln.forward(x)?;
            let mut x = (x + self.attn.forward(&normed, &normed, mask, false)?.0)?;
            let mut qk = None;
            if let (Some((cross, ln)), Some(audio)) = (&self.cross, audio) {
                let (y, scores) = cross.forward(&ln.forward(&x)?, audio, None, scores)?;
                x = (x + y)?;
                qk = scores;
            }
            let mlp = self
                .fc2
                .forward(&self.fc1.forward(&self.mlp_ln.forward(&x)?)?.gelu_erf()?)?;
            Ok(((x + mlp)?, qk))
        }
    }

    struct Whisper {
        config: Config,
        device: Device,
        dtype: DType,
        tokenizer: Tokenizer,
        filters: Vec<(usize, Vec<f32>)>,
        heads: Vec<[usize; 2]>,
        conv1: Conv1d,
        conv2: Conv1d,
        encoder_positions: Tensor,
        encoder: Vec<Block>,
        encoder_ln: LayerNorm,
        embed: Tensor,
        decoder_positions: Tensor,
        decoder: Vec<Block>,
        decoder_ln: LayerNorm,
        sot: u32,
        eot: u32,
        transcribe: u32,
        no_timestamps: u32,
        /// Tokens of `GAP_PADDING`.
        padding: Vec<u32>,
    }

    impl Whisper {
        fn load(dir: &Path, dtype: DType) -> Res<Self> {
            let config: Config = serde_json::from_slice(&fs::read(dir.join("config.json"))?)?;
            let generation: GenerationConfig =
                serde_json::from_slice(&fs::read(dir.join("generation_config.json"))?)?;
            let tokenizer = Tokenizer::from_file(dir.join("tokenizer.json"))?;
            let device = if std::env::var_os("YTU_ALIGN_CPU").is_some() {
                Device::Cpu
            } else {
                Device::new_metal(0)?
            };
            let vb = VarBuilder::from_buffered_safetensors(
                fs::read(dir.join("model.safetensors"))?,
                dtype,
                &device,
            )?;
            let width = config.d_model;
            let conv = |name: &str, input: usize, stride: usize| -> Res<Conv1d> {
                let vb = vb.pp("model.encoder").pp(name);
                Ok(Conv1d::new(
                    vb.get((width, input, 3), "weight")?,
                    Some(vb.get(width, "bias")?),
                    Conv1dConfig {
                        padding: 1,
                        stride,
                        dilation: 1,
                        groups: 1,
                        cudnn_fwd_algo: None,
                    },
                ))
            };
            let encoder = (0..config.encoder_layers)
                .map(|i| {
                    Block::load(
                        vb.pp(format!("model.encoder.layers.{i}")),
                        width,
                        config.encoder_attention_heads,
                        false,
                    )
                })
                .collect::<Res<_>>()?;
            let decoder = (0..config.decoder_layers)
                .map(|i| {
                    Block::load(
                        vb.pp(format!("model.decoder.layers.{i}")),
                        width,
                        config.decoder_attention_heads,
                        true,
                    )
                })
                .collect::<Res<_>>()?;
            let filters = mel_filters(config.num_mel_bins);
            report_filter_difference(dir, &filters, config.num_mel_bins);
            let token = |text: &str| {
                tokenizer
                    .token_to_id(text)
                    .ok_or_else(|| format!("{text} is not in the vocabulary"))
            };
            Ok(Self {
                sot: token("<|startoftranscript|>")?,
                eot: token("<|endoftext|>")?,
                transcribe: token("<|transcribe|>")?,
                no_timestamps: token("<|notimestamps|>")?,
                padding: tokenizer.encode(GAP_PADDING, false)?.get_ids().to_vec(),
                conv1: conv("conv1", config.num_mel_bins, 1)?,
                conv2: conv("conv2", width, 2)?,
                encoder_positions: vb.get(
                    (ENCODER_FRAMES, width),
                    "model.encoder.embed_positions.weight",
                )?,
                encoder,
                encoder_ln: layer_norm(vb.pp("model.encoder.layer_norm"), width)?,
                embed: vb.get_unchecked("model.decoder.embed_tokens.weight")?,
                decoder_positions: vb.get(
                    (config.max_target_positions, width),
                    "model.decoder.embed_positions.weight",
                )?,
                decoder,
                decoder_ln: layer_norm(vb.pp("model.decoder.layer_norm"), width)?,
                heads: generation.alignment_heads,
                filters: sparse(filters),
                config,
                device,
                dtype,
                tokenizer,
            })
        }

        fn token(&self, text: &str) -> Res<u32> {
            Ok(self
                .tokenizer
                .token_to_id(text)
                .ok_or_else(|| format!("{text} is not in the vocabulary"))?)
        }

        fn encode(&self, text: &str) -> Res<Vec<u32>> {
            Ok(self.tokenizer.encode(text, false)?.get_ids().to_vec())
        }

        /// The encoder output for one window, timing each block on its own
        /// so the longest single GPU submission is known.
        fn encode_audio(&self, window: Vec<f32>, timing: &mut Timing) -> Res<Tensor> {
            let started = Instant::now();
            let mel = Tensor::from_vec(
                window,
                (1, self.config.num_mel_bins, WINDOW_FRAMES),
                &self.device,
            )?
            .to_dtype(self.dtype)?;
            let x = self.conv1.forward(&mel)?.gelu_erf()?;
            let x = self.conv2.forward(&x)?.gelu_erf()?;
            let mut x = x
                .squeeze(0)?
                .t()?
                .contiguous()?
                .broadcast_add(&self.encoder_positions)?;
            self.device.synchronize()?;
            for block in &self.encoder {
                let block_started = Instant::now();
                x = block.forward(&x, None, None, false)?.0;
                self.device.synchronize()?;
                timing
                    .block_ms
                    .push(block_started.elapsed().as_secs_f64() * 1_000.0);
            }
            let x = self.encoder_ln.forward(&x)?;
            self.device.synchronize()?;
            timing.encoder += started.elapsed();
            Ok(x)
        }

        /// Word start and end times, in seconds from the window start, of
        /// `words` forced through the decoder: Whisper's cross-attention DTW
        /// (`openai/whisper` `timing.py`) over the alignment heads.
        fn align_window(
            &self,
            window: Vec<f32>,
            frames: usize,
            prompt: &[u32],
            words: &[Vec<u32>],
            options: Options,
            timing: &mut Timing,
        ) -> Res<Vec<WordTiming>> {
            timing.windows += 1;
            let audio = self.encode_audio(window, timing)?;
            let started = Instant::now();
            let text: Vec<u32> = words.iter().flatten().copied().collect();
            let padding: &[u32] = if options.gap_padding {
                &self.padding
            } else {
                &[]
            };
            let mut tokens = prompt.to_vec();
            tokens.extend_from_slice(padding);
            tokens.extend_from_slice(&text);
            tokens.push(self.eot);
            let rows = tokens.len();
            let mut x = self
                .embed
                .index_select(&Tensor::new(tokens.as_slice(), &self.device)?, 0)?
                .broadcast_add(&self.decoder_positions.narrow(0, 0, rows)?)?;
            let mask: Vec<f32> = (0..rows)
                .flat_map(|i| (0..rows).map(move |j| if j > i { f32::NEG_INFINITY } else { 0.0 }))
                .collect();
            let mask = Tensor::from_vec(mask, (rows, rows), &self.device)?.to_dtype(self.dtype)?;
            let mut scores: Vec<Option<Tensor>> = Vec::with_capacity(self.decoder.len());
            for (layer, block) in self.decoder.iter().enumerate() {
                let wanted = self.heads.iter().any(|[l, _]| *l == layer);
                let (y, qk) = block.forward(&x, Some(&audio), Some(&mask), wanted)?;
                x = y;
                scores.push(qk);
            }
            let predicting = self
                .decoder_ln
                .forward(&x.narrow(0, prompt.len() - 1 + padding.len(), text.len())?)?
                .matmul(&self.embed.narrow(0, 0, self.eot as usize)?.t()?)?
                .to_dtype(DType::F32)?;
            let targets = Tensor::new(text.as_slice(), &self.device)?.unsqueeze(1)?;
            let probabilities = softmax_last_dim(&predicting)?
                .gather(&targets, 1)?
                .flatten_all()?
                .to_vec1::<f32>()?;
            let mut weights = Vec::with_capacity(self.heads.len());
            for [layer, head] in &self.heads {
                let qk = scores
                    .get(*layer)
                    .and_then(Option::as_ref)
                    .ok_or("alignment head outside the decoder")?;
                weights.push(
                    qk.i(*head)?
                        .to_dtype(DType::F32)?
                        .flatten_all()?
                        .to_vec1::<f32>()?,
                );
            }
            self.device.synchronize()?;
            let elapsed = started.elapsed();
            timing.decoder += elapsed;
            timing.slowest_decoder_ms = timing
                .slowest_decoder_ms
                .max(elapsed.as_secs_f64() * 1_000.0);
            let started = Instant::now();
            let matrix = alignment_matrix(&weights, rows, (frames / 2).max(1), prompt.len() - 1);
            let jumps = jump_frames(&dtw(&matrix, options.open_ends));
            let mut placed = Vec::with_capacity(words.len());
            let mut boundary = 0;
            for word in words {
                let start = jumps[padding.len() + boundary] as f64 / ENCODER_FRAMES_PER_SECOND;
                let probability = probabilities[boundary..boundary + word.len()]
                    .iter()
                    .sum::<f32>()
                    / word.len().max(1) as f32;
                boundary += word.len();
                let end = jumps[padding.len() + boundary] as f64 / ENCODER_FRAMES_PER_SECOND;
                placed.push(WordTiming {
                    start,
                    end,
                    probability,
                });
            }
            timing.dtw += started.elapsed();
            Ok(placed)
        }
    }

    /// Tokens × audio frames: per head, the softmax of the scores over the
    /// frames that hold audio, standardized over tokens, median-filtered over
    /// frames, then averaged over heads. The rows kept start at the last
    /// prompt token, which predicts the first lyric token, and end before the
    /// end-of-text token.
    fn alignment_matrix(
        heads: &[Vec<f32>],
        rows: usize,
        frames: usize,
        first_row: usize,
    ) -> Vec<Vec<f32>> {
        let kept = rows - 1 - first_row;
        let mut matrix = vec![vec![0.0f32; frames]; kept];
        for scores in heads {
            let mut weights: Vec<Vec<f32>> = (0..rows)
                .map(|row| {
                    let values = &scores[row * ENCODER_FRAMES..row * ENCODER_FRAMES + frames];
                    let peak = values.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                    let exps: Vec<f32> = values.iter().map(|v| (v - peak).exp()).collect();
                    let sum: f32 = exps.iter().sum();
                    exps.into_iter().map(|v| v / sum).collect()
                })
                .collect();
            for frame in 0..frames {
                let mean = weights.iter().map(|row| row[frame]).sum::<f32>() / rows as f32;
                let variance = weights
                    .iter()
                    .map(|row| (row[frame] - mean).powi(2))
                    .sum::<f32>()
                    / rows as f32;
                let std = variance.sqrt().max(f32::MIN_POSITIVE);
                for row in &mut weights {
                    row[frame] = (row[frame] - mean) / std;
                }
            }
            for (target, row) in matrix.iter_mut().zip(&weights[first_row..rows - 1]) {
                for (sum, value) in target.iter_mut().zip(median_filter(row)) {
                    *sum += value / heads.len() as f32;
                }
            }
        }
        matrix
    }

    /// Median over `MEDIAN_WIDTH` frames with reflected edges.
    fn median_filter(row: &[f32]) -> Vec<f32> {
        let half = MEDIAN_WIDTH / 2;
        let n = row.len() as isize;
        if row.len() <= half {
            return row.to_vec();
        }
        let at = |i: isize| {
            let reflected = if i < 0 {
                -i
            } else if i >= n {
                2 * (n - 1) - i
            } else {
                i
            };
            row[reflected.clamp(0, n - 1) as usize]
        };
        (0..n)
            .map(|i| {
                let mut window: Vec<f32> =
                    (i - half as isize..=i + half as isize).map(at).collect();
                window.sort_by(f32::total_cmp);
                window[half]
            })
            .collect()
    }

    /// Dynamic time warping over the negated matrix with the steps of
    /// `openai/whisper` `dtw_cpu`. `open_ends` lets the path start and end at
    /// any frame instead of the window's first and last, so audio before the
    /// first word and after the last one is left out. Returns the path as
    /// (row, frame) pairs in order.
    fn dtw(matrix: &[Vec<f32>], open_ends: bool) -> Vec<(usize, usize)> {
        let n = matrix.len();
        let m = matrix.first().map_or(0, Vec::len);
        let mut cost = vec![vec![f32::INFINITY; m + 1]; n + 1];
        let mut trace = vec![vec![2u8; m + 1]; n + 1];
        cost[0][0] = 0.0;
        if open_ends {
            for cell in cost[0].iter_mut() {
                *cell = 0.0;
            }
        }
        for j in 1..=m {
            for i in 1..=n {
                let (c0, c1, c2) = (cost[i - 1][j - 1], cost[i - 1][j], cost[i][j - 1]);
                // Ties prefer the diagonal, then the vertical step; `dtw_cpu`
                // sends ties to the horizontal step, which open starts turn
                // into an unreachable first row.
                let (best, step) = if c0 <= c1 && c0 <= c2 {
                    (c0, 0)
                } else if c1 <= c2 {
                    (c1, 1)
                } else {
                    (c2, 2)
                };
                cost[i][j] = -matrix[i - 1][j - 1] + best;
                trace[i][j] = step;
            }
        }
        let mut j = if open_ends {
            (1..=m)
                .min_by(|a, b| cost[n][*a].total_cmp(&cost[n][*b]))
                .unwrap_or(m)
        } else {
            m
        };
        let mut i = n;
        let mut path = Vec::with_capacity(n + m);
        while i > 0 && j > 0 {
            path.push((i - 1, j - 1));
            match trace[i][j] {
                0 => {
                    i -= 1;
                    j -= 1;
                }
                1 => i -= 1,
                _ => j -= 1,
            }
        }
        while i > 0 {
            path.push((i - 1, 0));
            i -= 1;
        }
        path.reverse();
        path
    }

    /// The first frame of every row of a DTW path.
    fn jump_frames(path: &[(usize, usize)]) -> Vec<usize> {
        let mut jumps = Vec::new();
        for &(row, frame) in path {
            if row == jumps.len() {
                jumps.push(frame);
            }
        }
        jumps
    }

    /// The Slaney-scaled mel filter bank `librosa.filters.mel` builds for
    /// Whisper (16 kHz, 400-point FFT, 0–8 kHz, area normalization).
    fn mel_filters(n_mels: usize) -> Vec<Vec<f32>> {
        const F_SP: f64 = 200.0 / 3.0;
        const MIN_LOG_HZ: f64 = 1_000.0;
        let min_log_mel = MIN_LOG_HZ / F_SP;
        let log_step = 6.4f64.ln() / 27.0;
        let hz_to_mel = |hz: f64| {
            if hz >= MIN_LOG_HZ {
                min_log_mel + (hz / MIN_LOG_HZ).ln() / log_step
            } else {
                hz / F_SP
            }
        };
        let mel_to_hz = |mel: f64| {
            if mel >= min_log_mel {
                MIN_LOG_HZ * (log_step * (mel - min_log_mel)).exp()
            } else {
                F_SP * mel
            }
        };
        let nyquist = f64::from(SAMPLE_RATE) / 2.0;
        let top = hz_to_mel(nyquist);
        let edges: Vec<f64> = (0..n_mels + 2)
            .map(|i| mel_to_hz(top * i as f64 / (n_mels + 1) as f64))
            .collect();
        (0..n_mels)
            .map(|m| {
                let norm = 2.0 / (edges[m + 2] - edges[m]);
                (0..=N_FFT / 2)
                    .map(|k| {
                        let hz = k as f64 * f64::from(SAMPLE_RATE) / N_FFT as f64;
                        let lower = (hz - edges[m]) / (edges[m + 1] - edges[m]);
                        let upper = (edges[m + 2] - hz) / (edges[m + 2] - edges[m + 1]);
                        (lower.min(upper).max(0.0) * norm) as f32
                    })
                    .collect()
            })
            .collect()
    }

    /// Prints how far the computed filters are from the ones a Hugging Face
    /// preprocessor file stores, when it stores them.
    fn report_filter_difference(dir: &Path, filters: &[Vec<f32>], n_mels: usize) {
        let stored = fs::read(dir.join("preprocessor_config.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Preprocessor>(&bytes).ok())
            .map(|preprocessor| preprocessor.mel_filters)
            .unwrap_or_default();
        let bins = N_FFT / 2 + 1;
        let difference = if stored.len() == n_mels && stored.iter().all(|row| row.len() == bins) {
            Some(
                stored
                    .iter()
                    .zip(filters)
                    .flat_map(|(a, b)| a.iter().zip(b).map(|(a, b)| (*a as f32 - b).abs()))
                    .fold(0.0, f32::max),
            )
        } else if stored.len() == bins && stored.iter().all(|row| row.len() == n_mels) {
            Some(
                (0..bins)
                    .flat_map(|k| (0..n_mels).map(move |m| (k, m)))
                    .map(|(k, m)| (stored[k][m] as f32 - filters[m][k]).abs())
                    .fold(0.0, f32::max),
            )
        } else {
            None
        };
        match difference {
            Some(difference) => {
                println!("Mel filters: largest difference from the stored bank {difference:.2e}.")
            }
            None => println!("Mel filters: the model folder stores none to compare."),
        }
    }

    /// Each filter's first nonzero bin and its weights.
    fn sparse(filters: Vec<Vec<f32>>) -> Vec<(usize, Vec<f32>)> {
        filters
            .into_iter()
            .map(|row| {
                let first = row.iter().position(|w| *w > 0.0).unwrap_or(0);
                let last = row.iter().rposition(|w| *w > 0.0).map_or(first, |i| i + 1);
                (first, row[first..last.max(first)].to_vec())
            })
            .collect()
    }

    /// Whisper's log-mel spectrogram of the whole song followed by 30 s of
    /// silence (`log_mel_spectrogram(..., padding=N_SAMPLES)`), mel-major,
    /// and the number of frames that hold audio.
    fn log_mel(samples: &[f32], filters: &[(usize, Vec<f32>)]) -> (Vec<f32>, usize) {
        let audio_frames = samples.len() / HOP;
        let mut padded = Vec::with_capacity(samples.len() + WINDOW_FRAMES * HOP + N_FFT);
        let half = N_FFT / 2;
        let len = samples.len() + WINDOW_FRAMES * HOP;
        let at = |i: usize| samples.get(i).copied().unwrap_or(0.0);
        padded.extend((1..=half).rev().map(at));
        padded.extend((0..len).map(at));
        padded.extend((len - half - 1..len - 1).rev().map(at));
        let frames = len / HOP;
        let window: Vec<f32> = (0..N_FFT)
            .map(|n| 0.5 - 0.5 * (std::f32::consts::TAU * n as f32 / N_FFT as f32).cos())
            .collect();
        let fft = FftPlanner::<f32>::new().plan_fft_forward(N_FFT);
        let mut buffer = vec![Complex::new(0.0, 0.0); N_FFT];
        let mut mel = vec![0.0f32; filters.len() * frames];
        for frame in 0..frames {
            let start = frame * HOP;
            for (slot, (sample, weight)) in buffer
                .iter_mut()
                .zip(padded[start..start + N_FFT].iter().zip(&window))
            {
                *slot = Complex::new(sample * weight, 0.0);
            }
            fft.process(&mut buffer);
            for (m, (first, weights)) in filters.iter().enumerate() {
                let energy: f32 = weights
                    .iter()
                    .zip(&buffer[*first..])
                    .map(|(w, bin)| w * bin.norm_sqr())
                    .sum();
                mel[m * frames + frame] = energy.max(1e-10).log10();
            }
        }
        let peak = mel.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        for value in &mut mel {
            *value = (value.max(peak - 8.0) + 4.0) / 4.0;
        }
        (mel, audio_frames)
    }

    fn decode_mono_16k(path: &Path) -> Res<Vec<f32>> {
        let mut source = yt_media::decode_file(path)?;
        let channels = usize::from(source.channels().max(1));
        let rate = source.sample_rate();
        let mut mono = Vec::new();
        let mut frame = Vec::with_capacity(channels);
        for sample in source.by_ref() {
            frame.push(sample);
            if frame.len() == channels {
                mono.push(frame.iter().sum::<f32>() / channels as f32);
                frame.clear();
            }
        }
        if let Some(error) = source.decoding_error() {
            return Err(error.into());
        }
        Ok(resample(&mono, rate))
    }

    /// Band-limited resampling to 16 kHz with a Blackman-windowed sinc. Output
    /// positions repeat their fraction every `SAMPLE_RATE / gcd` samples, so
    /// each distinct fraction gets its own precomputed filter.
    fn resample(input: &[f32], rate: u32) -> Vec<f32> {
        let target = u64::from(SAMPLE_RATE);
        let rate = u64::from(rate);
        if rate == target || rate == 0 {
            return input.to_vec();
        }
        let gcd = {
            let (mut a, mut b) = (rate, target);
            while b != 0 {
                (a, b) = (b, a % b);
            }
            a
        };
        let phases = (target / gcd) as usize;
        let ratio = rate as f64 / target as f64;
        let cutoff = RESAMPLE_CUTOFF * 0.5 * (1.0 / ratio).min(1.0);
        let half = (RESAMPLE_ZERO_CROSSINGS / (2.0 * cutoff)).ceil() as i64;
        let taps: Vec<Vec<f32>> = (0..phases)
            .map(|phase| {
                let fraction = phase as f64 / phases as f64;
                (-half + 1..=half)
                    .map(|k| {
                        let x = k as f64 - fraction;
                        let u = x / (half as f64 + 1.0);
                        let window = 0.42
                            + 0.5 * (std::f64::consts::PI * u).cos()
                            + 0.08 * (std::f64::consts::TAU * u).cos();
                        let arg = 2.0 * cutoff * x;
                        let sinc = if arg.abs() < 1e-12 {
                            1.0
                        } else {
                            (std::f64::consts::PI * arg).sin() / (std::f64::consts::PI * arg)
                        };
                        (2.0 * cutoff * sinc * window) as f32
                    })
                    .collect()
            })
            .collect();
        let out_len = (input.len() as u64 * target / rate) as usize;
        (0..out_len)
            .map(|n| {
                let position = n as u64 * rate;
                let base = (position / target) as i64;
                let filter = &taps[((position % target) / gcd) as usize];
                let first = base - half + 1;
                if first >= 0 && (first as usize + filter.len()) <= input.len() {
                    let start = first as usize;
                    input[start..start + filter.len()]
                        .iter()
                        .zip(filter)
                        .map(|(x, h)| x * h)
                        .sum()
                } else {
                    filter
                        .iter()
                        .enumerate()
                        .filter_map(|(i, h)| {
                            let k = first + i as i64;
                            (k >= 0 && (k as usize) < input.len()).then(|| input[k as usize] * h)
                        })
                        .sum()
                }
            })
            .collect()
    }

    struct CalibrationStats {
        aae: f64,
        pco: f64,
        median: f64,
        bias: f64,
        line_pco: f64,
        line_median: f64,
        line_bias: f64,
        gross: f64,
    }

    impl std::fmt::Display for CalibrationStats {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                f,
                "{:>5.3}  {:>4.1}  {:>8.3}  {:>6.3}  {:>9.1}  {:>13.3}  {:>11.3}  {:>6.1}",
                self.aae,
                self.pco,
                self.median,
                self.bias,
                self.line_pco,
                self.line_median,
                self.line_bias,
                self.gross
            )
        }
    }

    /// Metrics of correction `index` over the development songs, or over the
    /// others.
    fn calibration_stats(
        results: &[(String, SongResult)],
        index: usize,
        development: bool,
    ) -> CalibrationStats {
        let songs: Vec<&SongResult> = results
            .iter()
            .filter(|(language, _)| DEV_LANGUAGES.contains(&language.as_str()) == development)
            .map(|(_, song)| song)
            .collect();
        let signed: Vec<f64> = songs
            .iter()
            .flat_map(|song| song.calibration[index].iter().copied())
            .collect();
        let errors: Vec<f64> = signed.iter().map(|error| error.abs()).collect();
        let line_signed: Vec<f64> = songs
            .iter()
            .flat_map(|song| {
                song.line_starts
                    .iter()
                    .map(|&start| song.calibration[index][start])
            })
            .collect();
        let line_errors: Vec<f64> = line_signed.iter().map(|error| error.abs()).collect();
        let song_aae: Vec<f64> = songs
            .iter()
            .map(|song| {
                mean(
                    &song.calibration[index]
                        .iter()
                        .map(|error| error.abs())
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        CalibrationStats {
            aae: mean(&song_aae),
            pco: 100.0 * within(&errors, PCO_TOLERANCE_SECONDS),
            median: median(&errors),
            bias: median(&signed),
            line_pco: 100.0 * within(&line_errors, PCO_TOLERANCE_SECONDS),
            line_median: median(&line_errors),
            line_bias: median(&line_signed),
            gross: 100.0 * (1.0 - within(&errors, GROSS_ERROR_SECONDS)),
        }
    }

    /// Every correction on the development songs and on the others, and the
    /// one with the highest word PCO plus line-start PCO on the development
    /// songs.
    fn summarize_calibration(results: &[(String, SongResult)]) {
        if results.iter().any(|(_, song)| song.calibration.is_empty()) {
            return;
        }
        let header = "AAE_s  PCO%  median_s  bias_s  line_PCO%  line_median_s  line_bias_s  gross%";
        println!();
        println!("calibration: development {DEV_LANGUAGES:?}, then held out");
        println!("shift  clip  lines | {header} | {header}");
        let mut best: Option<(usize, f64)> = None;
        for (index, post) in calibration_grid().iter().enumerate() {
            let development = calibration_stats(results, index, true);
            let held_out = calibration_stats(results, index, false);
            println!(
                "{:>5.2}  {:>4}  {:>5.2} | {development} | {held_out}",
                post.word_delay,
                post.clip_factor
                    .map_or("-".to_owned(), |factor| format!("{factor:.0}x")),
                post.line_delay
            );
            let score = development.pco + development.line_pco;
            if best.is_none_or(|(_, best)| score > best) {
                best = Some((index, score));
            }
        }
        if let Some((index, _)) = best {
            let post = calibration_grid()[index];
            println!(
                "chosen on the development songs: {post:?} | {} | {}",
                calibration_stats(results, index, true),
                calibration_stats(results, index, false)
            );
        }
    }

    /// Whether the reference keeps the same timeline as the audio: the median
    /// line start is within `REFERENCE_MISMATCH_SECONDS`. A catalog record of
    /// another version, or one with a constant offset, fails it throughout.
    fn reference_agrees(song: &SongResult) -> bool {
        median(&song.line_signed).abs() <= REFERENCE_MISMATCH_SECONDS
    }

    /// Share of a song's words placed at or above `CONFIDENT_PROBABILITY`.
    fn confident_share(song: &SongResult) -> f64 {
        let confident = song
            .probabilities
            .iter()
            .filter(|probability| **probability >= CONFIDENT_PROBABILITY)
            .count();
        confident as f64 / song.probabilities.len().max(1) as f64
    }

    /// The finite values: a word without a reference has a NaN error.
    fn finite(values: &[f64]) -> Vec<f64> {
        values
            .iter()
            .copied()
            .filter(|value| value.is_finite())
            .collect()
    }

    fn mean(values: &[f64]) -> f64 {
        let values = finite(values);
        if values.is_empty() {
            return f64::NAN;
        }
        values.iter().sum::<f64>() / values.len() as f64
    }

    fn median(values: &[f64]) -> f64 {
        let mut sorted = finite(values);
        if sorted.is_empty() {
            return f64::NAN;
        }
        sorted.sort_by(f64::total_cmp);
        let middle = sorted.len() / 2;
        if sorted.len().is_multiple_of(2) {
            (sorted[middle - 1] + sorted[middle]) / 2.0
        } else {
            sorted[middle]
        }
    }

    /// The value below which `share` of `values` fall (nearest rank).
    fn percentile(values: &[f64], share: f64) -> f64 {
        let mut sorted = finite(values);
        if sorted.is_empty() {
            return 0.0;
        }
        sorted.sort_by(f64::total_cmp);
        let rank = ((share * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
        sorted[rank - 1]
    }

    fn within(errors: &[f64], tolerance: f64) -> f64 {
        let errors = finite(errors);
        if errors.is_empty() {
            return f64::NAN;
        }
        errors.iter().filter(|error| **error <= tolerance).count() as f64 / errors.len() as f64
    }

    /// Per language and overall: AAE as the mean of per-song averages (as
    /// JamendoLyrics results are reported), the pooled median and PCO, line
    /// starts, and the time split.
    fn summarize(results: &[(String, SongResult)]) {
        println!();
        println!("set   songs  AAE_s  median_s  bias_s  PCO%  line_AAE_s  gross%  valid_lrc  RTF    enc_s  dec_s  dtw_s  mel_s  decode_s  block_median_ms  block_p99_ms  block_max_ms  slowest_decoder_ms");
        let groups: Vec<&str> = LANGUAGES
            .iter()
            .copied()
            .filter(|language| results.iter().any(|(l, _)| l == language))
            .chain([ALL_GROUP, AGREEING_GROUP])
            .collect();
        for group in groups {
            let songs: Vec<&SongResult> = results
                .iter()
                .filter(|(language, song)| match group {
                    ALL_GROUP => true,
                    AGREEING_GROUP => reference_agrees(song),
                    _ => language == group,
                })
                .map(|(_, result)| result)
                .collect();
            if songs.is_empty() {
                continue;
            }
            let song_aae: Vec<f64> = songs.iter().map(|song| mean(&song.errors)).collect();
            let errors: Vec<f64> = songs
                .iter()
                .flat_map(|song| song.errors.iter().copied())
                .collect();
            let signed: Vec<f64> = songs
                .iter()
                .flat_map(|song| song.signed.iter().copied())
                .collect();
            let line_aae: Vec<f64> = songs.iter().map(|song| mean(&song.line_errors)).collect();
            let audio: f64 = songs.iter().map(|song| song.audio_seconds).sum();
            let blocks: Vec<f64> = songs
                .iter()
                .flat_map(|song| song.timing.block_ms.iter().copied())
                .collect();
            let sum = |part: fn(&Timing) -> Duration| {
                songs
                    .iter()
                    .map(|song| part(&song.timing).as_secs_f64())
                    .sum::<f64>()
            };
            println!(
                "{:<4}  {:>5}  {:>5.3}  {:>8.3}  {:>6.3}  {:>4.1}  {:>10.3}  {:>6.1}  {:>9}  {:.4} {:>6.1} {:>6.1} {:>6.1} {:>6.1} {:>9.1}  {:>15.1}  {:>12.1}  {:>12.1}  {:>18.1}",
                group,
                songs.len(),
                mean(&song_aae),
                median(&errors),
                median(&signed),
                100.0 * within(&errors, PCO_TOLERANCE_SECONDS),
                mean(&line_aae),
                100.0 * (1.0 - within(&errors, GROSS_ERROR_SECONDS)),
                songs.iter().filter(|song| song.valid_lyrics).count(),
                sum(|t| t.total) / audio,
                sum(|t| t.encoder),
                sum(|t| t.decoder),
                sum(|t| t.dtw),
                sum(|t| t.mel),
                sum(|t| t.decode),
                median(&blocks),
                percentile(&blocks, 0.99),
                blocks.iter().copied().fold(0.0, f64::max),
                songs.iter().map(|song| song.timing.slowest_decoder_ms).fold(0.0, f64::max),
            );
        }
        let agreeing: Vec<&SongResult> = results
            .iter()
            .map(|(_, song)| song)
            .filter(|song| reference_agrees(song))
            .collect();
        let offset = |song: &SongResult| median(&song.line_signed);
        let residual: Vec<f64> = agreeing
            .iter()
            .flat_map(|song| {
                song.signed
                    .iter()
                    .map(move |error| (error - offset(song)).abs())
            })
            .collect();
        let line_residual: Vec<f64> = agreeing
            .iter()
            .flat_map(|song| {
                song.line_signed
                    .iter()
                    .map(move |error| (error - offset(song)).abs())
            })
            .collect();
        println!(
            "{AGREEING_GROUP} songs with each song's median line offset removed: PCO {:.1}%, median {:.3} s, line starts within {PCO_TOLERANCE_SECONDS} s {:.1}%, line median {:.3} s, gross {:.1}%",
            100.0 * within(&residual, PCO_TOLERANCE_SECONDS),
            median(&residual),
            100.0 * within(&line_residual, PCO_TOLERANCE_SECONDS),
            median(&line_residual),
            100.0 * (1.0 - within(&residual, GROSS_ERROR_SECONDS)),
        );
        println!();
        println!("gate  songs  passing%  AAE_s  median_s  PCO%  line_AAE_s  line_median_s  line_bias_s  line_PCO%  gross%");
        for gate in GATES {
            let passing: Vec<&SongResult> = results
                .iter()
                .map(|(_, song)| song)
                .filter(|song| confident_share(song) >= gate)
                .collect();
            let song_aae: Vec<f64> = passing.iter().map(|song| mean(&song.errors)).collect();
            let errors: Vec<f64> = passing
                .iter()
                .flat_map(|song| song.errors.iter().copied())
                .collect();
            let lines: Vec<f64> = passing
                .iter()
                .flat_map(|song| song.line_errors.iter().copied())
                .collect();
            let line_aae: Vec<f64> = passing.iter().map(|song| mean(&song.line_errors)).collect();
            let line_signed: Vec<f64> = passing
                .iter()
                .flat_map(|song| song.line_signed.iter().copied())
                .collect();
            println!(
                "{gate:.1}   {:>5}  {:>8.1}  {:>5.3}  {:>8.3}  {:>4.1}  {:>10.3}  {:>13.3}  {:>11.3}  {:>9.1}  {:>6.1}",
                passing.len(),
                100.0 * passing.len() as f64 / results.len().max(1) as f64,
                mean(&song_aae),
                median(&errors),
                100.0 * within(&errors, PCO_TOLERANCE_SECONDS),
                mean(&line_aae),
                median(&lines),
                median(&line_signed),
                100.0 * within(&lines, PCO_TOLERANCE_SECONDS),
                100.0 * (1.0 - within(&errors, GROSS_ERROR_SECONDS)),
            );
        }
        println!();
        println!("word probability  share%  AAE_s  PCO%");
        let pairs: Vec<(f32, f64)> = results
            .iter()
            .flat_map(|(_, song)| {
                song.probabilities
                    .iter()
                    .copied()
                    .zip(song.errors.iter().copied())
            })
            .collect();
        for (low, high) in PROBABILITY_BUCKETS {
            let errors: Vec<f64> = pairs
                .iter()
                .filter(|(probability, _)| (low..high).contains(probability))
                .map(|(_, error)| *error)
                .collect();
            println!(
                "{low:.2}–{high:.2}        {:>6.1}  {:>5.3}  {:>4.1}",
                100.0 * errors.len() as f64 / pairs.len().max(1) as f64,
                mean(&errors),
                100.0 * within(&errors, PCO_TOLERANCE_SECONDS)
            );
        }
    }
}
