use crate::{
    check, estimate_true_peak, invalid,
    wav::{Reader, Writer},
    CancellationToken, Result,
};
use std::{
    collections::VecDeque,
    f64::consts::{PI, TAU},
    ffi::CStr,
    path::Path,
    ptr,
};

const RATE: f64 = 44_100.0;
const PADDING: usize = 1024;
const BLOCK: usize = 441;
// soxr-sys 0.1.3's vr32.c publishes fade_coefs[0] before completing its other
// process-global coefficient tables. Serialize creation through the complete
// C call; our nonzero ratio/channels force native initialization before return.
// Existing, initialized instances continue processing concurrently.
static SOXR_CREATION: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Clone, Debug, serde::Serialize)]
pub struct RemapReport {
    pub method: &'static str,
    pub soxr_version: String,
    pub input_frames: u64,
    pub output_frames: u64,
    pub mean_speed_ratio: f64,
    pub shared_gain: f64,
    pub pre_gain_true_peak_dbtp: Option<f64>,
    pub map_first_source_sample: f64,
    pub map_last_source_sample: f64,
    pub map_min_increment: f64,
    pub map_max_increment: f64,
    pub measured_pitch_min_cents: Option<f64>,
    pub measured_pitch_max_cents: Option<f64>,
    pub marker_amplitude_max_error: f64,
    pub reported_delay_after_flush: f64,
}

