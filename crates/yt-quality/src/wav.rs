//! Bounded float WAV I/O retaining f64 precision through all DSP stages.
use crate::{invalid, QualityError, Result};
use std::{
    fs::File,
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spec {
    pub rate: u32,
    pub channels: usize,
    pub frames: u64,
}

pub struct Reader {
    file: BufReader<File>,
    pub spec: Spec,
    start: u64,
    position: u64,
    bytes: usize,
}
impl Reader {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let mut file = BufReader::new(File::open(path)?);
        let size = file.get_ref().metadata()?.len();
        let mut header = [0_u8; 12];
        file.read_exact(&mut header)?;
        if &header[..4] != b"RIFF" || &header[8..] != b"WAVE" {
            return Err(invalid("Se requiere un WAV de punto flotante."));
        }
        let mut format = None;
        let mut data = None;
        while file.stream_position()?.saturating_add(8) <= size {
            let mut chunk = [0_u8; 8];
            file.read_exact(&mut chunk)?;
            let len = u32::from_le_bytes(chunk[4..].try_into().unwrap()) as u64;
            let offset = file.stream_position()?;
            if offset.saturating_add(len) > size {
                return Err(invalid("WAV incompleto."));
            }
            if &chunk[..4] == b"fmt " {
                if len < 16 {
                    return Err(invalid("Formato WAV inválido."));
                }
                let mut fmt = [0_u8; 16];
                file.read_exact(&mut fmt)?;
                let mut tag = u16::from_le_bytes(fmt[..2].try_into().unwrap());
                let channels = u16::from_le_bytes(fmt[2..4].try_into().unwrap()) as usize;
                let rate = u32::from_le_bytes(fmt[4..8].try_into().unwrap());
                let align = u16::from_le_bytes(fmt[12..14].try_into().unwrap()) as usize;
                let bits = u16::from_le_bytes(fmt[14..16].try_into().unwrap());
                if tag == 0xfffe {
                    if len < 40 {
                        return Err(invalid("WAV_EXTENSIBLE incompleto."));
                    }
                    let mut extension = [0_u8; 24];
                    file.read_exact(&mut extension)?;
                    let float_guid = [3, 0, 0, 0, 0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113];
                    if extension[8..] != float_guid {
                        return Err(invalid("Se requiere WAV flotante para los stems."));
                    }
                    tag = 3;
                }
                if tag != 3
                    || ![32, 64].contains(&bits)
                    || !(1..=8).contains(&channels)
                    || !(8000..=384000).contains(&rate)
                    || align != channels * bits as usize / 8
                {
                    return Err(invalid("Se requiere WAV IEEE float32/64 de 1–8 canales."));
                }
                format = Some((channels, rate, bits as usize / 8));
            } else if &chunk[..4] == b"data" {
                data = Some((offset, len));
            }
            if format.is_some() && data.is_some() {
                break;
            }
            file.seek(SeekFrom::Start(offset + len + len % 2))?;
        }
        let (channels, rate, bytes) = format.ok_or_else(|| invalid("Falta formato WAV."))?;
        let (start, len) = data.ok_or_else(|| invalid("Faltan muestras WAV."))?;
        if len == 0 || len % (channels * bytes) as u64 != 0 {
            return Err(invalid("WAV vacío o incompleto."));
        }
        file.seek(SeekFrom::Start(start))?;
        Ok(Self {
            file,
            spec: Spec {
                channels,
                rate,
                frames: len / (channels * bytes) as u64,
            },
            start,
            position: 0,
            bytes,
        })
    }
    pub fn seek(&mut self, frame: u64) -> Result<()> {
        if frame > self.spec.frames {
            return Err(invalid("Posición WAV fuera de rango."));
        }
        self.file.seek(SeekFrom::Start(
            self.start + frame * (self.spec.channels * self.bytes) as u64,
        ))?;
        self.position = frame;
        Ok(())
    }
    pub fn read(&mut self, frames: usize) -> Result<Vec<f64>> {
        let frames = frames.min((self.spec.frames - self.position) as usize);
        let count = frames * self.spec.channels;
        let mut raw = vec![0_u8; count * self.bytes];
        self.file.read_exact(&mut raw)?;
        let mut output = Vec::with_capacity(count);
        for bytes in raw.chunks_exact(self.bytes) {
            let value = if self.bytes == 8 {
                f64::from_le_bytes(bytes.try_into().unwrap())
            } else {
                f32::from_le_bytes(bytes.try_into().unwrap()) as f64
            };
            if !value.is_finite() {
                return Err(invalid("El audio contiene muestras no finitas."));
            }
            output.push(value);
        }
        self.position += frames as u64;
        Ok(output)
    }
}

