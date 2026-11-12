# Lyrics alignment

> **Status:** design proposal with a measurement probe, not wired into the application (2026-11-11).
>
> **Scope:** an optional job, run on the user's device, that turns plain lyrics (LRCLIB `plainLyrics`, or plain
> lyrics in a library file's tags) into lyrics timed line by line and word by word, so karaoke works for songs that
> only have plain text. The rules of [`AGENTS.md`](../AGENTS.md) win over this document. When a stage ships, its
> behavior moves to [`docs/systems/Lyrics.md`](systems/Lyrics.md), its status to
> [`RUST_MIGRATION.md`](RUST_MIGRATION.md), and this document is updated.
>
> **Owner's constraints:** everything in Rust; heavy work on the GPU (Metal on macOS, DirectML or the vendor's
> provider on Windows) without heating the device or hurting playback; a production application with many users and
> a future SaaS backend, so model and lyrics licenses matter.

## 1. Summary

**Proposal.** An optional job, started only when the user asks for it on one song, times the song's plain lyrics
against its audio on the device's GPU and stores the result as word-timed `yt_core::Lyrics`. The method is Whisper
forced alignment: the known text is fed to Whisper's decoder, and dynamic time warping over the cross-attention of
the alignment heads OpenAI published places every word (`openai/whisper` `timing.py`), window by window across the
song as stable-ts does, plus two window rules and three timing corrections measured and calibrated here. It runs in
Rust on candle with Metal on macOS, and on ONNX Runtime with DirectML or the vendor's provider on Windows. Whisper's
code and weights are MIT.

**Measured** on JamendoLyrics MultiLang (79 songs in German, English, Spanish and French) on the development Mac's
GPU, with every rule and constant chosen on German and English and checked on Spanish and French (§6):

| | Whisper small, calibrated | Whisper medium, calibrated | Best published system (weights not public) |
|---|---|---|---|
| Words placed within 0.3 s, German and English (development) | 92.0 % | 91.6 % | 94 % over all four languages |
| Words placed within 0.3 s, Spanish and French (held out) | 88.9 % | 91.1 % | |
| Line starts within 0.3 s, development | 88.8 % | 84.0 % | — |
| Line starts within 0.3 s, held out | 82.7 % | 80.2 % | — |
| Median word error, held out | 0.08 s | 0.06 s | — |
| Average word error (AAE), held out | 0.50 s | 0.62 s | 0.18 s over all four languages |
| Words off by more than 1 s, held out | 3.9 % | 3.9 % | — |
| Owner's 30 songs against LRCLIB's user timing: words within 0.3 s (§6.8) | 50.1 %; 74.6 % without each song's constant offset | 49.4 % | — |
| GPU time for a 4-minute song (FP16) | 3.4 s | 9.8 s | — |
| Weights to download (FP16) | 0.48 GB | 1.5 GB | — |

**Decision after the macOS half of stage B: not yet a go for the product.** On JamendoLyrics, whose onsets were
marked by hand, calibrated Whisper small meets every shipping target of §12 but one, which it misses narrowly
(line-start AAE 0.55 s among songs that pass the gate, against 0.5 s): 92 % of their words within 0.3 s, 2.2 % gross
errors, three songs in four passing the gate, 3.4 s of GPU per song, GPU chunks of 16 ms and no thermal warning.
Medium is not worth three times the GPU and the download. On the owner's 30 songs, measured against LRCLIB's
user-timed lyrics, only half the words fall within 0.3 s, three in four once each song's constant offset is removed
(§6.8). That reference cannot say whether the rest of the gap is the alignment's or LRCLIB's, so the next step is
cheap and decisive: the owner listens to five of those songs with the aligned karaoke (§12), and then the Windows
half runs. The engine and the interface are built only if both meet the targets.

## 2. Starting point in the code

| Piece | Today | Role in alignment |
|---|---|---|
| `yt_core::Lyrics` (`crates/yt-core/src/lyrics.rs`) | Validated synced or plain lines; synced lines may carry `LyricWord`s that start at or after the line, ascend, and spell the line; `end_ms` marks the last word's end | The output type, unchanged. A new constructor checks an alignment against the plain lyrics it came from (§3.3) |
| Karaoke timing (`yt_lyrics::line_segments`, `LyricFill`) | One fill segment per timed word, or one across the line | Plays aligned lyrics with no change; word timing is what makes the fill follow the singing |
| Lookup order (`crates/yt-app/src/lyrics.rs`) | Synced file lyrics, the cached catalog lookup, LRCLIB (with word timing from its Lyricsfile documents when a record has one), then plain lyrics | Aligned lyrics slot in before plain lyrics (§9) |
| Decoder (`yt_media::decode_file`) | Every supported container and codec, no subprocess | Feeds the job's audio |
| GPU inference (`yt-media` `metal-stems`, `vendor/candle-roformer-metal`) | candle 0.11.1 with Metal runs the BS-RoFormer separator | Proves Rust inference on Metal; the probe reuses the same candle version |
| Heavy-job architecture (`docs/AI_DJ.md` §4.5, §5.8–5.9) | Designed, not built: `ytu-worker` process, `yt-infer` runtime, power and thermal governor | The alignment job is one more job type of that worker (§7, §8) |

Plain lyrics today render as a static list: there is no line timing at all, so "spread the fill across the line" is
not available to them. The cheapest honest baseline for a plain song is therefore no karaoke.

## 3. Inputs and outputs

### 3.1 Input

`AlignmentRequest`, built and validated in Rust:

| Field | Meaning | Bounds |
|---|---|---|
| `media` | The library track id or `youtube:<id>`, as `LyricsQuery` users name media | Same validation as the lyrics cache key |
| `content_key` | Identity of the exact audio: the PCM hash of a library file (`docs/FINGERPRINT.md`), or video id, itag and size for a YouTube stream | Results for other audio never apply |
| `audio` | A read-only path the application already confined: the library file, or the complete stream cache of the media that plays | Never a new download; never a path from the WebView |
| `lyrics` | The plain `Lyrics` shown to the user, already validated | ≤ 1 000 lines of ≤ 300 characters (`MAX_LYRICS_LINES`, `MAX_LINE_CHARS`) |
| `language` | ISO 639-1 code from the user's choice or a language detection pass | One of the model's languages |
| `duration` | Seconds of decoded audio | ≤ `MAX_ALIGN_SECONDS` (proposed 15 minutes) |

