use super::*;
use crate::wav::{Reader, Spec, Writer};
use std::{
    f64::consts::TAU,
    path::{Path, PathBuf},
};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}
fn reference() -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(fixture("reference.json")).unwrap()).unwrap()
}
fn read(path: impl AsRef<Path>) -> Vec<f64> {
    let mut reader = Reader::open(path).unwrap();
    reader.read(reader.spec.frames as usize).unwrap()
}
fn write(path: &Path, samples: &[f64]) {
    let mut writer = Writer::new(
        path,
        Spec {
            rate: 44100,
            channels: 2,
            frames: samples.len() as u64 / 2,
        },
    )
    .unwrap();
    writer.write(samples).unwrap();
    writer.finish().unwrap();
}
fn maximum_error(left: &[f64], right: &[f64]) -> f64 {
    assert_eq!(left.len(), right.len());
    left.iter()
        .zip(right)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f64::max)
}

#[test]
fn spectral_eq_matches_original_numpy_reference() {
    let temp = tempfile::tempdir().unwrap();
    let out = temp.path().join("eq.wav");
    let report = dynamic_instrumental_eq(
        fixture("input.wav"),
        &out,
        "rust-parity-v1",
        6.0,
        &CancellationToken::new(),
    )
    .unwrap();
    let error = maximum_error(&read(&out), &read(fixture("eq.wav")));
    assert!(error < 2e-14, "EQ reference difference {error}");
    assert_eq!(
        report.seed_sha256,
        reference()["eq"]["seed_sha256"].as_str().unwrap()
    );
    assert_eq!(report.frames, 44237);
}

#[test]
fn zero_gain_preserves_edges_and_arbitrary_lengths() {
    for frames in [1, 17, 511, 512, 2048, 4103] {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("in.wav");
        let out = temp.path().join("out.wav");
        let mut samples: Vec<f64> = (0..frames * 2)
            .map(|i| 0.15 * (i as f64 * 1.349).sin())
            .collect();
        samples[0] = 0.7;
        *samples.last_mut().unwrap() = -0.5;
        write(&input, &samples);
        dynamic_instrumental_eq(&input, &out, "zero", 0.0, &CancellationToken::new()).unwrap();
        assert!(maximum_error(&samples, &read(out)) < 1e-14);
    }
}

#[test]
fn true_peak_matches_scipy_polyphase_reference() {
    let report = estimate_true_peak(fixture("input.wav"), &CancellationToken::new()).unwrap();
    let expected = reference()["peak"]["peak_linear"].as_f64().unwrap();
    assert!(
        (report.peak_linear - expected).abs() < 1e-12,
        "{} vs {}",
        report.peak_linear,
        expected
    );
}

#[test]
fn shared_mapping_matches_original_soxr_reference() {
    let temp = tempfile::tempdir().unwrap();
    let out = temp.path().join("remap.wav");
    let report =
        shared_smooth_remap(fixture("input.wav"), &out, &CancellationToken::new()).unwrap();
    assert_eq!(
        report.output_frames,
        reference()["remap"]["output_frames"].as_u64().unwrap()
    );
    let error = maximum_error(&read(&out), &read(fixture("remap.wav")));
    assert!(error < 2e-6, "SoXR reference difference {error}");
    assert!(report.map_first_source_sample <= 2.0);
    assert!(report.input_frames as f64 - 1.0 - report.map_last_source_sample <= 2.0);
    assert!(report.measured_pitch_min_cents.unwrap() > 19.0);
    assert!(report.measured_pitch_max_cents.unwrap() < 37.0);
}

#[test]
fn shared_headroom_preserves_stereo_and_limits_intersample_peak() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input.wav");
    let out = temp.path().join("out.wav");
    let samples: Vec<f64> = (0..44100)
        .flat_map(|i| {
            let value = 0.95 * (TAU * 440.0 * i as f64 / 44100.0).sin();
            [value, -value * 0.5]
        })
        .collect();
    write(&input, &samples);
    let report = shared_smooth_remap(&input, &out, &CancellationToken::new()).unwrap();
    assert!(report.shared_gain < 1.0);
    let result = read(&out);
    assert!(result
        .chunks_exact(2)
        .all(|v| (v[1] + v[0] * 0.5).abs() < 1e-12));
    assert!(
        estimate_true_peak(out, &CancellationToken::new())
            .unwrap()
            .peak_dbtp
            .unwrap()
            <= -1.09
    );
}

