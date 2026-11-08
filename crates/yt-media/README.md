# Native media engine

Application logic is Rust. No Python, yt-dlp, Node, FFmpeg subprocess, or local
HTTP service is needed for playback, YouTube resolution or downloads.

| Capability | Implementation and verification |
| --- | --- |
| Audio playback | Rodio/CPAL output; a streaming Symphonia decoder selects a supported audio track within the container. |
| Audio formats | MP3, AAC/M4A, ALAC, WAV/PCM, FLAC, Vorbis and Opus. Opus is provided by the bundled native libopus library. MP4, WebM and Matroska containers are supported when their audio codec is supported. |
| Controls | Play, pause, seek, volume, sleep timer and SoundTouch tempo from 0.25× to 4× while preserving pitch. Position is reported in source time. |
| Search and metadata | `innertube-rs` with an embedded QuickJS interpreter. Complete legacy/modern search cards are parsed once, merging duplicate identities and reading duration badges without inventing rows from nested watch commands. No external JavaScript runtime is launched. |
| Public playlists | Real browse/continuation requests preserve order and repeated videos. Up to 500 playable entries, 20 pages and 120 seconds, with cancellation. Partial results are explicitly marked; unavailable entries are counted separately. Current `continuationItemViewModel` nodes are supported. |
| HTTP playback | A seekable disk range cache opens the decoder before download completion, prioritizes seeks, reports buffering and cancels promptly. One expired YouTube URL can be renewed using the original connection preferences. |
| YouTube audio | AAC/M4A selected for interoperability. The currently validated player client is VISIONOS; earlier IOS/ANDROID_VR clients could resolve URLs but serve only their first minute. |
| Video muxing | Pure Rust MP4/fMP4 demux and mux preserve encoded samples, codec configuration, DTS/CTS, keyframes, edit lists, rotation and language. No external FFmpeg runs. Files with discontinuous timelines, segments/metadata above 64 MiB, or more than two million samples per track are explicitly rejected. |
| Video selection | Best or a maximum height from 144p to 4320p. Returns the actual resolution and separate original MP4 video/AAC tracks where DASH is needed. H.264 is preferred at equal height and frame rate for compatibility. |
| Temporary video playback | `prepare_video` selects H.264/AAC, downloads and muxes original tracks into an owned temporary MP4 outside the library. It reports bytes and actual resolution, supports cancellation, and removes its files on drop. This fallback waits for preparation when no progressive video is available; it does not depend on HLS support in the Windows WebView. |
| Downloads | Bounded ranges, known complete length verification, cancellation, progress and atomic no-clobber completion. Failed/aborted transfers remove temporary files. |
| Resumable downloads | Durable ID/itag/language/DRC/size/SHA256 checkpoints, OS file locks and validated range retries. Restarting with a fresh URL resumes committed bytes; corrupted or mismatched partials are rejected. Cancellation pauses; explicit discard deletes only the matching inactive job. |
| Subtitles | Lists the actual caption tracks, downloads the exact selected language, and exports timed SRT/WebVTT without a subprocess. |
| Connection preferences | Configurable socket timeout and HTTP(S) proxy; explicit Netscape cookie files filter expired, foreign-origin and path-restricted cookies. A user-initiated browser connection reads only YouTube cookies and keeps the session in memory. Native Keychain/DPAPI permission checks are honored. Credentials and signed URLs are excluded from diagnostic output and resume files. Browser stores are never opened automatically. |
| Cover art | Bounded 16 MiB HTTPS image downloads from YouTube image hosts, with cancellation, redirect validation, and no account cookies. |
| Existing files | No download overwrites an existing destination, including concurrent completion races. |
| Optional vocal separation | `native-stems`: real BS-RoFormer inference in Rust/LibTorch, overlapping 8-second windows and float WAV vocals/instrumental. Detects available CUDA/MPS and retries on CPU if the model cannot execute there. Reports the actual device and fallback reason. The current model falls back on this Mac because MPS cannot load its float64 constants; Windows Full bundles CPU LibTorch. |

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

`cargo run -p yt-media --example stream_probe` verifies decoding and seeking
without an audio device. The same complete 10.27 MiB YouTube source opened in
167 ms after only 256 KiB had arrived; seeking to 120 seconds also passed while
the cache was incomplete. A deliberately slow local range-server regression test
asserts both behaviors without Internet access.

`cargo run -p yt-media --example resume_probe` pauses that public audio at
1,048,576 bytes, creates a new resolver and downloader, resumes it, and decodes all
634.625 seconds. Offline tests also simulate an uncommitted crash tail, corrupted
committed bytes, and mismatched identities. The temporary files are removed.

`cargo run -p yt-media --example dash_probe` selects the Blender short's best
available resolution (2160p60 in the verified response), then downloads its 144p
MP4 video and AAC audio into `ytu-dash-probe` under the operating system temporary
directory for mux verification. An optional second argument selects the directory.

`cargo run -p yt-media --example subtitles_probe` found 36 caption tracks for
3Blue1Brown's linear algebra preview and retrieved the English track's 80 cues
(8,044-byte WebVTT). It never downloads a substitute language silently.

`cargo run -p yt-media --example preview_probe -- 'peso pluma' --prepare`
verified 23 unique search results with actual durations, then prepared Hollywood
as a 17,798,586-byte H.264/AAC 360p MP4. Preparation and decoding all 289.065 seconds
of audio completed in 9.44 seconds on this Mac. The file is removed on completion.

## Resuming a job

Persist the canonical video ID and destination in the app job record, then resolve
a fresh URL when retrying. `ResumeKey::for_media(&media)` contains only the video
identity, format, audio-track identifier, DRC flag and expected total. YouTube can
reuse the same itag for several dubbed or compressed variants; `resolve_resume`
renews the exact saved variant instead of switching its language. Pass it to `Downloader::download_resumable`.
`resume_status` exposes committed progress. Cancelling its token retains the
checkpoint for a retry; after the future finishes, `discard_resume` explicitly
removes that matching job's partial files. Neither operation overwrites a final
file. Checkpoint digests are validated before truncating an uncommitted tail or
requesting any additional bytes.

## Build dependencies and limitations

The default desktop build needs a C/C++ compiler and CMake for bundled native
dependencies, including QuickJS, libopus and SoundTouch; users do not need those tools to
run the packaged app. No credentials or browser cookies are read automatically.
Authenticated access depends on YouTube accepting the explicitly supplied cookies;
private-account access has not been verified against a real account. The explicit
browser reader supports Firefox SQLite, Safari binarycookies on macOS, and
Chromium-family profiles through macOS Keychain or the current Windows user's
DPAPI key. Chromium Windows App-Bound `v20` cookies are rejected with an actionable
error; the app never elevates, injects into a browser, or bypasses that protection.
Tests use synthetic profiles and keys only, including domain/partition/expiry
filtering and parser corruption checks. No real browser account was read for
validation. The adapted Safari parser's MIT license is in `LICENSE-UNJAR`.
Interactive sign-in/PO-token challenges and live recording are not implemented.
Format conversion lives in `yt-audio`. Playback of video codecs such
as AV1/VP9 depends on the operating system's video decoder; the downloaded source
codec is reported instead of silently pretending every system supports it.

AI processing is a separate optional build. See [model and native runtime
instructions](models/README.md). A real Rust inference probe passed on this Mac;
this does not establish Windows execution. The additional CALIDAD DSP chain and
its numerical reference tests live in [`yt-quality`](../yt-quality/README.md).