**Sung text.** A pure function in `yt-lyrics` decides which lines are sung: whole-line section labels in square
brackets (`[Coro]`, `[Verse 2: …]`) are not, everything else is, including parenthesized backing vocals. Words are
the whitespace-separated pieces of a line. Each word is tokenized on its own (`" " + word`), so every token belongs
to exactly one word.

### 3.2 Output

A synced `yt_core::Lyrics` and an `AlignmentReport`:

- every sung line keeps its exact text, gets `start_ms` from its first word, one `LyricWord` per word (the word plus
  the space after it) and `end_ms` from its last word's end;
- section labels keep their text and take the start of the next sung line, with no words, so they show and never
  fill;
- empty lines between stanzas stay as breaks, timed at the end of the stanza before them;
- the report names the model and its version, the audio content key, the SHA-256 of the plain text, the share of
  words placed with confidence, the per-line confidence, and the time the job took.

### 3.3 Validation against the input

Whatever produced the timing (the local worker today, a server tomorrow) is untrusted. A new `yt-core` constructor,
`Lyrics::aligned(plain, timing)`, accepts the timing only when:

1. the result spells exactly the plain lines, in order, with only the section-label rule of §3.1 applied;
2. it satisfies every existing `Lyrics` invariant (time order, words ascending and spelling their line, bounds);
3. every time lies within the audio's duration;
4. the confidence gate (§5.4) passed.

Otherwise the job fails, the plain lyrics stay, and nothing is stored.

## 4. Model choice and licenses

| Candidate | License | Languages | Evidence on singing | Rust path | Verdict |
|---|---|---|---|---|---|
| **Whisper** (OpenAI), cross-attention DTW forced alignment | Code and weights MIT | 99, including Spanish | Transcribes lyrics over background music without fine-tuning (Jam-ALT WER 27.9–37.8 % for large-v2 and large-v3); alignment measured here (§6) | The probe implements the model with cross-attention capture, the mel front end, DTW and windowing in Rust on candle with Metal | **Chosen** |
| Qwen3-ForcedAligner-0.6B (Alibaba) | Apache-2.0 | 11, including Spanish | Speech only on its card (32–43 ms accumulated average shift on speech, against 101–133 ms for NeMo and WhisperX); the Qwen3-ASR models claim singing, the aligner does not | Port of its audio encoder plus Qwen3; 0.9 B parameters in BF16, at most 5 minutes | Second candidate, to measure if Whisper misses the targets |
| NVIDIA Parakeet / FastConformer CTC | CC-BY-4.0 on Hugging Face and NGC, while NVIDIA's own card names its open model license: to verify | English; Spanish in separate or multilingual models | Speech only | ONNX export; CTC Viterbi alignment is simple | Not chosen: speech-trained, licensing unclear |
| MMS_FA (Meta, torchaudio) | **CC-BY-NC-4.0** | 1 100+ | Used for lyrics with separated vocals in research | ONNX export | **Excluded:** non-commercial |
| Durand, Stoller & Ewert 2023 (Spotify) | No public weights found | EN, ES, DE, FR | Best published: 0.15 s AAE in English, 0.18 s on all four languages, 0.13 s in Spanish, 1.2 M parameters (4.8 MB) | — | Unavailable; the reference to beat |
| whisper.cpp DTW timestamps (`whisper-rs`) | MIT | As Whisper | Experimental; times what Whisper transcribes, it cannot force known text | C++ through bindings | Not chosen: wrong task, not Rust |

Notes:

- **Whisper's training data** was never published. The MIT license covers the code and the weights; it says nothing
  about the data, the same situation as every other open speech model in the table.
- **Vocal separation first** is not part of the method. On Jam-ALT, HTDemucs vocals made Whisper large-v3 transcribe
  worse (WER 35.5 → 48.0), and a 2025 study found little effect on Jam-ALT. It also needs the Full edition's
  separator, whose own weights have unclear rights (`docs/AI_DJ.md` §7.4). Whether separation helps *alignment* is
  in the benchmark plan (§12) as a measurement, not an assumption.
- **Model size.** Measured on this Mac (§6.3, §6.5): once calibrated, small and medium align equally well and small
  costs a third as much; large-v3 and turbo align worse than small even though they transcribe better; tiny fails
  too often.

## 5. Method

### 5.1 Audio front end

The song is decoded with `yt_media::decode_file`, mixed to mono and resampled to 16 kHz with a band-limited
windowed-sinc resampler (one precomputed filter per output phase). The log-mel spectrogram is Whisper's exactly:
400-point FFT with a periodic Hann window and reflected edges, 160-sample hop, the Slaney mel bank that
`librosa.filters.mel` builds (80 bands, or 128 for large-v3 and turbo), `log10` clamped to 8 decades below the
song's peak, then `(x + 4) / 4`. The whole song is analyzed once, followed by 30 s of silence, as Whisper's long-form
transcription does. The probe's filter bank matches the one Hugging Face stores for Whisper to 1.9 × 10⁻⁹, and 78 of
the 79 songs it aligned parsed as valid enhanced LRC through `yt_core::Lyrics::parse` (§6.4).

### 5.2 One window

For a 30-second window and the next words of the lyrics:

1. The encoder runs once (1 500 frames of 20 ms).
2. The decoder runs once, teacher-forced with `<|startoftranscript|> <|language|> <|transcribe|>
   <|notimestamps|>`, the words' tokens and `<|endoftext|>`: no sampling, so it cannot invent text.
3. From the decoder's alignment heads (the `alignment_heads` OpenAI published per model), the cross-attention scores
   of every token over the frames that hold audio are softmaxed over frames, standardized over tokens, median-filtered
   over 7 frames and averaged over heads (`openai/whisper` `timing.py`).
4. Dynamic time warping over the negated matrix finds the monotonic token-to-frame path. A word starts at the frame
   where the path enters the row that predicts its first token, and ends where the next word starts.
5. The same decoder pass gives each token's probability; a word's probability is the mean of its tokens'.

### 5.3 The whole song

Windows advance greedily, as stable-ts aligns long audio: each window receives the next words up to 100 tokens,
keeps the words it placed before its last placed word (never a word that touches the window's end while text
remains), and the next window starts where the last kept word ends. A first word longer than 3 s means the window
opened in an instrumental gap, so the window moves to just before that word's end and runs again (at most four
times); a later word longer than 3 s ends what the window keeps.

The measurements (§6) added two window rules and two timing corrections, chosen on the German and English songs and
checked on the Spanish and French ones:

- **Lost path.** Three or more consecutive words of one frame or less mean the path lost the singing (it happens on
  repeated choruses): the window keeps only the words before them.
- **Not sung yet.** Trailing words whose probability is below 0.05 are given back to the next window.
- **Line starts.** The first word of a line that lasts more than one typical word (the median word, capped at 0.7 s
  as Whisper caps it) took in the pause before the line, so it starts one typical word before its end, and then
  0.1 s later. Whisper's own rule after a sentence mark allows two typical words; one, plus the delay, is what the
  development songs chose, and it doubles the line starts within 0.3 s (§6.3).
- **Word delay.** Every time moves later by a constant of the model: 0.15 s for small, none for medium.

The constants depend on the model, so they belong in its manifest, next to its SHA-256 (§10). §6.2 and §6.3 measure
each rule.

### 5.4 Confidence gate

Word probability predicts error strongly (§6.4). The job reports per-line confidence (mean word probability) and the
share of words above `ALIGN_CONFIDENT_PROBABILITY`. A song passes the gate when at least `ALIGN_MIN_CONFIDENT_SHARE`
of its words are confident and no line has all its words unplaced; otherwise the job reports that the lyrics do not
seem to match the recording, and nothing is stored. Lines below the line threshold keep line timing and drop word
timing, so a doubtful line lights up without a misleading word fill. The thresholds are named constants in
`yt-lyrics`, calibrated on the benchmark (§12) and on the owner's songs.

### 5.5 Where the code lives

| Layer | Crate | What |
|---|---|---|
| Domain | `yt-core` | `Lyrics::aligned` (validation against the plain lyrics), `LyricsSource::Aligned`, the stored results table and its erasure (§9) |
| Pure engine | `yt-lyrics` (new `alignment` module) | Sung-text rules, attention matrix normalization, median filter, DTW, the window planner as a state machine (`next_window()` / `accept(result)`), the keep rules, line post-processing, the confidence gate and assembly into timing. No model, file or clock: tested with synthetic matrices |
| Media and services | `yt-infer` (planned in `docs/AI_DJ.md` §5.2), or `yt-media` behind a feature until it exists | Whisper encoder and decoder with cross-attention capture, the mel front end and resampler, model manifests and SHA-256 checks; candle with Metal on macOS, ONNX Runtime on Windows |
| Worker | `ytu-worker` (`docs/AI_DJ.md` §5.8) | The `AlignLyrics` job: chunked GPU work under the power governor, cancellation, progress |
| Application | `yt-app` lyrics controller | Starts the job only on the user's action, follows its generation token, stores and shows the result |

The probe (`crates/yt-media/examples/lyrics_align_probe.rs`, feature `metal-align`) holds all of this in one file
for measurement only. Its model code, DTW and windowing are the seeds of the modules above, not a module to ship.

## 6. Measurements

### 6.1 Setup

- **Machine:** the development Mac, Apple M4 Pro (14 CPU cores, 20 GPU cores), 24 GB, macOS 26.2, on AC power.
- **Code:** `crates/yt-media/examples/lyrics_align_probe.rs`, release build, candle 0.11.1 with Metal.
- **Data:** JamendoLyrics MultiLang, all 79 songs (4 h 43 min of audio, 3 min 35 s per song on average), with the
  dataset's normalized lyrics.
- **Models:** OpenAI's Hugging Face snapshots of Whisper tiny, small, medium, large-v3 and large-v3-turbo, with
  their published alignment heads; SHA-256 checked against the files' published hashes.
- **Metrics:** as in §12. AAE is the mean over songs of each song's mean absolute onset error, the way JamendoLyrics
  results are published; it is dominated by the few songs where alignment fails, which is why the median, PCO and
  the gross-error share are reported next to it.

### 6.2 Window rules (German and English, 40 songs, Whisper small, FP32)

| Variant | AAE (s) | Median (s) | Bias (s) | PCO 0.3 s | Line-start AAE (s) | Words off by > 1 s |
|---|---|---|---|---|---|---|
| OpenAI DTW per window, stable-ts windows (`strict`) | 1.431 | 0.114 | −0.108 | 78.2 % | 2.271 | 8.1 % |
| + lost path, not sung yet (`trim`) | 0.590 | 0.112 | −0.106 | 79.9 % | 1.389 | 6.2 % |
| + Whisper's two-word clip of line starts (`trim,clip`) | 0.543 | 0.112 | −0.105 | 80.4 % | 1.104 | 3.3 % |
| + stable-ts gap padding « ...» (`pad,trim,clip`) | 0.694 | 0.113 | −0.102 | 80.8 % | 0.994 | 3.9 % |
| + 0.1 s delay (`trim,clip,shift`) | 0.496 | 0.074 | −0.005 | 84.5 % | 1.018 | 2.8 % |

- "Lost path" and "not sung yet" are the only changes to the windowing. Gap padding improved German and broke one
  English song, so it was left out.
- **Open-ended DTW** (the path may start and end at any frame) gave paths identical to the strict ones on the three
  songs tried: the row that predicts the first token always attends to the window's first frames. Not pursued.
- **Where errors come from** (window traces): words fed before an instrumental break are squeezed in before it,
  with probabilities near 0 (fixed by "not sung yet"); repeated choruses make the path collapse dozens of words
  into one frame (fixed by "lost path"); the first word of a line starts during the pause before it. That last one
  left line starts early by a median 0.32 s on all songs, which §6.3 corrects.
- These runs shared the GPU three at a time, so their speed is not reported; §6.6 is measured alone.

### 6.3 Calibration of the timing corrections

The corrections move times after the alignment and never change what the windows decide, so the probe's
`calibrate` measures all 36 combinations (4 word delays × 9 line-start treatments) from one alignment per model, on
all 79 songs, in FP16, with `trim`. The combination with the highest word PCO plus line-start PCO on German and
English is chosen; Spanish and French check it. Selected rows (word / line-start share within 0.3 s):

