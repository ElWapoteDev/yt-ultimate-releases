//! Bounded EBML indexing. Payloads remain in the source file, including laced
//! audio; unlike a generic demuxed frame, this retains lace index and padding.
mod filter;
use super::{check_cancel, invalid};
use crate::{CancellationToken, Result};
pub(super) use filter::{copy_webm_segment, webm_redactions};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
};

const SEGMENT: u64 = 0x18538067;
const CLUSTER: u64 = 0x1f43b675;
const MAX_BLOCK: u64 = 64 * 1024 * 1024;
const MAX_METADATA: usize = 8 * 1024 * 1024;
const MAX_PACKETS: usize = 2_000_000;

#[derive(Clone, Debug)]
pub(super) struct Packet {
    pub track: u64,
    pub pts_ticks: i64,
    pub offset: u64,
    pub size: usize,
    pub lace_index: u16,
    pub lace_count: u16,
    pub duration_ticks: Option<u64>,
    pub discard_padding: i64,
    pub keyframe: bool,
    pub invisible: bool,
}

pub(super) struct Index {
    pub metadata: Vec<u8>,
    pub packets: Vec<Packet>,
    pub segment_start: u64,
    pub segment_end: u64,
}

#[derive(Clone, Copy)]
struct Element {
    id: u64,
    start: u64,
    data: u64,
    end: u64,
    unknown: bool,
}

pub(super) fn index(file: &mut File, cancel: &CancellationToken) -> Result<Index> {
    let length = file.metadata()?.len();
    let header = element(file, length)?;
    if header.id != 0x1a45dfa3 || header.unknown || header.end > 4096 {
        return Err(invalid("cabecera EBML no válida"));
    }
    let mut metadata = read_element(file, header)?;
    let mut segment = element(file, length)?;
    while matches!(segment.id, 0xec | 0xbf) {
        file.seek(SeekFrom::Start(segment.end))?;
        segment = element(file, length)?;
    }
    if segment.id != SEGMENT {
        return Err(invalid("falta el segmento Matroska"));
    }
    // All original seek offsets are deliberately excluded from the metadata
    // view; its maximum length also bounds allocations inside the metadata crate.
    metadata.extend_from_slice(&[0x18, 0x53, 0x80, 0x67, 0xff]);
    let mut packets = Vec::new();
    let mut info = false;
    let mut tracks = false;
    while file.stream_position()? < segment.end {
        check_cancel(cancel)?;
        let item = element(file, segment.end)?;
        match item.id {
            CLUSTER => scan_cluster(file, item, &mut packets, cancel)?,
            0x1549a966 | 0x1654ae6b => {
                if item.unknown
                    || item.end - item.start > MAX_METADATA as u64
                    || metadata.len() + (item.end - item.start) as usize > MAX_METADATA
                {
                    return Err(invalid("metadatos Matroska demasiado grandes"));
                }
                if item.id == 0x1549a966 {
                    if info {
                        return Err(invalid("Info duplicado"));
                    }
                    info = true;
                } else {
                    if tracks {
                        return Err(invalid("Tracks duplicado"));
                    }
                    tracks = true;
                }
                if item.id == 0x1654ae6b {
                    validate_track_times(file, item)?;
                }
                metadata.extend(read_element(file, item)?);
            }
            SEGMENT => {
                return Err(invalid(
                    "los segmentos enlazados necesitan otro reproductor",
                ));
            }
            _ => {
                if item.unknown {
                    return Err(invalid("elemento desconocido sin tamaño"));
                }
                file.seek(SeekFrom::Start(item.end))?;
            }
        }
    }
    if !info || !tracks || packets.is_empty() {
        return Err(invalid("faltan pistas, tiempos o contenido multimedia"));
    }
    metadata.extend_from_slice(&[0x1f, 0x43, 0xb6, 0x75, 0x80]);
    Ok(Index {
        metadata,
        packets,
        segment_start: segment.data,
        segment_end: segment.end,
    })
}

