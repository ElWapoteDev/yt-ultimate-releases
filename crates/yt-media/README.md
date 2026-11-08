# Native media engine

Application logic is Rust. No Python, yt-dlp, Node, FFmpeg subprocess, or local
HTTP service is needed for playback, YouTube resolution or downloads.

| Capability | Implementation and verification |
| --- | --- |
| Audio playback | Rodio/CPAL output; a streaming Symphonia decoder selects a supported audio track within the container. |
| Audio formats | MP3, AAC/M4A, ALAC, WAV/PCM, FLAC, Vorbis and Opus. Opus is provided by the bundled native libopus library. MP4, WebM and Matroska containers are supported when their audio codec is supported. |
| Controls | Play, pause, seek, volume, playback rate and sleep timer. Playback-rate adjustment also changes pitch. |
| Search and metadata | `innertube-rs` with an embedded QuickJS interpreter. No external JavaScript runtime is launched. |
| Public playlists | Real browse/continuation requests preserve order and repeated videos. Up to 500 playable entries, 20 pages and 120 seconds, with cancellation. Partial results are explicitly marked; unavailable entries are counted separately. Current `continuationItemViewModel` nodes are supported. |
| YouTube audio | AAC/M4A selected for interoperability. The currently validated player client is VISIONOS; earlier IOS/ANDROID_VR clients could resolve URLs but serve only their first minute. |
| Video download | A progressive MP4 containing both audio and video. Separate high-resolution streams are not muxed. |
| Downloads | Bounded ranges, known complete length verification, cancellation, progress and atomic no-clobber completion. Failed/aborted transfers remove temporary files. |
| Existing files | No download overwrites an existing destination, including concurrent completion races. |
| Optional vocal separation | `native-stems`: real BS-RoFormer inference in Rust/LibTorch, overlapping 8-second windows and float WAV vocals/instrumental. Requires the prepared model and matching native libraries. |

The downloader treats ordinary HTTP 206 `Content-Range` responses separately
from Google video CDN query ranges, whose response length describes one chunk.
`download_sized` should receive the resolver's complete `content_length`.
Short or metadata-only network probes do not prove a complete video can download.

## Verification

`cargo test -p yt-media` covers decoding, Opus seeking, URL validation,
filename portability, cancellation, interrupted responses and byte-range joins.
These tests do not contact YouTube and do not need an audio device.

`cargo run -p yt-media --example audio_probe` plays generated digital silence
and exercises the actual audio output device. Verified on this Mac.

`cargo run -p yt-media --example online_probe -- aqz-KE-bpKQ --download`
searches/resolves the open Blender short *Big Buck Bunny*, downloads its complete
audio into an automatically removed temporary directory and decodes it natively.
On 2026-11-08 the complete 10,271,496-byte, 635-second AAC file passed this check.
Network access is required, and YouTube can change its client behavior.

`cargo run -p yt-media --example playlist_probe` reads a public MIT linear
algebra playlist. On 2026-11-08 it returned all 35 entries with `truncated=false`;
limiting it to three entries correctly returned `truncated=true`.
`cargo run -p yt-media --example playlist_probe -- UUEBb1b_L6zDS3xTUrIALZOw 105`
also passed: 105 entries from the 8,161-item MIT OpenCourseWare uploads list,
proving a real continuation beyond the first 100-item page. These probes only
read metadata and do not download videos or publish playlists.

## Build dependencies and limitations

The default desktop build needs a C/C++ compiler and CMake for bundled native
dependencies, including QuickJS and libopus; users do not need those tools to
run the packaged app. No credentials or browser cookies are read automatically.
Private/restricted content, sign-in/PO-token flows, live recording, high-resolution
video/audio muxing and format conversion are not implemented here.

AI processing is a separate optional build. See [model and native runtime
instructions](models/README.md). A real Rust inference probe passed on this Mac;
this does not establish Windows execution. The additional CALIDAD DSP chain and
its numerical reference tests live in [`yt-quality`](../yt-quality/README.md).