| Model | Word delay | Line starts | Development | Held out | Held-out line-start median | Held-out words off by > 1 s |
|---|---|---|---|---|---|---|
| small | 0 | As aligned | 79.9 % / 32.0 % | 76.5 % / 27.7 % | 0.547 s | 6.8 % |
| small | 0.10 s | Two-word clip (§6.2) | 84.5 % / 46.1 % | 82.0 % / 42.8 % | 0.361 s | 4.0 % |
| small | 0.10 s | Two-word clip, then +0.3 s | 88.5 % / 70.6 % | 85.9 % / 68.6 % | 0.200 s | 3.8 % |
| small | 0.10 s | One-word clip | 90.7 % / 84.6 % | 87.2 % / 76.6 % | 0.151 s | 3.8 % |
| **small** | **0.15 s** | **One-word clip, then +0.1 s (chosen)** | **92.0 % / 88.8 %** | **88.9 % / 82.7 %** | **0.128 s** | **3.9 %** |
| medium | 0 | As aligned | 88.0 % / 61.6 % | 88.6 % / 64.0 % | 0.202 s | 4.7 % |
| medium | 0.10 s | Two-word clip (small's earlier choice) | 84.9 % / 69.5 % | 88.6 % / 74.5 % | 0.130 s | 3.8 % |
| medium | 0 | One-word clip | 91.4 % / 82.8 % | 91.1 % / 80.2 % | 0.127 s | 3.8 % |
| **medium** | **0** | **One-word clip, then +0.1 s (chosen)** | **91.6 % / 84.0 %** | **91.1 % / 80.2 %** | **0.115 s** | **3.9 %** |

- **Clipping a line's first word at one typical word** instead of Whisper's two is what fixes line starts: for small
  it lifts the held-out line starts within 0.3 s from 43 % to 77 %, and the extra 0.1 s brings them to 83 %.
- **The word delay belongs to the model:** small places words about 0.1 s early, medium does not.
- **After calibration small and medium are within three points** of each other on every measure, so small's
  three-times-lower cost wins (§6.6).

### 6.4 All songs, calibrated small

Whisper small, FP16, `trim,clip=1,shift=0.15,lines=0.1`, all 79 songs:

| Set | Songs | AAE (s) | Median (s) | Bias (s) | PCO 0.3 s | Line-start AAE (s) | Words off by > 1 s |
|---|---|---|---|---|---|---|---|
| German (development) | 20 | 0.397 | 0.083 | +0.054 | 93.8 % | 0.669 | 1.7 % |
| English (development) | 20 | 0.522 | 0.105 | +0.075 | 90.3 % | 0.825 | 3.2 % |
| Spanish (held out) | 20 | 0.574 | 0.085 | +0.032 | 90.3 % | 0.927 | 2.8 % |
| French (held out) | 19 | 0.420 | 0.078 | +0.041 | 87.5 % | 0.697 | 5.0 % |
| **All** | **79** | **0.479** | **0.088** | **+0.050** | **90.4 %** | **0.780** | **3.2 %** |
| Spanish and French with plain OpenAI DTW and stable-ts windows (`strict`, FP32) | 39 | 1.887 | 0.143 | −0.135 | 76.0 % | 2.801 | 7.6 % |

78 of the 79 aligned songs parsed as valid enhanced LRC through `yt_core::Lyrics::parse`; the other one ended with
words the last window could not place, which the job would time by line instead. FP16 gives the same alignments as
FP32: the FP16 rows of §6.3 reproduce the FP32 rows of §6.2 exactly.

**Confidence gate.** A word is *confident* when its probability is at least 0.2.

| Gate: confident words in the song | Songs passing | AAE (s) | Median (s) | PCO 0.3 s | Line-start AAE (s) | Line-start median (s) | Line starts within 0.3 s | Words off by > 1 s |
|---|---|---|---|---|---|---|---|---|
| None | 79 (100 %) | 0.479 | 0.088 | 90.4 % | 0.780 | 0.123 | 85.8 % | 3.2 % |
| ≥ 60 % | 77 (97.5 %) | 0.451 | 0.087 | 90.6 % | 0.744 | 0.122 | 85.9 % | 3.1 % |
| ≥ 80 % | 59 (74.7 %) | 0.280 | 0.086 | 92.3 % | 0.549 | 0.123 | 87.4 % | 2.2 % |

| Word probability | Share of words | AAE (s) | PCO 0.3 s |
|---|---|---|---|
| < 0.05 | 8.4 % | 1.813 | 72.7 % |
| 0.05–0.2 | 6.2 % | 0.758 | 83.3 % |
| 0.2–0.5 | 10.5 % | 0.428 | 86.1 % |
| ≥ 0.5 | 74.9 % | 0.154 | 93.6 % |

- **The gate works:** the decoder's own probability separates good words (0.15 s average error) from bad ones
  (1.8 s), and a song-level gate at 80 % keeps three songs in four while bringing the average error from 0.48 s to
  0.28 s.
- **The one target missed** is the line-start AAE among songs that pass (0.55 s against 0.5 s), by its tail: the
  median line start is 0.12 s off and 87 % are within 0.3 s.

### 6.5 Models compared (Spanish, 20 songs, before calibration)

Same rules and small's earlier corrections (Whisper's two-word clip and a 0.1 s delay, which makes medium 0.11 s
late):

| Model | Parameters | FP16 weights | AAE (s) | Median (s) | Bias (s) | PCO 0.3 s | Line-start AAE (s) | Words off by > 1 s |
|---|---|---|---|---|---|---|---|---|
| tiny (FP32) | 39 M | 0.08 GB | 3.968 | 0.142 | +0.102 | 80.3 % | 4.380 | 10.8 % |
| **small (FP32)** | 244 M | 0.48 GB | 0.621 | 0.080 | −0.038 | 82.1 % | 1.168 | 3.2 % |
| **medium (FP16)** | 769 M | 1.5 GB | 0.845 | 0.136 | +0.111 | **89.0 %** | 1.242 | **2.7 %** |
| large-v3 (FP16) | 1 550 M | 3.1 GB | 1.896 | | | 69.9 % | 2.458 | 7.2 % |
| large-v3-turbo (FP16) | 809 M | 1.6 GB | 5.817 | 0.086 | −0.050 | 75.3 % | 6.509 | 11.4 % |

- **Medium** placed the most words and had far better line starts before calibration (median 0.125 s off, 74 %
  within 0.3 s on Spanish, against 0.35 s and 45 % for small over all songs). Its average is pulled up by two songs
  that failed for small too («Fantasma», «Intentando destacar»). Calibrated (§6.3), small catches up.
- **Bigger is not better here.** large-v3 and turbo transcribe better than small, but their published alignment heads
  give a worse path on singing; turbo, whose decoder has 4 layers instead of 32, fails most.
- **Tiny** is fast but fails too often (11 % gross errors; only 8 of 79 songs pass the 80 % gate).
- large-v3 stopped after the Spanish songs, so its median and bias were not printed.

### 6.6 Speed, memory and heat