fn scan_cluster(
    file: &mut File,
    cluster: Element,
    packets: &mut Vec<Packet>,
    cancel: &CancellationToken,
) -> Result<()> {
    let mut timestamp = None;
    while file.stream_position()? < cluster.end {
        check_cancel(cancel)?;
        let item = element(file, cluster.end)?;
        if cluster.unknown && top_level(item.id) {
            file.seek(SeekFrom::Start(item.start))?;
            break;
        }
        if item.unknown {
            return Err(invalid("elemento de Cluster sin tamaño"));
        }
        match item.id {
            0xe7 => timestamp = Some(read_uint(file, item)?),
            0xa3 => block(
                file,
                item,
                timestamp.ok_or_else(|| invalid("Cluster sin Timestamp"))?,
                true,
                None,
                0,
                packets,
            )?,
            0xa0 => {
                let mut payload = None;
                let mut duration = None;
                let mut padding = 0;
                let mut reference = false;
                let mut fields = 0;
                while file.stream_position()? < item.end {
                    fields += 1;
                    if fields > 1024 {
                        return Err(invalid("demasiados campos de bloque"));
                    }
                    let child = element(file, item.end)?;
                    if child.unknown {
                        return Err(invalid("campo de bloque sin tamaño"));
                    }
                    match child.id {
                        0xa1 => {
                            if payload.is_some() {
                                return Err(invalid("Block duplicado"));
                            }
                            payload = Some(child);
                        }
                        0x9b => duration = Some(read_uint(file, child)?),
                        0x75a2 => padding = read_int(file, child)?,
                        0xfb => reference = true,
                        0xa4 | 0x75a1 => {
                            return Err(invalid(
                                "el vídeo cambia su configuración o contiene datos adicionales",
                            ));
                        }
                        _ => {}
                    }
                    file.seek(SeekFrom::Start(child.end))?;
                }
                let payload = payload.ok_or_else(|| invalid("BlockGroup vacío"))?;
                block(
                    file,
                    payload,
                    timestamp.ok_or_else(|| invalid("Cluster sin Timestamp"))?,
                    !reference,
                    duration,
                    padding,
                    packets,
                )?;
                file.seek(SeekFrom::Start(item.end))?;
            }
            _ => {
                file.seek(SeekFrom::Start(item.end))?;
            }
        }
    }
    Ok(())
}

fn block(
    file: &mut File,
    item: Element,
    cluster_ts: u64,
    keyframe: bool,
    duration: Option<u64>,
    padding: i64,
    packets: &mut Vec<Packet>,
) -> Result<()> {
    if item.end - item.data > MAX_BLOCK {
        return Err(invalid("un bloque Matroska supera 64 MiB"));
    }
    file.seek(SeekFrom::Start(item.data))?;
    let (track, _, _) = vint(file, false)?;
    if track == 0 {
        return Err(invalid("número de pista inválido"));
    }
    let mut header = [0; 3];
    file.read_exact(&mut header)?;
    let pts = i128::from(cluster_ts) + i128::from(i16::from_be_bytes([header[0], header[1]]));
    let pts = i64::try_from(pts).map_err(|_| invalid("timestamp fuera de rango"))?;
    let flags = header[2];
    let lacing = (flags >> 1) & 3;
    let mut sizes = Vec::new();
    if lacing == 0 {
        sizes.push(
            item.end
                .checked_sub(file.stream_position()?)
                .ok_or_else(|| invalid("bloque truncado"))?,
        );
    } else {
        let count = usize::from(read_byte(file)?) + 1;
        if count < 2 {
            return Err(invalid("lacing con un solo frame"));
        }
        match lacing {
            1 => {
                for _ in 0..count - 1 {
                    let mut size = 0u64;
                    loop {
                        if file.stream_position()? >= item.end {
                            return Err(invalid("lacing truncado"));
                        }
                        let byte = read_byte(file)?;
                        size = size
                            .checked_add(u64::from(byte))
                            .ok_or_else(|| invalid("lacing fuera de rango"))?;
                        if size > MAX_BLOCK {
                            return Err(invalid("lacing demasiado grande"));
                        }
                        if byte != 255 {
                            break;
                        }
                    }
                    sizes.push(size);
                }
            }
            2 => {
                let remaining = item
                    .end
                    .checked_sub(file.stream_position()?)
                    .ok_or_else(|| invalid("lacing truncado"))?;
                if remaining % count as u64 != 0 {
                    return Err(invalid("lacing fijo inconsistente"));
                }
                sizes.resize(count - 1, remaining / count as u64);
            }
            3 => {
                let (first, _, _) = vint(file, false)?;
                sizes.push(first);
                for _ in 1..count - 1 {
                    let (delta, bytes, _) = vint(file, false)?;
                    let bias = (1i128 << (7 * bytes - 1)) - 1;
                    let size = i128::from(*sizes.last().unwrap()) + i128::from(delta) - bias;
                    sizes.push(u64::try_from(size).map_err(|_| invalid("lacing negativo"))?);
                }
            }
            _ => unreachable!(),
        }
        let remaining = item
            .end
            .checked_sub(file.stream_position()?)
            .ok_or_else(|| invalid("lacing truncado"))?;
        let known = sizes
            .iter()
            .try_fold(0u64, |sum, size| sum.checked_add(*size))
            .ok_or_else(|| invalid("lacing fuera de rango"))?;
        sizes.push(
            remaining
                .checked_sub(known)
                .ok_or_else(|| invalid("lacing excede su bloque"))?,
        );
    }
    let count = sizes.len() as u16;
    let mut offset = file.stream_position()?;
    for (index, size) in sizes.into_iter().enumerate() {
        if size == 0 || size > MAX_BLOCK || packets.len() >= MAX_PACKETS {
            return Err(invalid("frame vacío o límite de tamaño/cantidad excedido"));
        }
        packets.push(Packet {
            track,
            pts_ticks: pts,
            offset,
            size: size as usize,
            lace_index: index as u16,
            lace_count: count,
            duration_ticks: duration,
            discard_padding: if (padding >= 0 && index + 1 == usize::from(count))
                || (padding < 0 && index == 0)
            {
                padding
            } else {
                0
            },
            keyframe: if item.id == 0xa3 {
                flags & 0x80 != 0
            } else {
                keyframe
            },
            invisible: flags & 8 != 0,
        });
        offset = offset
            .checked_add(size)
            .ok_or_else(|| invalid("offset fuera de rango"))?;
    }
    if offset != item.end {
        return Err(invalid("longitud de bloque inconsistente"));
    }
    file.seek(SeekFrom::Start(item.end))?;
    Ok(())
}

