# Native desktop and media components

The application's own license does not replace the licenses of its dependencies.
These notices cover the native desktop and media components added during the Rust migration;
they are not an exhaustive inventory of every transitive Cargo dependency.

| Component | Version / source | License declared by the component |
| --- | --- | --- |
| LAME | 3.100, included through mp3lame-sys 0.1.11 | See LAME-COPYING.txt |
| mp3lame-sys | 0.1.11, https://github.com/DoumanAsh/mp3lame-sys | LGPL-3.0 |
| mp3lame-encoder | 0.2.5, https://github.com/DoumanAsh/mp3lame-encoder | LGPL-3.0 |
| SoundTouch | 2.3.2, included through soundtouch-ffi 0.4.1 | LGPL-2.1 |
| soundtouch | 0.5.4, https://github.com/Cyanistic/soundtouch | LGPL-2.1 |
| rusty-chromaprint | 0.3.0, https://github.com/darksv/rusty-chromaprint | MIT, as declared in the published Cargo manifest |
| unjar binarycookies parser | adapted from https://github.com/vladkens/unjar | MIT; see unjar-LICENSE-MIT.txt |
| Lucide icons | 0.554.0, https://lucide.dev | ISC; original icon paths reused from the legacy interface |
| arboard | 3.7.0, https://crates.io/crates/arboard | MIT or Apache-2.0; MIT text included |
| souvlaki | 0.9.3, https://github.com/Sinono3/souvlaki | MIT; see souvlaki-LICENSE-MIT.txt |
| window-vibrancy | 0.8.1, https://github.com/tauri-apps/window-vibrancy | MIT or Apache-2.0; MIT text included |
| drag | 2.1.1, https://crates.io/crates/drag | MIT or Apache-2.0; MIT text included |
| shiguredo_mp4 | 2026.5.0, https://github.com/shiguredo/mp4-rs | Apache-2.0; see shiguredo-mp4-APACHE-2.0.txt |
| Opus | bundled by opusic-sys 0.7.7 | See Opus-COPYING.txt |
| Fraunhofer FDK AAC | bundled by fdk-aac-sys 0.5.0, build-only patch in vendor/fdk-aac-sys | Fraunhofer FDK AAC license; see FDK-AAC-NOTICE.txt. The Rust wrapper's MIT declaration does not replace the codec license |
| Vorbis aoTuV/Lancer | bundled by aotuv_lancer_vorbis_sys 0.1.6 through vorbis_rs 0.5.6 | BSD-3-Clause; see Vorbis-COPYING.txt |
| Ogg | bundled by ogg_next_sys 0.1.5 through vorbis_rs 0.5.6 | BSD-3-Clause; see Ogg-COPYING.txt |
| SoXR | native source included through soxr-sys 0.1.3 | LGPL-2.1-or-later; see SoXR-LICENSE.txt. The wrapper's Apache-2.0 declaration does not replace the native library license |
| LibTorch, optional vocal-separation edition | 2.12.0, https://github.com/pytorch/pytorch | See LibTorch-LICENSE.txt, including its third-party notices |
| tch / torch-sys | tch-rs commit bbf49e6, https://github.com/LaurentMazare/tch-rs | MIT or Apache-2.0; MIT text included |
| Microsoft Visual C++ runtime, Windows vocal-separation edition | 14.44.35211, official vc_redist.x64.exe | Microsoft-VC-Runtime-License.rtf; app-local x64 msvcp140/vcruntime140/vcruntime140_1 DLLs |

The original license texts are included without modification. The local LAME
build patch and its source are retained under `vendor/mp3lame-sys` in the source
project. No LAME codec implementation was changed by that patch.

The optional BS-RoFormer model is a separate trained asset. Its exported graph,
provenance and checksum are documented in `crates/yt-media/models/README.md` in
the source project; software-library notices do not grant rights to that model.

The published preview includes a source snapshot; later development adds
components listed here that are not in that immutable preview. A distribution
must include the corresponding source/relinking materials required by its
library licenses and the complete dependency/model notices for that release.
