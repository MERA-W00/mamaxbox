//! Decodes the console instructions that can raise an illegal-instruction exception.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Op {
    Zero {
        address32: bool,
    },
    WriteBack,
    ReadCounter,
    Monitor,
    Wait,
    ProcessorId(u8),
    Field {
        insert: bool,
        dst: u8,
        src: u8,
        immediate: Option<[u8; 2]>,
    },
}

/// Each instruction has its own 15-byte budget, including prefixes and operands.
pub(super) struct Reader<'a> {
    bytes: &'a [u8],
    pub pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes: &bytes[..bytes.len().min(15)],
            pos: 0,
        }
    }

    pub fn byte(&mut self) -> Option<u8> {
        let byte = *self.bytes.get(self.pos)?;
        self.pos += 1;
        Some(byte)
    }

    pub fn dword(&mut self) -> Option<i32> {
        let bytes = self.bytes.get(self.pos..self.pos + 4)?;
        self.pos += 4;
        Some(i32::from_le_bytes(bytes.try_into().ok()?))
    }
}

pub(super) fn reg(m: u8, rex: u8) -> u8 {
    ((m >> 3) & 7) | ((rex & 4) << 1)
}

pub(super) fn rm(m: u8, rex: u8) -> u8 {
    (m & 7) | ((rex & 1) << 3)
}

pub(super) fn decode(bytes: &[u8]) -> Option<(Op, usize)> {
    let mut r = Reader::new(bytes);
    let (mut size, mut repeat, mut address32, mut rex) = (false, 0, false, 0);
    loop {
        match r.byte()? {
            0x0f => break,
            0x66 => size = true,
            p @ (0xf2 | 0xf3) => repeat = p,
            0x67 => address32 = true,
            0x26 | 0x2e | 0x36 | 0x3e | 0x64 | 0x65 => {}
            p @ 0x40..=0x4f => {
                rex = p;
                continue;
            }
            _ => return None,
        }
        // Only a REX immediately before the opcode extends the register fields.
        rex = 0;
    }
    let opcode = r.byte()?;
    let m = r.byte()?;
    let ext = (m >> 3) & 7;
    let mode = m >> 6;
    let op = match (size, repeat, opcode) {
        (false, 0, 0x01) => match m {
            0xfa => Op::Monitor,
            0xfb => Op::Wait,
            0xfc => Op::Zero { address32 },
            0xfd => Op::ReadCounter,
            _ => return None,
        },
        (true, 0, 0xae) if mode != 3 && ext == 6 => {
            let base = m & 7;
            let has_sib = base == 4;
            let address_base = if has_sib { r.byte()? & 7 } else { base };
            if mode == 2 || (mode == 0 && address_base == 5) {
                r.dword()?;
            } else if mode == 1 {
                r.byte()?;
            }
            Op::WriteBack
        }
        (false, 0xf3, 0xc7) if ext == 7 && mode == 3 => Op::ProcessorId(rm(m, rex)),
        (true, 0, 0x78 | 0x79) | (false, 0xf2, 0x78 | 0x79) if mode == 3 => {
            let insert = repeat == 0xf2;
            let immediate = if opcode == 0x78 {
                if !insert && ext != 0 {
                    return None;
                }
                Some([r.byte()?, r.byte()?])
            } else {
                None
            };
            let src = rm(m, rex);
            let dst = if !insert && immediate.is_some() {
                src
            } else {
                reg(m, rex)
            };
            Op::Field {
                insert,
                dst,
                src,
                immediate,
            }
        }
        _ => return None,
    };
    Some((op, r.pos))
}