Each model alone on the GPU, on the first three Spanish songs (10 min 21 s of audio), `trim,clip,shift`. GPU time
is encoder plus decoder; the job's total adds decoding, the spectrogram and DTW on the CPU.

| Model | GPU s for a 4-minute song | Job total for a 4-minute song | Encoder block median / p99 / max (ms) | First window (s) | Probe's peak memory |
|---|---|---|---|---|---|
| tiny, FP32 | 0.9 | 1.9 | 7.2 / 12.1 / 17.3 | 0.08 | 0.8 GB |
| small, FP32 | 4.2 | 5.5 | 16.5 / 18.3 / 21.3 | 0.26 | 3.4 GB |
| **small, FP16** | **3.4** | **4.7** | 12.2 / 15.8 / 16.4 | 0.36 | 3.1 GB |
| **medium, FP16** | **9.8** | **10.7** | 19.6 / 20.4 / 21.5 | 0.58 | 8.0 GB |
| large-v3-turbo, FP16 | 16.3 | 17.3 | 24.7 / 25.5 / 27.5 | 0.92 | 5.1 GB |
| large-v3, FP16 | 18.4 | 19.7 | 24.9 / 25.7 / 31.4 | 1.08 | 9.6 GB |
| small, FP32 on the CPU (one song) | — | 15.9 | 48.3 / 61.7 / 62.3 | 0.73 | 2.4 GB |

- **FP16 holds parity** for small on these songs (identical errors) and saves 20 % of the GPU time.
- **Every encoder block stays under the 50 ms chunk** of `docs/AI_DJ.md` §5.9 (31 ms at most, large-v3). A whole
  decoder pass took up to 220 ms (large-v3), so the job also synchronizes after each decoder layer.
- **Turbo is not cheaper here:** it has large-v3's encoder, and its failed windows are re-run.
- **Memory:** the probe reads the whole FP32 or FP16 file into memory before uploading it, which roughly doubles its
  peak. The job maps an FP16 file instead, so it needs about the weights (0.48 GB for small, 1.5 GB for medium) plus a
  few hundred MB of activations (the encoder's attention scores for one block are 1 500 × 1 500 per head).
- **Heat:** `pmset -g therm` recorded no thermal or performance warning before or after any run, across about an hour
  of back-to-back benchmarks at full speed. The job itself would run under the governor's duty cycle.
- **The CPU is no fallback:** small on the CPU took three times as long as on Metal and kept about eight of the 14
  cores busy (105 s of CPU time in 12 s).

### 6.7 Against published results

| System | AAE all four languages (s) | AAE Spanish (s) | PCO 0.3 s | Weights |
|---|---|---|---|---|
| Durand et al. 2023, language-conditioned, trained on all four languages | 0.18 | 0.13 | 94 % | Not public |
| Durand et al. 2023, trained on English only | 0.35 | 0.37 | 89 % | Not public |
| Gupta, Yılmaz & Li 2020 (as reported by Durand et al.) | 0.65 | 0.97 | 73 % | — |
| This probe: Whisper small, no training, before calibration (`trim,clip,shift`, FP32) | 0.52 | 0.62 | 83 % | MIT |
| **This probe: Whisper small, no training, calibrated (`trim,clip=1,shift=0.15,lines=0.1`, FP16)** | **0.48** | **0.57** | **90 %** | **MIT** |

The published numbers come from the 80-song release of the same dataset (the current one has 79), so they compare
closely but not exactly. Calibrated Whisper forced alignment comes within four points of the best published system
on words within 0.3 s, without any training on singing; its average error stays higher because of a tail of failed
windows, which the confidence gate refuses (§6.4).

### 6.8 The owner's songs (stage B, macOS)

**Set.** 30 songs of the owner's genres (corridos, regional Mexican, reggaeton, cumbia), proposed by the measuring
session and approved by the owner, 1 h 43 min in all. `crates/yt-media/examples/lyrics_set_probe.rs` took for each
the LRCLIB record with both synced and plain lyrics (24 with word timing from Lyricsfile documents) and the YouTube
audio whose title names the song and whose duration matches the record within 2 s, one request at a time. Audio,
lyrics and results stay outside the repository.

**Reference.** LRCLIB's synced lyrics, timed by its users for display. The record's plain text is what gets aligned,
as the application would; each plain line takes the start of the synced line it matches (longest common subsequence
of equal texts), and its words take the synced words' times where the line has word timing for the same words. Two
records follow another timeline throughout (median line start off by more than 1 s: «Baila esta cumbia», 19.5 s,
another recording; «17 años», 1.1 s), both from unofficial uploads; *agreeing* means the other 28.

| Run (small FP16 unless noted, JamendoLyrics' calibrated corrections) | Songs | Words within 0.3 s | Median | Bias | Line starts within 0.3 s | Words off by > 1 s |
|---|---|---|---|---|---|---|
| All songs | 30 | 50.1 % | 0.30 s | +0.19 s | 45.7 % | 10.9 % |
| Agreeing songs | 28 | 53.8 % | 0.27 s | +0.16 s | | 7.5 % |
| Agreeing songs, each song's constant offset removed | 28 | 74.6 % | 0.17 s | — | 74.7 % | 4.8 % |
| *JamendoLyrics Spanish and French, the same way, for comparison* | 39 | 86.9 % | 0.09 s | — | 84.1 % | 3.6 % |
| Medium (FP16), all songs | 30 | 49.4 % | 0.30 s | +0.19 s | 45.3 % | 11.9 % |
| Repeated lines dropped from the text (`dedupe`), all songs | 30 | 48.0 % | 0.32 s | +0.19 s | 45.4 % | 13.4 % |

- **Against LRCLIB, agreement is far below JamendoLyrics** (half the words within 0.3 s instead of nine in ten).
- **Most of the gap is a constant offset per song** (0.29 s on average, either sign): removing it lifts agreement
  to three in four. The offset belongs to the record (users time lines for display, often a little before the
  voice, and sometimes against another release) or to the upload (another master, another intro); this set cannot
  tell which. Alignment follows the audio the user plays, so a version offset is what it avoids; catalog lyrics
  carry it, which is why the lyrics view has a manual offset.
- **What remains after the offset** (75 % against 87 % on JamendoLyrics) is the owner's genres being harder (dense,
  fast, ad-libs, backing vocals) or LRCLIB's lines being noisier within a song; only hand-marked onsets settle it.
- **Medium does not help** (49 % against 50 %): the limit here is not the model's size.
- **Plain text without repeated choruses** costs two points and makes the 80 % gate refuse more songs (23 pass instead
  of 28), as it should.
- **Display convention.** LRCLIB's line starts lead the voice: with the corrections calibrated on true onsets our
  line starts are 0.23 s later than LRCLIB's, and with none of them they are level with it (sweep on this set,
  reported, not used for choosing: the best combinations reach 52 % of words and 50 % of line starts). Whether aligned
  lyrics should lead the voice like catalog lyrics is a product choice for the owner; a lead of about 0.2 s is the
  catalog's habit.
