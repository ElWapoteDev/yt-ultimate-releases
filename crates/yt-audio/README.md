# yt-audio

In-process MP3, WAV, FLAC, M4A, AAC/ADTS, OGG/Vorbis and Opus export, audio variants
and fingerprints. Encoding uses bundled LAME, Fraunhofer FDK AAC, Vorbis and
Opus libraries; Hound writes 24-bit WAV and flacenc writes 24-bit FLAC. M4A and
Ogg packet/container handling is Rust. Native codec license texts are under
`packaging/licenses`; no FFmpeg, Python or Node runtime is launched. All
exports publish through a private temporary file without overwriting a result.
Cancellation and decoder errors prevent partial output from being published.

`convert(input, output, format, cancel, progress)` preserves source tags and
pictures. `convert_with_metadata(input, output, format, metadata, cancel,
progress)` applies `yt_core::MediaMetadata` overrides, including a validated
local cover. Use `AppCore::media_metadata(track_id)` for existing library entries;
for downloads, supply title/artist/source URL and a cover cached by the core.
`None` preserves a source field; `Some("")` clears it. Setting
`preserve_source_tags: false` uses only explicit metadata.

`copy_metadata(input, output, metadata, cancel)` tags an already generated
result in any supported format, such as a variant or stem. It writes a private copy and
atomically replaces the result only after successful tagging; it rejects the
source as destination and never modifies the source. Existing output tags are
replaced, including when explicit metadata preservation is disabled.

Tags are written as ID3v2 in MP3/WAV/AAC, MP4 `ilst` in M4A and Vorbis comments
in FLAC/OGG/Opus. Standard fields,
multi-value items, custom text and pictures are retained where representable.
ID3 format-specific frames remain native for ID3 outputs. When a field cannot be
represented in the target container, its serialized original tag is preserved
as base64 under `YTULTIMATE_SOURCE_TAG_<TYPE>` in addition to normal mapped tags;
ordinary players do not interpret this archival field as a native frame. This
prevents silent loss of private binary metadata when changing tag formats.
Metadata is capped at 64 MiB; supplied artwork is decoded/validated and capped
at 16 MiB. Unsupported source containers may have no Lofty tags to copy.

All encoders consume bounded PCM blocks. FLAC keeps one frame plus incremental
STREAMINFO/MD5 state, including for sources larger than the former 512 MiB limit.
It supports high-rate PCM through 655350 Hz within the FLAC format's limits;
its short last frame, frame CRCs and complete PCM MD5 survive tagging. M4A retains
a bounded packet index, capped at eight million access units, and writes a
fast-start index without buffering the audio payload. Opus uses 48 kHz and AAC
uses a supported rate up to 48 kHz; FFT resampling preserves channel layout and
trims converter delay. Vorbis quality mode accommodates mono and unusual rates.
Source files are never tagged in place.

Variants use linked SoundTouch for independent pitch/tempo, Rust EQ and constant
headroom gain. Inputs from 8–384 kHz are supported. Sources above SoundTouch's
192 kHz limit are temporarily resampled and restored to their original output
rate. The DSP implementation and resulting audio are not bit-identical to v4's
FFmpeg filters. Chromaprint algorithm 2, its first-120-second window and the
legacy alignment rules are retained; its result is a local comparison, not a
recognition-service lookup. The PCM hash records native-rate PCM16 and channel
layout, identified as `native_rate_pcm16le_with_layout_v1` in processing metadata.

Tests encode and decode all seven real formats; preserve title, artist, album,
composer, album artist, date, genre, track/disc totals, lyrics, comments, custom
text, private ID3 bytes and cover bytes; verify source immutability and atomic
failure on invalid artwork; and cover cancellation, pitch variants and audio
fingerprint stability. A generated source exceeding 512 MiB of virtual PCM
checks bounded FLAC output and an independently calculated MD5 without allocating
that source in memory or writing a large input file. No test uses a real user profile.