fn top_level(id: u64) -> bool {
    matches!(
        id,
        0x114d9b74
            | 0x1549a966
            | CLUSTER
            | 0x1654ae6b
            | 0x1c53bb6b
            | 0x1941a469
            | 0x1043a770
            | 0x1254c367
            | SEGMENT
    )
}
fn element(file: &mut File, parent_end: u64) -> Result<Element> {
    let start = file.stream_position()?;
    let (id, bytes, _) = vint(file, true)?;
    if bytes > 4 {
        return Err(invalid("identificador EBML demasiado largo"));
    }
    let (size, _, unknown) = vint(file, false)?;
    let data = file.stream_position()?;
    if data > parent_end {
        return Err(invalid("cabecera EBML fuera de su elemento"));
    }
    let end = if unknown {
        parent_end
    } else {
        data.checked_add(size)
            .filter(|end| *end <= parent_end)
            .ok_or_else(|| invalid("elemento EBML truncado"))?
    };
    Ok(Element {
        id,
        start,
        data,
        end,
        unknown,
    })
}

fn validate_track_times(file: &mut File, tracks: Element) -> Result<()> {
    file.seek(SeekFrom::Start(tracks.data))?;
    while file.stream_position()? < tracks.end {
        let entry = element(file, tracks.end)?;
        if entry.unknown {
            return Err(invalid("TrackEntry sin tamaño"));
        }
        if entry.id == 0xae {
            while file.stream_position()? < entry.end {
                let field = element(file, entry.end)?;
                if field.unknown {
                    return Err(invalid("campo de pista sin tamaño"));
                }
                if field.id == 0x23314f {
                    let mut bytes = [0; 8];
                    let size = field.end - field.data;
                    let value = match size {
                        4 => {
                            file.read_exact(&mut bytes[..4])?;
                            f64::from(f32::from_be_bytes(bytes[..4].try_into().unwrap()))
                        }
                        8 => {
                            file.read_exact(&mut bytes)?;
                            f64::from_be_bytes(bytes)
                        }
                        _ => return Err(invalid("TrackTimestampScale inválido")),
                    };
                    if value != 1.0 {
                        return Err(invalid(
                            "TrackTimestampScale distinto de 1 todavía no compatible",
                        ));
                    }
                }
                if field.id == 0x537f && read_int(file, field)? != 0 {
                    return Err(invalid("TrackOffset todavía no compatible"));
                }
                file.seek(SeekFrom::Start(field.end))?;
            }
        }
        file.seek(SeekFrom::Start(entry.end))?;
    }
    Ok(())
}
fn vint(file: &mut File, id: bool) -> Result<(u64, usize, bool)> {
    let first = read_byte(file)?;
    let width = first.leading_zeros() as usize + 1;
    if width > 8 {
        return Err(invalid("entero EBML inválido"));
    }
    let marker = 1u8 << (8 - width);
    let mut value = u64::from(if id { first } else { first & !marker });
    for _ in 1..width {
        value = (value << 8) | u64::from(read_byte(file)?);
    }
    let unknown = !id && value == ((1u64 << (width * 7)) - 1);
    Ok((value, width, unknown))
}
fn read_byte(file: &mut File) -> Result<u8> {
    let mut byte = [0];
    file.read_exact(&mut byte)?;
    Ok(byte[0])
}
fn read_uint(file: &mut File, item: Element) -> Result<u64> {
    let size = item.end - item.data;
    if size == 0 || size > 8 {
        return Err(invalid("entero Matroska inválido"));
    }
    let mut bytes = [0; 8];
    file.read_exact(&mut bytes[8 - size as usize..])?;
    Ok(u64::from_be_bytes(bytes))
}
fn read_int(file: &mut File, item: Element) -> Result<i64> {
    let size = item.end - item.data;
    if size == 0 || size > 8 {
        return Err(invalid("entero Matroska inválido"));
    }
    let mut bytes = [0; 8];
    file.read_exact(&mut bytes[8 - size as usize..])?;
    let shift = 64 - size * 8;
    Ok(((u64::from_be_bytes(bytes) << shift) as i64) >> shift)
}
fn read_element(file: &mut File, item: Element) -> Result<Vec<u8>> {
    file.seek(SeekFrom::Start(item.start))?;
    let mut bytes = vec![0; (item.end - item.start) as usize];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}
