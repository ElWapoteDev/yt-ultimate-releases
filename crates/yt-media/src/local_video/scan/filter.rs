//! Remove unusable WebM tracks without shifting any surviving seek offsets.
use super::*;
use crate::MuxProgress;
use std::{io::Write, ops::Range};

struct Filter<'a> {
    keep: &'a [u64],
    cancel: &'a CancellationToken,
    ranges: Vec<Range<u64>>,
}

impl Filter<'_> {
    fn remove(&mut self, item: Element) -> Result<()> {
        if self.ranges.len() >= MAX_PACKETS || item.end - item.start < 2 {
            return Err(invalid("demasiados elementos WebM descartados"));
        }
        self.ranges.push(item.start..item.end);
        Ok(())
    }

    fn children(&self, file: &mut File, parent: Element) -> Result<Vec<Element>> {
        file.seek(SeekFrom::Start(parent.data))?;
        let mut children = Vec::new();
        while file.stream_position()? < parent.end {
            check_cancel(self.cancel)?;
            let item = element(file, parent.end)?;
            if item.unknown || children.len() >= MAX_PACKETS {
                return Err(invalid("estructura WebM demasiado grande o sin tamaño"));
            }
            children.push(item);
            file.seek(SeekFrom::Start(item.end))?;
        }
        Ok(children)
    }

    fn field(&self, file: &mut File, parent: Element, id: u64) -> Result<Option<u64>> {
        for item in self.children(file, parent)? {
            if item.id == id {
                file.seek(SeekFrom::Start(item.data))?;
                return read_uint(file, item).map(Some);
            }
        }
        Ok(None)
    }

    fn tracks(&mut self, file: &mut File, parent: Element) -> Result<()> {
        let children = self.children(file, parent)?;
        let before = self.ranges.len();
        for item in children.iter().copied().filter(|item| item.id == 0xae) {
            let number = self
                .field(file, item, 0xd7)?
                .ok_or_else(|| invalid("TrackEntry sin número"))?;
            if !self.keep.contains(&number) {
                self.remove(item)?;
            }
        }
        if self.ranges.len() != before {
            self.remove_checksums(&children)?;
        }
        Ok(())
    }

    fn remove_checksums(&mut self, children: &[Element]) -> Result<()> {
        for item in children.iter().copied().filter(|item| item.id == 0xbf) {
            self.remove(item)?;
        }
        Ok(())
    }

    fn cluster(&mut self, file: &mut File, parent: Element) -> Result<()> {
        file.seek(SeekFrom::Start(parent.data))?;
        let before = self.ranges.len();
        let mut checksums = Vec::new();
        while file.stream_position()? < parent.end {
            check_cancel(self.cancel)?;
            let item = element(file, parent.end)?;
            if parent.unknown && top_level(item.id) {
                file.seek(SeekFrom::Start(item.start))?;
                break;
            }
            if item.unknown {
                return Err(invalid("elemento de Cluster sin tamaño"));
            }
            let block = match item.id {
                0xa3 => Some(item),
                0xa0 => self
                    .children(file, item)?
                    .into_iter()
                    .find(|child| child.id == 0xa1),
                0xbf => {
                    checksums.push(item);
                    None
                }
                _ => None,
            };
            if let Some(block) = block {
                file.seek(SeekFrom::Start(block.data))?;
                let (track, _, _) = vint(file, false)?;
                if !self.keep.contains(&track) {
                    // Remove the entire group, including duration, references,
                    // padding and additions belonging to its discarded track.
                    self.remove(item)?;
                }
            }
            file.seek(SeekFrom::Start(item.end))?;
        }
        let end = file.stream_position()?;
        if self.ranges.len() != before {
            self.remove_checksums(&checksums)?;
        }
        file.seek(SeekFrom::Start(end))?;
        Ok(())
    }

    fn cues(&mut self, file: &mut File, parent: Element) -> Result<()> {
        let children = self.children(file, parent)?;
        let before = self.ranges.len();
        for point in children.iter().copied().filter(|item| item.id == 0xbb) {
            let fields = self.children(file, point)?;
            let start = self.ranges.len();
            let mut retained = false;
            for position in fields.iter().copied().filter(|item| item.id == 0xb7) {
                let track = self
                    .field(file, position, 0xf7)?
                    .ok_or_else(|| invalid("CueTrackPositions sin pista"))?;
                if self.keep.contains(&track) {
                    retained = true;
                } else {
                    self.remove(position)?;
                }
            }
            if self.ranges.len() != start {
                if retained {
                    self.remove_checksums(&fields)?;
                } else {
                    self.ranges.truncate(start);
                    self.remove(point)?;
                }
            }
        }
        if self.ranges.len() != before {
            self.remove_checksums(&children)?;
        }
        Ok(())
    }
}