pub struct Writer {
    temp: tempfile::NamedTempFile,
    writer: BufWriter<File>,
    destination: PathBuf,
    spec: Spec,
    samples: u64,
    bytes: u32,
}
impl Writer {
    pub fn new(path: impl AsRef<Path>, spec: Spec) -> Result<Self> {
        Self::with_bytes(path, spec, 8)
    }
    pub fn float32(path: impl AsRef<Path>, spec: Spec) -> Result<Self> {
        Self::with_bytes(path, spec, 4)
    }
    fn with_bytes(path: impl AsRef<Path>, spec: Spec, bytes: u32) -> Result<Self> {
        let path = path.as_ref();
        if path.exists() {
            return Err(QualityError::DestinationExists);
        }
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)?;
        let temp = tempfile::Builder::new()
            .prefix(".yt-quality-")
            .suffix(".part")
            .tempfile_in(parent)?;
        let mut writer = BufWriter::new(temp.as_file().try_clone()?);
        writer.write_all(&[0_u8; 44])?;
        Ok(Self {
            temp,
            writer,
            destination: path.to_owned(),
            spec,
            samples: 0,
            bytes,
        })
    }
    pub fn write(&mut self, samples: &[f64]) -> Result<()> {
        if !samples.len().is_multiple_of(self.spec.channels)
            || samples.iter().any(|n| !n.is_finite())
        {
            return Err(invalid("Muestras WAV inválidas."));
        }
        for value in samples {
            if self.bytes == 8 {
                self.writer.write_all(&value.to_le_bytes())?;
            } else {
                if !(*value as f32).is_finite() {
                    return Err(invalid("La muestra excede el rango float32."));
                }
                self.writer.write_all(&(*value as f32).to_le_bytes())?;
            }
        }
        self.samples += samples.len() as u64;
        Ok(())
    }
    pub fn finish(mut self) -> Result<()> {
        let bytes = self
            .samples
            .checked_mul(self.bytes as u64)
            .filter(|n| *n <= u32::MAX as u64 - 36)
            .ok_or_else(|| invalid("El WAV supera 4 GiB."))? as u32;
        if bytes == 0 {
            return Err(invalid("No se puede guardar audio vacío."));
        }
        self.writer.seek(SeekFrom::Start(0))?;
        self.writer.write_all(b"RIFF")?;
        self.writer.write_all(&(bytes + 36).to_le_bytes())?;
        self.writer.write_all(b"WAVEfmt ")?;
        self.writer.write_all(&16_u32.to_le_bytes())?;
        self.writer.write_all(&3_u16.to_le_bytes())?;
        self.writer
            .write_all(&(self.spec.channels as u16).to_le_bytes())?;
        self.writer.write_all(&self.spec.rate.to_le_bytes())?;
        self.writer
            .write_all(&(self.spec.rate * self.spec.channels as u32 * self.bytes).to_le_bytes())?;
        self.writer
            .write_all(&(self.spec.channels as u16 * self.bytes as u16).to_le_bytes())?;
        self.writer
            .write_all(&(self.bytes as u16 * 8).to_le_bytes())?;
        self.writer.write_all(b"data")?;
        self.writer.write_all(&bytes.to_le_bytes())?;
        self.writer.flush()?;
        self.writer.get_ref().sync_all()?;
        drop(self.writer);
        self.temp.persist_noclobber(self.destination).map_err(|e| {
            if e.error.kind() == std::io::ErrorKind::AlreadyExists {
                QualityError::DestinationExists
            } else {
                QualityError::Io(e.error)
            }
        })?;
        Ok(())
    }
}