- Small ran at 0.021 of real time on this set, and `pmset -g therm` recorded no warning.

## 7. GPU first, without heating the device

| Platform | Runtime | Status |
|---|---|---|
| macOS, Apple Silicon | candle 0.11.1 with Metal, the version the Metal Full edition already ships | Measured here (§6) |
| Windows x64 | ONNX Runtime through Windows ML (DirectML on any DX12 GPU, or the vendor's provider), as `docs/AI_DJ.md` §4.5 decides | To measure on the Windows reference PC. Needs an ONNX export whose decoder also returns the alignment heads' cross-attention; community exports for transformers.js show the pattern, ours is produced by a reproducible export step and checked for parity against the Metal path |
| macOS Intel, machines without a usable GPU, software adapters | — | Not offered. No CPU fallback: on the CPU the same job takes three times as long and keeps about half the cores busy (§6.6), the heat the owner rules out |
| Server (later) | ONNX Runtime with CUDA or TensorRT, or candle with CUDA | Same model artifacts |

The export step, like the stems export (`crates/yt-media/models/README.md`), is model preparation done once with the
reference implementation; the shipped application loads the weights from Rust only.

Rules, inherited from the governor of `docs/AI_DJ.md` §5.9:

- **Chunks.** The job synchronizes the GPU after every encoder block and every decoder layer and checks cancellation
  there. Measured encoder blocks take 12–31 ms (§6.6), under the 50 ms chunk.
- **Duty cycle.** On AC power the GPU is busy at most 50 % of the time, on battery at most 25 %; in Low Power Mode,
  under memory pressure, with battery below 20 % or with a serious or critical thermal state the job does not start,
  or pauses. So the wall-clock time of a job is at least twice its GPU time.
- **One GPU job at a time** in the worker. An imminent DJ transition deadline wins over an alignment; an alignment the
  user is waiting for wins over background analysis.
- **Memory.** Weights load when a job starts and unload when the worker idles (3 minutes). Every tier of
  `docs/AI_DJ.md` §4.5 that gets the feature (G1, G2 and G3: Apple silicon, dedicated GPUs, integrated GPUs through
  DirectML) uses small in FP16, 0.48 GB of weights; medium is not worth three times the GPU and the download once
  both are calibrated (§6.3). C0 is not offered the feature.
- **Precision.** Weights are stored in FP16; activations run in FP16 where parity holds (§6), FP32 otherwise.

## 8. The job: explicit, cancellable, bounded

- **Never automatic.** The job starts only when the user asks for it on one song, from the lyrics view of a song whose
  only lyrics are plain («Sincronizar letra», marked as beta). Nothing aligns in the background, ahead of time or for
  the whole library. A later batch action, if the owner wants one, is another explicit choice with its own limits.
- **Preconditions,** each explained in the interface when unmet: the plain lyrics are on screen; the complete audio is
  local (a library file, or a stream already fully cached by playback: the job never downloads audio); the device
  tier allows a model (§7); the governor allows work now; the model is installed.
- **Model install.** The first use offers the download with its size (§6) from a pinned URL on Hugging Face, through
  the resumable downloader, with the host allowlist, a size cap and the manifest's SHA-256 checked before loading.
  Settings → Letras shows the installed model and removes it. Models are not personal data.
- **Bounds.** Songs up to `MAX_ALIGN_SECONDS`; the `Lyrics` line and character bounds; one job at a time (a second
  request is refused while one runs); a wall-clock limit of `ALIGN_TIMEOUT_FACTOR` times the expected time from the
  cost model; at most `MAX_RESEEKS` re-runs per window and a hard cap on windows per song.
- **Cancellation.** A `CancellationToken` checked after every encoder block and between windows; «Cancelar» in the
  view, quitting the application, or erasing the history cancels the job. Every job carries the controller's
  generation token (the `stream_generation` pattern), so a result for media or lyrics that changed meanwhile is
  dropped.
- **Feedback without waiting.** The view shows progress per window from the worker; playback, input and the 60 FPS
  interface never wait on it, since the work happens in the worker process.
- **Failure** keeps the plain lyrics and explains it: the lyrics do not seem to match this recording, the device got
  too warm, the job was cancelled.

## 9. Storage, privacy and erasure

- **Where.** A dedicated table added by the next numbered migration, for example `rust_aligned_lyrics(media_id,
  content_key, text_sha256, model, lyrics_json, report_json, created_at, used_at)`, with `media_id` as primary key.
  Not the provider cache: its 400 entries are evicted by browsing, and a result the user asked for should not vanish
  because they opened artist pages.
- **Bounded.** At most `ALIGNED_LYRICS_LIMIT` rows (proposed 2 000, about 40 MB at the worst case of the `Lyrics`
  bounds and well under 1 MB typically), pruned by `used_at` on write.
- **Validated again** on every read through `Lyrics::aligned` against the plain text whose hash it stores; a row whose
  plain lyrics or audio content key no longer match is ignored and replaced on the next explicit request.
- **Lookup order** (`docs/systems/Lyrics.md`): synced lyrics from the file, the catalog's synced lyrics (with word
  timing when it provides it), then aligned lyrics, then plain lyrics. Aligned lyrics never replace synced lyrics.
  The view marks them («Sincronizada en este equipo») and offers «Quitar sincronización».
- **Never written** into the user's files: no sidecar `.lrc`, no tag edits. An export would be a separate, explicit
  action.
- **Erasure.** The table reveals what the user played, so `AppCore::clear_listening_history` (Settings →
  Privacidad) deletes it in the same transaction as the provider cache. Removing the model does not erase results;
  erasing results does not remove the model.
- **Nothing leaves the device.** No lyrics, audio, timing or report is sent anywhere. Diagnostics may count jobs and
  their timings, never songs or lyrics.

## 10. Trust boundary

- The WebView sends only clicks («Sincronizar», «Cancelar», «Quitar sincronización»); the controller maps them to the
  media shown and checks it against what plays, as line seeks already do.
