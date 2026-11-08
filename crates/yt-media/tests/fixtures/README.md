# Decoder fixtures

`tone.opus` is a generated one-second 440 Hz stereo sine tone at 48 kHz,
encoded as Ogg Opus at 32 kbit/s. It contains no third-party recording.
The fixture tests the bundled native Opus decoder and seeking without an
external program, network connection, or audio output device.

Generated for development with FFmpeg's `sine` filter and `libopus` encoder;
FFmpeg is not a runtime or test dependency.
