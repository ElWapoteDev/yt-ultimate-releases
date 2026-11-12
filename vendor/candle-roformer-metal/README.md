# Candle RoFormer Metal

The axial transformer and spectral inference code adapt [candle-roformer 0.1.0](https://crates.io/crates/candle-roformer/0.1.0), upstream revision `1f823c41414249472617d8b275fee17dd8d9ef33`. See `LICENSE` and `NOTICE` for attribution. The model weights are separate.

This implementation targets the stereo, 44.1 kHz ViperX BS-RoFormer checkpoint with 62 disjoint frequency bands and 12 axial transformer stages. RustFFT computes STFT and inverse STFT. Candle runs the network on Metal with fused attention, rotary embeddings and RMS normalization. Linear projections flatten leading dimensions to avoid broadcasted Metal matrix products. Fourier plans and rotary tables are reused between windows. The runtime loads FP16 safetensors and uses FP32 arithmetic on the GPU.