- The worker's (and later the server's) reply is external input: parsed into typed, bounded structs, then validated by
  `Lyrics::aligned` (§3.3).
- Models load only from safetensors or ONNX files whose SHA-256 matches a manifest compiled into the application;
  never pickle or PyTorch checkpoints at run time.
- Paths reaching the worker are confined by the application (library roots, the stream cache); the worker opens them
  read-only.

## 11. Server-ready contract

The job is a pure function of (audio, plain lyrics, language, model), so the contract is the same locally and on a
server:

```text
AlignLyricsJob    { protocol_version, job_id, content_key, audio: AudioRef, lines: [String], language, model }
AlignLyricsResult { job_id, model, model_version, lines: [LineTiming { index, start_ms, words: [start_ms], end_ms }],
                    report: { confident_share, line_confidence: [f32], gpu_ms } }
```

- **Today** `AudioRef` is a confined local path and the executor is the local worker over its versioned stdin/stdout
  protocol (`docs/AI_DJ.md` §5.8).
- **On the server** `AudioRef` becomes a catalog identifier, the queue deduplicates by (content key, SHA-256 of the
  lyrics, model version), so a popular song is aligned once for every user, and results are shared. The client still
  validates every result with `Lyrics::aligned`, and keeps its local path when offline or when the feature is off.
- **Privacy.** A server request carries a catalog id and the lyrics hash, never the listening history.
- **Rights before scale.** Lyrics are copyrighted texts and LRCLIB's are user-contributed with no stated license for
  commercial use; YouTube audio may not be processed on a server (`docs/AI_DJ.md` §7.6). Local alignment of what the
  user is already playing is the same position the application holds today for cached lyrics. A shared server-side
  alignment service needs licensed audio and licensed lyrics, or a licensed lyrics provider that already supplies
  word timing, which would also make alignment unnecessary for that catalog. LRCLIB accepts contributions
  (`/api/publish`, with a proof-of-work challenge), but publishing machine timing on the user's behalf needs the
  user's explicit review and is out of scope.

## 12. Benchmark plan

**Data.**

| Set | Content | Use | Rights |
|---|---|---|---|
| JamendoLyrics MultiLang (Hugging Face `jamendolyrics/jamendolyrics`) | 79 songs: 20 German, 20 English, 20 Spanish, 19 French; normalized lyrics with line breaks; an onset and an end for every word; 409 MB of MP3 | Word and line accuracy; the published reference numbers use it | Each song has its own Creative Commons license, several NC or ND: evaluation only, kept outside the repository, never redistributed |
| Jam-ALT (`jamendolyrics/jam-alt`, revision `v1.4.0`) | The same audio with revised lyrics as people write them (punctuation, casing, parentheses) and line timings | Robustness to real lyric text and line-start accuracy | Same audio licenses |
| The owner's set (to build) | 20–30 songs of the owner's genres (corridos, reggaeton, cumbia, regional Mexican), lyrics as LRCLIB returns them, line starts marked by hand and spot-checked words | The decision for this audience; JamendoLyrics has no Latin urban music | Private, outside the repository |

**Metrics**, all computed by the probe:

- word onset **AAE**: mean over songs of each song's mean absolute onset error, as JamendoLyrics results are
  reported; the pooled **median** error and the **signed bias** (predicted minus annotated);
- **PCO**: share of word onsets within 0.3 s, the published tolerance;
- **line-start AAE** and the share of **gross errors** (over 1 s), which is what a singer notices;
- the share of words placed and of songs that pass the confidence gate, with the accuracy of those that pass
  (a refused song costs less than a wrong one);
- **RTF**: wall-clock time over audio duration, with GPU time split into encoder, decoder, DTW, mel and decoding;
  the slowest single encoder block (the GPU chunk); peak memory; `pmset -g therm` before and after (macOS).

**Protocol.** Rules and thresholds are chosen on German and English and reported on all four languages, with Spanish
and French as the held-out check; the owner's set is held out entirely until a candidate is frozen. Every number in
this document names the model, precision, variant, machine and date.

**Commands** (this Mac; models and data outside the repository):

```sh
cargo run --locked --release -p yt-media --features metal-align --example lyrics_align_probe -- \
  <whisper-model-dir> <jamendolyrics-dir> all 0 f16 trim,clip=1,shift=0.15,lines=0.1
```

`trim,calibrate` instead measures every combination of corrections from one alignment (§6.3); for medium the chosen
corrections are `clip=1,lines=0.1` with no `shift`.

The owner's set is built and measured with:

```sh
cargo run --locked --release -p yt-media --example lyrics_set_probe -- <songs.txt> <owner-set-dir>
cargo run --locked --release -p yt-media --features metal-align --example lyrics_align_probe -- \
  <whisper-small-dir> <owner-set-dir> es 0 f16 trim,clip=1,shift=0.15,lines=0.1
```

`dedupe` drops repeated lines from the text first; `YTU_ALIGN_LRC_DIR=<dir>` writes each aligned song as enhanced LRC,
for listening in the application as a sidecar file.

The arguments are the languages (`all` or a list such as `de,en`), the number of songs (0 for all), the precision and
the variant; `YTU_ALIGN_TRACE`, `YTU_ALIGN_WORDS` and `YTU_ALIGN_CPU` print every window, every word, or run on the
CPU.

The model folder is a Hugging Face snapshot (`openai/whisper-small`, `-medium`, `-large-v3`, `-large-v3-turbo`) with
`config.json`, `generation_config.json`, `tokenizer.json` and `model.safetensors`; the dataset folder is a copy of the
JamendoLyrics repository.

**Done:** each model's timing corrections, calibrated on German and English for small and medium (§6.3); the
owner's 30 songs against LRCLIB's synced lyrics, built by `lyrics_set_probe` (§6.8).

**Still to measure,** in this order:

1. A by-ear check of the owner's set: five songs where the alignment and LRCLIB disagree most, each played with the
   aligned karaoke, marked by the owner as following the voice, early or late (about three minutes a song).