#[test]
fn mix_rejects_saturated_voice_and_preserves_valid_voice() {
    let temp = tempfile::tempdir().unwrap();
    let v = temp.path().join("v.wav");
    let m = temp.path().join("m.wav");
    let out = temp.path().join("mix.wav");
    write(&v, &[0.8, -0.8, 0.3, -0.3]);
    write(&m, &[0.5, -0.5, 0.4, -0.4]);
    let report = mix_preserving_vocals(&v, &m, &out, &CancellationToken::new()).unwrap();
    assert!((report.instrumental_gain - 0.4).abs() < 1e-6);
    assert!(read(&out).iter().all(|v| v.abs() <= 1.0));
    let bad = temp.path().join("bad.wav");
    write(&bad, &[1.2, -1.2, 0.0, 0.0]);
    assert!(mix_preserving_vocals(
        &bad,
        &m,
        temp.path().join("fail.wav"),
        &CancellationToken::new()
    )
    .is_err());
    assert!(!temp.path().join("fail.wav").exists());
}

#[test]
fn cancellation_and_existing_destinations_preserve_sources() {
    let temp = tempfile::tempdir().unwrap();
    let out = temp.path().join("out.wav");
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        shared_smooth_remap(fixture("input.wav"), &out, &cancel),
        Err(QualityError::Cancelled)
    ));
    let cancel = CancellationToken::new();
    let result = process_stems(
        fixture("input.wav"),
        fixture("input.wav"),
        &out,
        "cancel",
        &cancel,
        |progress| {
            if progress.fraction >= 0.35 {
                cancel.cancel();
            }
        },
    );
    assert!(matches!(result, Err(QualityError::Cancelled)));
    assert!(std::fs::read_dir(temp.path()).unwrap().next().is_none());
    std::fs::write(&out, b"existing").unwrap();
    assert!(matches!(
        dynamic_instrumental_eq(
            fixture("input.wav"),
            &out,
            "seed",
            6.0,
            &CancellationToken::new()
        ),
        Err(QualityError::DestinationExists)
    ));
    assert_eq!(std::fs::read(out).unwrap(), b"existing");
}

#[test]
fn final_flac_export_is_audited_and_changes_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let encoded = temp.path().join("output.flac");
    let cancel = CancellationToken::new();
    yt_audio::convert(
        &fixture("remap.wav"),
        &encoded,
        yt_audio::AudioFormat::Flac,
        &cancel,
        |_| {},
    )
    .unwrap();
    let report = verify_final_flac(fixture("remap.wav"), &encoded, &cancel).unwrap();
    assert_eq!(report.bit_depth, 24);
    assert!(report.max_abs_error <= 2_f64.powi(-23));
    let mut samples = read(fixture("remap.wav"));
    samples[200] += 0.001;
    let altered = temp.path().join("altered.wav");
    write(&altered, &samples);
    assert!(verify_final_flac(altered, encoded, &cancel).is_err());
}

#[test]
fn extensible_float_stems_are_supported() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("float-stem.wav");
    let mut writer = hound::WavWriter::create(
        &path,
        hound::WavSpec {
            channels: 2,
            sample_rate: 44100,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for value in [0.125_f32, -0.25, 0.5, -0.75] {
        writer.write_sample(value).unwrap();
    }
    writer.finalize().unwrap();
    assert_eq!(read(path), [0.125, -0.25, 0.5, -0.75]);
}

#[test]
fn preparation_resamples_with_antialiasing_and_duplicates_mono() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("mono.wav");
    let output = temp.path().join("prepared.wav");
    let mut writer = hound::WavWriter::create(
        &input,
        hound::WavSpec {
            channels: 1,
            sample_rate: 48000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for frame in 0..48000 {
        writer
            .write_sample((0.5 * (TAU * 23000.0 * frame as f64 / 48000.0).sin()) as f32)
            .unwrap();
    }
    writer.finalize().unwrap();
    let report = prepare_source(input, &output, &CancellationToken::new()).unwrap();
    assert_eq!(report.input_frames, 48000);
    assert_eq!(report.output_frames, 44100);
    let samples = read(&output);
    assert!(samples.chunks_exact(2).all(|frame| frame[0] == frame[1]));
    let body = &samples[2000..samples.len() - 2000];
    let rms = (body.iter().map(|v| v * v).sum::<f64>() / body.len() as f64).sqrt();
    assert!(rms < 0.002, "Above-Nyquist tone was not filtered: {rms}");
    assert_eq!(
        hound::WavReader::open(output)
            .unwrap()
            .spec()
            .bits_per_sample,
        32
    );
}