/// Shared stereo variable-rate mapping, including independent quadrature audit
/// markers and constant true-peak headroom gain. Matches the legacy parameters.
pub fn shared_smooth_remap(
    source: impl AsRef<Path>,
    destination: impl AsRef<Path>,
    cancel: &CancellationToken,
) -> Result<RemapReport> {
    check(cancel)?;
    let mut audio = Reader::open(source)?;
    let spec = audio.spec;
    if spec.rate != 44100 || spec.channels != 2 || spec.frames < 2 {
        return Err(invalid("El remapeo requiere audio estéreo de 44.1 kHz."));
    }
    let destination = destination.as_ref();
    if destination.exists() {
        return Err(crate::QualityError::DestinationExists);
    }
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let temporary = tempfile::Builder::new()
        .prefix(".yt-remap-")
        .tempdir_in(parent)?;
    let raw_path = temporary.path().join("unscaled.wav");
    let mut raw = Writer::new(&raw_path, spec)?;
    let duration = (spec.frames - 1) as f64 / RATE;
    let initial = 2_f64.powf(0.28 / 12.0);
    let maximum = 2_f64.powf(0.36 / 12.0);
    let mut stream = Soxr::new(maximum, 4)?;
    stream.ratio(initial, 0)?;
    let mut previous_ratio = initial;
    let mut phase_previous: Option<f64> = None;
    let mut first_position = None;
    let mut last_position = None;
    let mut minimum_step = f64::INFINITY;
    let mut maximum_step = f64::NEG_INFINITY;
    let mut minimum_cents = f64::INFINITY;
    let mut maximum_cents = f64::NEG_INFINITY;
    let mut marker_error = 0.0_f64;
    let mut written = 0_u64;
    let mut finished = false;
    let mut tail = VecDeque::with_capacity(2206);
    let total = spec.frames + 2 * PADDING as u64;
    for position in (0..total).step_by(BLOCK) {
        check(cancel)?;
        let count = (total - position).min(BLOCK as u64) as usize;
        let mut block = vec![0.0; count * 4];
        let valid_count = (0..count)
            .filter(|i| {
                let index = position as i64 + *i as i64 - PADDING as i64;
                index >= 0 && index < spec.frames as i64
            })
            .count();
        let samples = audio.read(valid_count)?;
        let mut sample_position = 0;
        for i in 0..count {
            let index = position as i64 + i as i64 - PADDING as i64;
            if index >= 0 && index < spec.frames as i64 {
                block[i * 4..i * 4 + 2]
                    .copy_from_slice(&samples[sample_position..sample_position + 2]);
                sample_position += 2;
            }
            let phase = TAU * index as f64 / RATE;
            block[i * 4 + 2] = phase.sin();
            block[i * 4 + 3] = phase.cos();
        }
        let time = (position as f64 + count as f64 - PADDING as f64) / RATE;
        let taper_in = (PI / 2.0 * (time / 3.0).clamp(0.0, 1.0)).sin().powi(2);
        let taper_out = (PI / 2.0 * ((duration - time) / 3.0).clamp(0.0, 1.0))
            .sin()
            .powi(2);
        let modulation = 0.05 * (TAU * time / 13.0).sin() + 0.03 * (TAU * time / 23.0 + 1.1).sin();
        let ratio = 2_f64.powf((0.28 + taper_in * taper_out * modulation) / 12.0);
        stream.ratio(
            ratio,
            (count as f64 / previous_ratio).round().max(1.0) as usize,
        )?;
        let result = stream.chunk(&block, position + count as u64 == total)?;
        previous_ratio = ratio;
        let mut selected = Vec::with_capacity(result.len() / 2);
        for frame in result.as_chunks::<4>().0 {
            if frame.iter().any(|v| !v.is_finite()) {
                return Err(invalid("El remapeo produjo muestras inválidas."));
            }
            let raw_phase = frame[2].atan2(frame[3]);
            let phase = phase_previous.map_or(raw_phase, |old| {
                old + (raw_phase - old + PI).rem_euclid(TAU) - PI
            });
            phase_previous = Some(phase);
            let mapping = phase * RATE / TAU;
            let amplitude = frame[2].hypot(frame[3]);
            let valid = amplitude > 0.99 && mapping >= 0.0 && mapping <= (spec.frames - 1) as f64;
            if !valid {
                if written > 0 || !selected.is_empty() {
                    finished = true;
                }
                continue;
            }
            if finished {
                return Err(invalid("El remapeo produjo un mapa discontinuo."));
            }
            if let Some(previous) = last_position {
                let step = mapping - previous;
                if step <= 0.0 {
                    return Err(invalid("El remapeo invirtió el tiempo."));
                }
                minimum_step = minimum_step.min(step);
                maximum_step = maximum_step.max(step);
            }
            first_position.get_or_insert(mapping);
            last_position = Some(mapping);
            tail.push_back(mapping);
            if tail.len() > 2205 {
                let previous = tail.pop_front().unwrap();
                let slope = (mapping - previous) / 2205.0;
                let cents = 1200.0 * slope.log2();
                if !cents.is_finite() || cents.abs() > 50.0 {
                    return Err(invalid("El remapeo superó el límite de medio semitono."));
                }
                minimum_cents = minimum_cents.min(cents);
                maximum_cents = maximum_cents.max(cents);
            }
            marker_error = marker_error.max((amplitude - 1.0).abs());
            selected.extend_from_slice(&frame[..2]);
        }
        check(cancel)?;
        raw.write(&selected)?;
        written += (selected.len() / 2) as u64;
    }
    let (first, last) = first_position
        .zip(last_position)
        .ok_or_else(|| invalid("El remapeo no produjo audio."))?;
    if first > 2.0
        || (spec.frames - 1) as f64 - last > 2.0
        || spec.frames as f64 > written as f64 * 1.02 + 2.0
    {
        return Err(invalid("El remapeo no conservó el intervalo completo."));
    }
    check(cancel)?;
    raw.finish()?;
    let peak = estimate_true_peak(&raw_path, cancel)?;
    let gain = if peak.peak_linear > 0.0 {
        (10_f64.powf(-1.1 / 20.0) / peak.peak_linear).min(1.0)
    } else {
        1.0
    };
    let mut input = Reader::open(raw_path)?;
    let mut output = Writer::new(destination, input.spec)?;
    loop {
        check(cancel)?;
        let mut samples = input.read(65536)?;
        if samples.is_empty() {
            break;
        }
        for sample in &mut samples {
            *sample *= gain;
        }
        output.write(&samples)?;
    }
    check(cancel)?;
    output.finish()?;
    Ok(RemapReport {
        method: "shared_smooth_remap",
        soxr_version: stream.version(),
        input_frames: spec.frames,
        output_frames: written,
        mean_speed_ratio: spec.frames as f64 / written as f64,
        shared_gain: gain,
        pre_gain_true_peak_dbtp: peak.peak_dbtp,
        map_first_source_sample: first,
        map_last_source_sample: last,
        map_min_increment: minimum_step,
        map_max_increment: maximum_step,
        measured_pitch_min_cents: minimum_cents.is_finite().then_some(minimum_cents),
        measured_pitch_max_cents: maximum_cents.is_finite().then_some(maximum_cents),
        marker_amplitude_max_error: marker_error,
        reported_delay_after_flush: stream.delay(),
    })
}

