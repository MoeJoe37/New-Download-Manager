use super::webm::{ebml_id, vint, CLUSTER, SEGMENT, TRACKS};

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Mp4,
    Webm,
}

/// Downloaded pieces re-cut into whole container units, whatever the piece boundaries were (a single
/// file fetched in byte ranges): MP4 up to each `moov` / `mdat`, WebM up to each `Tracks` / `Cluster`.
pub struct Regroup<'a> {
    inner: Box<dyn Iterator<Item = Result<Vec<u8>, String>> + 'a>,
    kind: Kind,
    buf: Vec<u8>,
    pos: usize,
    eof: bool,
}

impl<'a> Regroup<'a> {
    pub fn new(
        inner: impl Iterator<Item = Result<Vec<u8>, String>> + 'a,
        kind: Kind,
    ) -> Regroup<'a> {
        Regroup {
            inner: Box::new(inner),
            kind,
            buf: Vec::new(),
            pos: 0,
            eof: false,
        }
    }

    /// Ok(Some(end, last)) when a whole element starts at `pos`; None = more data needed.
    fn element(&self) -> Option<(usize, bool)> {
        let (d, at) = (&self.buf, self.pos);
        match self.kind {
            Kind::Mp4 => {
                let size = u32::from_be_bytes(d.get(at..at + 4)?.try_into().ok()?) as u64;
                let t = d.get(at + 4..at + 8)?;
                let size = match size {
                    1 => u64::from_be_bytes(d.get(at + 8..at + 16)?.try_into().ok()?),
                    0 => return None, // to the end of the file
                    s => s,
                };
                let end = at.checked_add(size.max(8) as usize)?;
                (end <= d.len()).then_some((end, t == b"moov" || t == b"mdat"))
            }
            Kind::Webm => {
                let (id, il) = ebml_id(d, at)?;
                let (size, sl, unknown) = vint(d, at + il)?;
                if id == SEGMENT {
                    return Some((at + il + sl, false)); // step inside
                }
                if unknown {
                    if id != CLUSTER {
                        return None; // other open-ended elements run to the end of the data
                    }
                    // a live cluster without a size ends where an element that can't be inside it starts
                    let mut e = at + il + sl;
                    loop {
                        let (cid, cl) = ebml_id(d, e)?;
                        if ![0xe7, 0xa3, 0xa0, 0xa7, 0xab].contains(&cid) {
                            return Some((e, true));
                        }
                        let (size, l, _) = vint(d, e + cl)?;
                        e = (e + cl + l)
                            .checked_add(size as usize)
                            .filter(|end| *end <= d.len())?;
                    }
                }
                let end = (at + il + sl).checked_add(size as usize)?;
                (end <= d.len()).then_some((end, id == TRACKS || id == CLUSTER))
            }
        }
    }
}

impl Iterator for Regroup<'_> {
    type Item = Result<Vec<u8>, String>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            while let Some((end, last)) = self.element() {
                self.pos = end;
                if last {
                    let unit: Vec<u8> = self.buf.drain(..end).collect();
                    self.pos = 0;
                    return Some(Ok(unit));
                }
            }
            if self.eof {
                self.pos = 0;
                return (!self.buf.is_empty()).then(|| Ok(std::mem::take(&mut self.buf)));
            }
            match self.inner.next() {
                Some(Ok(p)) => self.buf.extend_from_slice(&p),
                Some(Err(e)) => return Some(Err(e)),
                None => self.eof = true,
            }
        }
    }
}
