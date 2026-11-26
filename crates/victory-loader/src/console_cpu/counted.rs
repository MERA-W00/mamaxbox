//! Proves the trip count before combining adjacent cache-line writes.

use super::decode::{Reader, reg, rm};

#[derive(Clone, Copy)]
enum Bound {
    Register(u8),
    Immediate(u64),
}

#[derive(Clone, Copy)]
enum Continue {
    Below,
    AtMost,
    Different,
    Equal,
}

pub(super) struct Counted {
    wide: bool,
    bound: Bound,
    reversed: bool,
    signed: bool,
    condition: Continue,
    pub end: usize,
}

fn opcode(r: &mut Reader<'_>) -> Option<(u8, u8)> {
    let first = r.byte()?;
    Some(if (0x40..=0x4f).contains(&first) {
        (first, r.byte()?)
    } else {
        (0, first)
    })
}

/// Only RAX or EAX can advance the implicit address used by CLZERO. A memory
/// bound could change during the writes, so it must run one iteration at a time.
pub(super) fn recognize(code: &[u8], head: usize) -> Option<Counted> {
    let mut r = Reader::new(code.get(head..)?);
    let (rex, op) = opcode(&mut r)?;
    let wide = rex & 8 != 0;
    let stride = match op {
        0x05 => r.dword()?,
        0x81 | 0x83 if rex & 1 == 0 && r.byte()? == 0xc0 => {
            if op == 0x83 {
                r.byte()? as i8 as i32
            } else {
                r.dword()?
            }
        }
        0x8d => {
            let m = r.byte()?;
            if reg(m, rex) != 0 || rm(m, rex) != 0 {
                return None;
            }
            match m >> 6 {
                1 => r.byte()? as i8 as i32,
                2 => r.dword()?,
                _ => return None,
            }
        }
        _ => return None,
    };
    if stride != 64 {
        return None;
    }
    let cmp = head + r.pos;
    let mut r = Reader::new(code.get(cmp..)?);
    let (rex, op) = opcode(&mut r)?;
    if (rex & 8 != 0) != wide {
        return None;
    }
    let (bound, reversed) = match op {
        0x39 | 0x3b => {
            let m = r.byte()?;
            if m < 0xc0 {
                return None;
            }
            let (a, b) = (rm(m, rex), reg(m, rex));
            let (lhs, rhs) = if op == 0x39 { (a, b) } else { (b, a) };
            match (lhs, rhs) {
                (0, 1..=15) => (Bound::Register(rhs), false),
                (1..=15, 0) => (Bound::Register(lhs), true),
                _ => return None,
            }
        }
        0x3d => (Bound::Immediate(r.dword()? as i64 as u64), false),
        0x81 | 0x83 if rex & 1 == 0 && r.byte()? == 0xf8 => {
            let imm = if op == 0x83 {
                r.byte()? as i8 as i32
            } else {
                r.dword()?
            };
            (Bound::Immediate(imm as i64 as u64), false)
        }
        _ => return None,
    };
    let jump = cmp + r.pos;
    let mut r = Reader::new(code.get(jump..)?);
    let (cc, displacement) = match r.byte()? {
        p @ 0x70..=0x7f => (p & 15, r.byte()? as i8 as i32),
        0x0f => {
            let p = r.byte()?;
            if !(0x80..=0x8f).contains(&p) {
                return None;
            }
            (p & 15, r.dword()?)
        }
        _ => return None,
    };
    let end = jump + r.pos;
    if i64::from(displacement) != -(end as i64) {
        return None;
    }
    let condition = match (cc, reversed) {
        (2 | 12, false) | (7 | 15, true) => Continue::Below,
        (6 | 14, false) | (3 | 13, true) => Continue::AtMost,
        (5, _) => Continue::Different,
        (4, _) => Continue::Equal,
        _ => return None,
    };
    Some(Counted {
        wide,
        bound,
        reversed,
        signed: cc >= 12,
        condition,
        end,
    })
}

pub(super) const STATUS: u32 = 0x8d5;

/// CMP replaces six flags. In particular AF and OF depend on the operand
/// order even when the branch only tests equality.
fn flags(lhs: u64, rhs: u64, mask: u64) -> u32 {
    let difference = lhs.wrapping_sub(rhs) & mask;
    let sign = mask ^ (mask >> 1);
    u32::from(lhs < rhs)
        | (u32::from((difference as u8).count_ones() % 2 == 0) << 2)
        | ((lhs ^ rhs ^ difference) as u32 & 16)
        | (u32::from(difference == 0) << 6)
        | (u32::from(difference & sign != 0) << 7)
        | (u32::from((lhs ^ rhs) & (lhs ^ difference) & sign != 0) << 11)
}

pub(super) struct Exit {
    pub start: u64,
    pub bytes: u64,
    cursor: u64,
    limit: u64,
    mask: u64,
    reversed: bool,
}

impl Exit {
    /// Recovers the state at a completed comparison if a mapping changes
    /// partway through a bulk write. The caller supplies at most `self.bytes`.
    pub fn after(&self, written: u64) -> (u64, u32) {
        let address = self.cursor + written;
        let (lhs, rhs) = if self.reversed {
            (self.limit, address)
        } else {
            (address, self.limit)
        };
        (address, flags(lhs, rhs, self.mask))
    }
}

impl Counted {
    pub fn plan(
        &self,
        initial: u64,
        address32: bool,
        read: impl FnOnce(u8) -> u64,
    ) -> Option<Exit> {
        let mask = if self.wide {
            u64::MAX
        } else {
            u64::from(u32::MAX)
        };
        let addr_mask = if address32 {
            u64::from(u32::MAX)
        } else {
            u64::MAX
        };
        let cursor = initial & mask;
        // A 32-bit ADD zero-extends its result. If the first line was in
        // another 4 GiB window, the writes need not be contiguous.
        if initial & addr_mask != cursor || cursor > addr_mask {
            return None;
        }
        let limit = match self.bound {
            Bound::Register(i) => read(i),
            Bound::Immediate(i) => i,
        } & mask;
        let first = cursor.checked_add(64)?;
        let bias = if self.signed { mask ^ (mask >> 1) } else { 0 };
        let (next, stop) = (first ^ bias, limit ^ bias);
        let extra = match self.condition {
            Continue::Below if next < stop => (stop - next).div_ceil(64),
            Continue::AtMost if next <= stop => (stop - next) / 64 + 1,
            Continue::Different if next != stop => {
                let distance = stop.checked_sub(next)?;
                if distance % 64 != 0 {
                    return None;
                }
                distance / 64
            }
            Continue::Equal if next == stop => 1,
            _ => 0,
        };
        let bytes = extra.checked_add(1)?.checked_mul(64)?;
        let address = cursor.checked_add(bytes)?;
        if address > mask.min(addr_mask) || (first ^ address) & bias != 0 {
            return None;
        }
        Some(Exit {
            start: cursor & !63,
            bytes,
            cursor,
            limit,
            mask,
            reversed: self.reversed,
        })
    }
}
