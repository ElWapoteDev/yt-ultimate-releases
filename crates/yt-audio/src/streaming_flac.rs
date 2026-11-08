use crate::{check, codec, pcm24, report_progress, Result};
use flacenc::{
    bitsink::ByteSink,
    component::{BitRepr, Frame, FrameHeader, FrameOffset, StreamInfo},
    error::Verify,
    source::{Context, Fill, FrameBuf},
};
use rodio::Source;
use std::{
    fs::File,
    io::{Seek, SeekFrom, Write},
};
use yt_media::CancellationToken;

/// Encode one FLAC frame at a time. Only STREAMINFO, MD5 state and one block
/// remain in memory; total duration no longer imposes a PCM allocation limit.
pub(crate) fn encode(
    source: &mut impl Source<Item = f32>,
    file: &mut File,
    cancel: &CancellationToken,
    progress: &impl Fn(f64),
) -> Result<u64> {
    let rate = source.sample_rate();
    let channels = source.channels() as usize;
    if rate == 0 || rate > 655350 {
        return Err(codec("Frecuencia fuera del rango de FLAC"));
    }
    let expected = source
        .total_duration()
        .map(|d| d.as_secs_f64() * rate as f64 * channels as f64);
    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|_| codec("Configuración FLAC inválida"))?;
    let block = config.block_size;
    // flacenc 0.5's StreamInfo constructor restricts rates to its 96 kHz
    // subset, while FrameHeader supports FLAC's higher standard frequencies.
    // Prediction uses PCM values only. Write the actual rate into both the
    // frame headers and final STREAMINFO when using a higher input rate.
    let mut info = StreamInfo::new((rate as usize).min(96000), channels, 24).map_err(codec)?;
    let mut data = FrameBuf::with_size(channels, block).map_err(codec)?;
    let mut context = Context::new(24, channels);
    let mut count = 0u64;
    file.write_all(&[0u8; 42])?;
    loop {
        check(cancel)?;
        let floats = source.by_ref().take(block * channels).collect::<Vec<_>>();
        if floats.is_empty() {
            break;
        }
        if !floats.len().is_multiple_of(channels) || floats.iter().any(|v| !v.is_finite()) {
            return Err(codec("La fuente contiene cuadros de audio inválidos"));
        }
        let samples = floats.into_iter().map(pcm24).collect::<Vec<_>>();
        if (count + samples.len() as u64) / channels as u64 > 0xF_FFFF_FFFF {
            return Err(codec("La duración supera el límite de cuadros de FLAC"));
        }
        data.fill_interleaved(&samples).map_err(codec)?;
        context.fill_interleaved(&samples).map_err(codec)?;
        let number = context
            .current_frame_number()
            .expect("a nonempty block was filled");
        let mut frame =
            flacenc::encode_fixed_size_frame(&config, &data, number, &info).map_err(codec)?;
        if rate > 96000 {
            let header = FrameHeader::new(
                frame.block_size(),
                frame.header().channel_assignment().clone(),
                24,
                rate as usize,
                FrameOffset::Frame(number as u32),
            )
            .map_err(codec)?;
            let subframes = (0..frame.subframe_count())
                .map(|index| frame.subframe(index).unwrap().clone())
                .collect::<Vec<_>>();
            frame = Frame::new(header, subframes.into_iter()).map_err(codec)?;
        }
        check(cancel)?;
        info.update_frame_info(&frame);
        let mut sink = ByteSink::new();
        frame.write(&mut sink).map_err(codec)?;
        file.write_all(sink.as_slice())?;
        count += samples.len() as u64;
        report_progress(count, expected, progress);
    }
    if count == 0 {
        return Err(codec("La fuente no contiene muestras de audio"));
    }
    check(cancel)?;
    info.set_block_sizes(block, block).map_err(codec)?;
    info.set_md5_digest(&context.md5_digest());
    let mut header = ByteSink::new();
    info.write(&mut header).map_err(codec)?;
    let mut bytes = header.as_slice().to_vec();
    if bytes.len() != 34 {
        return Err(codec("STREAMINFO FLAC inválido"));
    }
    let packed = u64::from_be_bytes(bytes[10..18].try_into().unwrap());
    bytes[10..18]
        .copy_from_slice(&((packed & ((1u64 << 44) - 1)) | ((rate as u64) << 44)).to_be_bytes());
    file.seek(SeekFrom::Start(0))?;
    file.write_all(b"fLaC\x80\0\0\x22")?;
    file.write_all(&bytes)?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::{buffer::SamplesBuffer, source::Zero};

    #[test]
    fn short_final_frame_and_high_rate_preserve_pcm_and_stream_md5() {
        let dir = tempfile::tempdir().unwrap();
        let values = [-8_388_608, -1, 0, 1, 8_388_607];
        let samples = (0..4101 * 2)
            .map(|index| values[index % values.len()] as f32 / 8_388_608.0)
            .collect::<Vec<_>>();
        // Independently calculated MD5 of the interleaved, signed PCM24 bytes.
        let md5 = [
            0x25, 0xe5, 0x87, 0x8b, 0xcf, 0xb0, 0x4a, 0x51, 0x47, 0x42, 0xce, 0x55, 0xb3, 0x9e,
            0x9d, 0x01,
        ];
        for rate in [44_100, 192_000, 384_000] {
            let output = dir.path().join(format!("{rate}.flac"));
            let mut file = File::create(&output).unwrap();
            let mut source = SamplesBuffer::new(2, rate, samples.clone());
            assert_eq!(
                encode(&mut source, &mut file, &CancellationToken::new(), &|_| {}).unwrap(),
                samples.len() as u64
            );
            drop(file);
            let bytes = std::fs::read(&output).unwrap();
            assert_eq!(&bytes[26..42], &md5);
            let packed = u64::from_be_bytes(bytes[18..26].try_into().unwrap());
            assert_eq!(packed >> 44, rate as u64);
            assert_eq!((packed >> 41) & 7, 1); // stereo
            assert_eq!((packed >> 36) & 31, 23); // 24-bit
            assert_eq!(packed & 0xF_FFFF_FFFF, 4101);
            let decoded = yt_media::decode_file(&output).unwrap();
            assert_eq!(decoded.sample_rate(), rate);
            assert_eq!(decoded.channels(), 2);
            assert_eq!(decoded.collect::<Vec<_>>(), samples);
        }
    }

    #[test]
    fn long_virtual_source_exceeds_the_old_512_mib_pcm_limit() {
        let frames = 67_108_881;
        let mut source = Zero::new_samples(2, 44_100, frames * 2);
        let mut file = tempfile::tempfile().unwrap();
        assert_eq!(
            encode(&mut source, &mut file, &CancellationToken::new(), &|_| {}).unwrap(),
            (frames * 2) as u64
        );
        assert!(file.metadata().unwrap().len() < 2 * 1024 * 1024);
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut header = [0u8; 42];
        std::io::Read::read_exact(&mut file, &mut header).unwrap();
        let packed = u64::from_be_bytes(header[18..26].try_into().unwrap());
        assert_eq!(packed & 0xF_FFFF_FFFF, frames as u64);
        // This covers every sample, including the final 17-frame partial block.
        assert_eq!(
            &header[26..42],
            &[
                0xda, 0x3a, 0xa5, 0x5d, 0x23, 0xfb, 0xdb, 0x8b, 0x52, 0x26, 0x2e, 0x62, 0x02, 0x55,
                0xbc, 0xe3
            ]
        );
    }
}