pub(in super::super) fn webm_redactions(
    file: &mut File,
    index: &Index,
    keep: &[u64],
    cancel: &CancellationToken,
) -> Result<Vec<Range<u64>>> {
    let mut filter = Filter {
        keep,
        cancel,
        ranges: Vec::new(),
    };
    let mut checksums = Vec::new();
    file.seek(SeekFrom::Start(index.segment_start))?;
    while file.stream_position()? < index.segment_end {
        check_cancel(cancel)?;
        let item = element(file, index.segment_end)?;
        if item.id == CLUSTER {
            filter.cluster(file, item)?;
            continue;
        }
        if item.unknown {
            return Err(invalid("elemento WebM sin tamaño"));
        }
        match item.id {
            0x1654ae6b => filter.tracks(file, item)?,
            0x1c53bb6b => filter.cues(file, item)?,
            0xbf => checksums.push(item),
            _ => {}
        }
        file.seek(SeekFrom::Start(item.end))?;
    }
    if !filter.ranges.is_empty() {
        filter.remove_checksums(&checksums)?;
    }
    filter.ranges.sort_unstable_by_key(|range| range.start);
    for pair in filter.ranges.windows(2) {
        if pair[0].end > pair[1].start {
            return Err(invalid("elementos WebM solapados"));
        }
    }
    Ok(filter.ranges)
}

pub(in super::super) fn copy_webm_segment(
    source: &mut File,
    output: &mut File,
    index: &Index,
    redactions: &[Range<u64>],
    cancel: &CancellationToken,
    on_progress: impl Fn(MuxProgress),
) -> Result<()> {
    let mut position = index.segment_start;
    let mut buffer = [0; 64 * 1024];
    // The zero-length sentinel finishes copying the final retained extent.
    for range in redactions
        .iter()
        .cloned()
        .chain(std::iter::once(index.segment_end..index.segment_end))
    {
        source.seek(SeekFrom::Start(position))?;
        while position < range.start {
            check_cancel(cancel)?;
            let count = (range.start - position).min(buffer.len() as u64) as usize;
            source.read_exact(&mut buffer[..count])?;
            output.write_all(&buffer[..count])?;
            position += count as u64;
            on_progress(MuxProgress {
                copied_bytes: position,
                total_bytes: index.segment_end,
            });
        }
        if range.is_empty() {
            continue;
        }
        let total = range.end - range.start;
        let (width, payload) = (1..=8)
            .find_map(|width| {
                let payload = total.checked_sub(1 + width)?;
                (payload < (1u64 << (7 * width)) - 1).then_some((width, payload))
            })
            .ok_or_else(|| invalid("Void WebM fuera de rango"))?;
        output.write_all(&[0xec])?;
        let encoded = payload | (1u64 << (7 * width));
        output.write_all(&encoded.to_be_bytes()[8 - width as usize..])?;
        buffer.fill(0);
        let mut remaining = payload;
        while remaining > 0 {
            check_cancel(cancel)?;
            let count = remaining.min(buffer.len() as u64) as usize;
            output.write_all(&buffer[..count])?;
            remaining -= count as u64;
            on_progress(MuxProgress {
                copied_bytes: range.end - remaining,
                total_bytes: index.segment_end,
            });
        }
        position = range.end;
    }
    Ok(())
}