pub(crate) struct Soxr {
    handle: soxr_sys::soxr_t,
    channels: usize,
}
impl Soxr {
    fn new(maximum_ratio: f64, channels: usize) -> Result<Self> {
        Self::create(maximum_ratio, channels, true)
    }
    pub(crate) fn fixed(ratio: f64, channels: usize) -> Result<Self> {
        Self::create(ratio, channels, false)
    }
    fn create(maximum_ratio: f64, channels: usize, variable: bool) -> Result<Self> {
        let _initialization = SOXR_CREATION
            .lock()
            .map_err(|_| invalid("Falló la inicialización de SoXR."))?;
        // SAFETY: specs are produced by the linked C API; all pointers live
        // through create(), which copies the specs into its owned instance.
        unsafe {
            let io = soxr_sys::soxr_io_spec(
                soxr_sys::soxr_datatype_t_SOXR_FLOAT64_I,
                soxr_sys::soxr_datatype_t_SOXR_FLOAT64_I,
            );
            let quality = soxr_sys::soxr_quality_spec(
                soxr_sys::SOXR_20_BITQ as _,
                if variable { soxr_sys::SOXR_VR as _ } else { 0 },
            );
            let mut error = ptr::null();
            let handle = soxr_sys::soxr_create(
                maximum_ratio,
                1.0,
                channels as _,
                &mut error,
                &io,
                &quality,
                ptr::null(),
            );
            if let Err(error) = soxr_error(error) {
                if !handle.is_null() {
                    soxr_sys::soxr_delete(handle);
                }
                return Err(error);
            }
            if handle.is_null() {
                return Err(invalid("No se pudo crear SoXR."));
            }
            Ok(Self { handle, channels })
        }
    }
    fn ratio(&mut self, ratio: f64, slew: usize) -> Result<()> {
        // SAFETY: handle remains owned and valid until Drop.
        unsafe { soxr_error(soxr_sys::soxr_set_io_ratio(self.handle, ratio, slew)) }
    }
    pub(crate) fn chunk(&mut self, input: &[f64], last: bool) -> Result<Vec<f64>> {
        let mut consumed = 0;
        let frames = input.len() / self.channels;
        let mut output = Vec::new();
        while consumed < frames {
            let mut buffer = vec![0.0; 4096 * self.channels];
            let (mut used, mut produced) = (0, 0);
            // SAFETY: input/output contain exactly the advertised interleaved
            // frame counts; output allocation stays alive throughout the call.
            unsafe {
                soxr_error(soxr_sys::soxr_process(
                    self.handle,
                    input[consumed * self.channels..].as_ptr().cast(),
                    frames - consumed,
                    &mut used,
                    buffer.as_mut_ptr().cast(),
                    4096,
                    &mut produced,
                ))?;
            }
            if used == 0 && produced == 0 {
                return Err(invalid("SoXR no pudo avanzar."));
            }
            consumed += used;
            output.extend_from_slice(&buffer[..produced * self.channels]);
        }
        if last {
            loop {
                let mut buffer = vec![0.0; 4096 * self.channels];
                let mut produced = 0;
                // SAFETY: null input is the documented end-of-input/flush signal.
                unsafe {
                    soxr_error(soxr_sys::soxr_process(
                        self.handle,
                        ptr::null(),
                        0,
                        ptr::null_mut(),
                        buffer.as_mut_ptr().cast(),
                        4096,
                        &mut produced,
                    ))?;
                }
                output.extend_from_slice(&buffer[..produced * self.channels]);
                if produced == 0 {
                    break;
                }
            }
        }
        Ok(output)
    }
    fn version(&self) -> String {
        // SAFETY: SoXR returns a static, NUL-terminated version string.
        unsafe {
            CStr::from_ptr(soxr_sys::soxr_version())
                .to_string_lossy()
                .into_owned()
        }
    }
    fn delay(&self) -> f64 {
        // SAFETY: handle is valid while self is alive.
        unsafe { soxr_sys::soxr_delay(self.handle) }
    }
}
impl Drop for Soxr {
    fn drop(&mut self) {
        // SAFETY: this uniquely owned instance is released exactly once.
        unsafe {
            soxr_sys::soxr_delete(self.handle);
        }
    }
}
fn soxr_error(error: soxr_sys::soxr_error_t) -> Result<()> {
    if error.is_null() {
        Ok(())
    } else {
        // SAFETY: non-null errors returned by SoXR are static C strings.
        Err(invalid(
            unsafe { CStr::from_ptr(error) }
                .to_string_lossy()
                .into_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn concurrent_cold_start_produces_identical_audio_and_markers() {
        const CHILD: &str = "YTU_TEST_SOXR_COLD_START";
        if std::env::var_os(CHILD).is_none() {
            // A fresh process is necessary: another test may already have
            // initialized the native library's global coefficient tables.
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "remap::tests::concurrent_cold_start_produces_identical_audio_and_markers",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "cold-start regression failed:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let input: Arc<Vec<f64>> = Arc::new(
            (0..44_100)
                .flat_map(|i| {
                    let phase = TAU * i as f64 / RATE;
                    [
                        (phase * 440.0).sin(),
                        (phase * 440.0).sin() * 0.5,
                        phase.sin(),
                        phase.cos(),
                    ]
                })
                .collect(),
        );
        let barrier = Arc::new(Barrier::new(16));
        let process = |input: &[f64]| {
            let mut stream = Soxr::new(1.021, 4).unwrap();
            stream.ratio(1.0163, 0).unwrap();
            stream.chunk(input, true).unwrap()
        };
        let workers: Vec<_> = (0..16)
            .map(|_| {
                let input = input.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    process(&input)
                })
            })
            .collect();
        let outputs: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        let expected = process(&input);
        for output in outputs {
            assert_eq!(output.len(), expected.len());
            let error = output
                .iter()
                .zip(&expected)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0_f64, f64::max);
            assert_eq!(
                error, 0.0,
                "concurrent initialization altered audio/markers"
            );
        }
    }
}