2. Jam-ALT's revised text against JamendoLyrics' normalized text, for the same songs.
3. Separated vocals (the Full edition's BS-RoFormer) against the original mix, on the songs that fail.
4. The Windows reference PC: ONNX Runtime with DirectML and the vendor's provider, parity with Metal, RTF, the
   slowest dispatch, and the GPU's temperature over a batch of songs.
5. A long run on a fanless MacBook Air under the governor's duty cycle, for heat.
6. Qwen3-ForcedAligner-0.6B on the same sets, if Whisper misses the targets.
7. Character-level teacher forcing and head selection (arXiv:2509.09987), as a cheap accuracy improvement for
   Whisper.

**Targets for shipping** (go when all hold on the owner's set and on JamendoLyrics), with where calibrated small
stands on JamendoLyrics at the 80 % gate (§6.4, §6.6):

| Measure | Target | Why | Calibrated small, JamendoLyrics |
|---|---|---|---|
| Line-start AAE among songs that pass the gate | ≤ 0.5 s | The active line must light up with the singing | 0.55 s: **missed**, by the tail (median 0.12 s, 87 % within 0.3 s) |
| Word PCO (0.3 s) among songs that pass the gate | ≥ 80 % | The fill must follow the words | 92.3 %: met |
| Gross word errors (> 1 s) among songs that pass the gate | ≤ 5 % | A singer notices them | 2.2 %: met |
| Songs that pass the gate | ≥ 70 % | Otherwise the button mostly disappoints | 74.7 %: met |
| GPU time for a 4-minute song on the reference Mac | ≤ 15 s | With the 50 % duty cycle, under half a minute of waiting | 3.4 s: met |
| Slowest GPU chunk | ≤ 50 ms | The governor's chunk rule (`docs/AI_DJ.md` §5.9) | 16 ms: met |
| Audio and interface | No glitch, 60 FPS while the job runs | §6 of `AGENTS.md` | Not measurable before the worker exists |

## 13. Stages

Each stage is delivered whole (code, tests, crate README, system document, `cargo run --locked -p xtask -- check`
green) and only when the owner asks.

| Stage | Content | Done when |
|---|---|---|
| A. Measurement and calibration (this document) | The probe and the numbers of §6 | Done on 2026-11-11 for this Mac |
| B. Owner's set and Windows | §12 items 1–4. Done: the calibration (§6.3) and the owner's songs against LRCLIB (§6.8); pending: the by-ear check and Windows | Targets checked on both reference machines |
| C. Engine | `yt-lyrics::alignment` (pure, tested with synthetic matrices), `Lyrics::aligned`, the stored table, its migration and its erasure in `clear_listening_history` | Unit tests; the probe uses the engine instead of its own copy |
| D. Runtime and worker | Whisper in `yt-infer` (Metal and ONNX Runtime), manifests, the `AlignLyrics` job in `ytu-worker` under the governor | Parity test between backends; the stage-B numbers reproduce through the worker |
| E. Interface | «Sincronizar letra» in the lyrics view, progress, cancellation, the «Sincronizada en este equipo» mark, «Quitar sincronización», model install and removal in Settings → Letras | VirtualDom tests; checked on an isolated profile in both window sizes |

Stage C can start without the worker, since the engine is pure. Stages D and E share the worker with the DJ
(`docs/AI_DJ.md` §8, stage 1): whichever ships first builds it.

## 14. Risks and open questions

| Risk | Mitigation |
|---|---|
| Lyrics that do not match the recording (another version, missing repeats, ad-libs) | The lost-path and not-sung-yet rules, the confidence gate, and refusing rather than showing a wrong karaoke |
| Genres absent from JamendoLyrics (fast reggaeton flows, corridos with requinto intros, chants) | The owner's set decides (§12); the owner can mark songs that came out wrong |
| Whisper's early bias (words start slightly before the annotation) | Measured and corrected per model on the development languages (§6.3); the constants live in the model's manifest |
| Model size on 8 GB Macs and integrated GPUs | Tiers (§7); the smaller model where it meets the targets |
| Windows driver variety | DirectML or the vendor provider through Windows ML; no CPU fallback |
| Lyrics rights for a SaaS | §11: licensed lyrics or a licensed provider before any shared server-side alignment |

**Questions for the owner**

1. Which songs make up the owner's set, and who marks their line starts?
2. Is a one-time 0.48 GB model download behind the «Sincronizar letra» button acceptable, or should the Full
   edition bundle the model?
3. Should the job be allowed on battery power (with the 25 % duty cycle), or only on AC power?
4. Is a later batch action («Sincronizar las letras de este álbum») wanted, with its own explicit confirmation?

## 15. Sources

Consulted on 2026-11-11.

- OpenAI Whisper, `whisper/timing.py` (`find_alignment`, `dtw_cpu`, `add_word_timestamps`) and the model card
  (MIT code and weights): <https://github.com/openai/whisper>
- stable-ts, long-form alignment of known text (`stable_whisper/alignment.py`, `non_whisper/alignment.py`):
  <https://github.com/jianfch/stable-ts>
- S. Durand, D. Stoller, S. Ewert, *Contrastive learning-based audio to lyrics alignment for multiple languages*,
  ICASSP 2023, arXiv:2306.07744 (Tables 2 and 3; JamendoLyrics MultiLang word annotations).
- JamendoLyrics MultiLang: <https://huggingface.co/datasets/jamendolyrics/jamendolyrics> (archived copy:
  <https://github.com/f90/jamendolyrics>).
- O. Cífka et al., *Lyrics transcription for humans: a readability-aware benchmark*, ISMIR 2024, arXiv:2408.06370
  (Jam-ALT; HTDemucs before Whisper); Jam-ALT dataset with line timings: <https://huggingface.co/datasets/jamendolyrics/jam-alt>
- *Exploiting music source separation for automatic lyrics transcription with Whisper*, arXiv:2506.15514 (2025).
- S.-L. Yeh, Y. Meng, H. Tang, *Whisper has an internal word aligner*, arXiv:2509.09987 (character-level teacher
  forcing and head selection).
- Qwen3-ASR technical report, arXiv:2601.21337, and the Qwen3-ForcedAligner-0.6B card (Apache-2.0):
  <https://huggingface.co/Qwen/Qwen3-ForcedAligner-0.6B>
- torchaudio `MMS_FA` (CC-BY-NC-4.0): <https://docs.pytorch.org/audio/main/generated/torchaudio.pipelines.MMS_FA.html>
- NVIDIA Parakeet-TDT-0.6B-v3 (licenses differ between the NGC catalog and NVIDIA's model card):
  <https://build.nvidia.com/nvidia/parakeet-tdt-0_6b/modelcard>, arXiv:2509.14128
- whisper.cpp DTW token timestamps (experimental) and their `whisper-rs` bindings (`DtwParameters`).
- onnx-community `whisper-large-v3-turbo_timestamped` (ONNX exports with cross-attention outputs for word timing):
  <https://huggingface.co/onnx-community/whisper-large-v3-turbo_timestamped>
