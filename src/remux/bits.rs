//! Big-endian bit parsing for audio headers and video parameter sets.

pub(super) struct Bits {
    pub(super) d: Vec<u8>,
    pub(super) at: usize,
}

impl Bits {
    /// RBSP of a NAL (emulation-prevention bytes removed), after the header byte.
    pub(super) fn new(nal: &[u8]) -> Bits {
        let mut d = Vec::with_capacity(nal.len());
        for (k, b) in nal.iter().enumerate().skip(1) {
            if *b == 3 && k >= 3 && nal[k - 1] == 0 && nal[k - 2] == 0 && d.len() >= 2 && d[d.len() - 1] == 0 && d[d.len() - 2] == 0 {
                continue;
            }
            d.push(*b);
        }
        Bits { d, at: 0 }
    }
    pub(super) fn bit(&mut self) -> u32 {
        let b = self.d.get(self.at / 8).map_or(0, |b| (b >> (7 - self.at % 8)) & 1);
        self.at += 1;
        b as u32
    }
    pub(super) fn bits(&mut self, n: u32) -> u32 {
        (0..n).fold(0, |v, _| v << 1 | self.bit())
    }
    pub(super) fn ue(&mut self) -> u32 {
        let mut z = 0;
        while self.bit() == 0 && z < 32 {
            z += 1;
        }
        ((1u64 << z) - 1 + self.bits(z) as u64) as u32
    }
    pub(super) fn se(&mut self) -> i32 {
        let v = self.ue() as i64;
        (if v % 2 == 1 { (v + 1) / 2 } else { -(v / 2) }) as i32
    }
}
