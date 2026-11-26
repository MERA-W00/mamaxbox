//! Original implementation written from the AMD64 Architecture Programmer's
//! Manual and the supplied console instruction specification.
//!
//! A title can clear consecutive cache lines in a counted loop. On a host
//! without CLZERO each line costs an exception unless the handler can prove
//! where the loop stops and which flags its final comparison produces.

mod counted;
mod decode;
mod memory;
#[cfg(test)]
mod tests;

use crate::crash::{advance_writebacks, gpr};
use decode::Op;
use std::sync::{
    OnceLock,
    atomic::{AtomicU64, Ordering::Relaxed},
};
use windows_sys::Win32::System::Diagnostics::Debug::{CONTEXT, M128A};

#[derive(Clone, Copy)]
struct Options {
    bulk: bool,
    ask: bool,
    stats: bool,
}

const DEFAULT: Options = Options {
    bulk: true,
    ask: false,
    stats: true,
};
static OPTIONS: OnceLock<Options> = OnceLock::new();

/// Read environment switches before installing the handler. An exception can
/// interrupt a thread while it holds a process environment lock.
pub(super) fn configure() {
    OPTIONS.get_or_init(|| {
        let switch = |name, default| match std::env::var(name).as_deref() {
            Ok("0") => false,
            Ok("1") => true,
            _ => default,
        };
        Options {
            bulk: switch("VICTORY_CLZERO_BULK", DEFAULT.bulk),
            ask: switch("VICTORY_CLZERO_ASK", DEFAULT.ask),
            stats: switch("VICTORY_CPU_INSN_STATS", DEFAULT.stats),
        }
    });
}

pub(super) fn read_code(at: usize, out: &mut [u8]) -> usize {
    memory::fetch(at, out, OPTIONS.get().unwrap_or(&DEFAULT).ask)
}

#[cfg(test)]
pub(super) fn clwb_len(code: [u8; 15]) -> Option<usize> {
    match decode::decode(&code)? {
        (Op::WriteBack, len) => Some(len),
        _ => None,
    }
}

/// Both control bytes use six bits. For the architecturally undefined crossing
/// case we truncate at bit 63. The undefined high quadword keeps its old value.
fn field(xmm: &mut [M128A; 16], insert: bool, dst: u8, src: u8, immediate: Option<[u8; 2]>) {
    let source = xmm[usize::from(src)];
    let target = &mut xmm[usize::from(dst)];
    let control = if insert {
        source.High as u64
    } else {
        source.Low
    };
    let [len, offset] = immediate.unwrap_or([control as u8, (control >> 8) as u8]);
    let index = offset & 63;
    let width = match len & 63 {
        0 => 64,
        n => n,
    };
    let mask = u64::MAX >> (64 - width);
    target.Low = if insert {
        (target.Low & !(mask << index)) | ((source.Low & mask) << index)
    } else {
        (target.Low >> index) & mask
    };
}

/// Windows user mode cannot sample the two privileged performance counters.
/// Model constant-frequency execution by advancing both at the TSC rate.
fn read_counter(ctx: &mut CONTEXT) {
    let supported = ctx.Rcx as u32 <= 1;
    let value = if supported {
        unsafe { std::arch::x86_64::_rdtsc() }
    } else {
        0
    };
    ctx.Rax = u64::from(value as u32);
    ctx.Rdx = value >> 32;
    ctx.EFlags = (ctx.EFlags & !1) | u32::from(!supported);
}

#[repr(C)]
#[derive(Default)]
struct ProcessorNumber {
    group: u16,
    number: u8,
    reserved: u8,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentProcessorNumberEx(out: *mut ProcessorNumber);
    fn GetTickCount64() -> u64;
}

fn processor_id() -> u64 {
    use std::arch::x86_64::{__cpuid, __rdtscp};
    if __cpuid(0x8000_0000).eax >= 0x8000_0001 && __cpuid(0x8000_0001).edx & (1 << 27) != 0 {
        // RDTSCP supplies the same OS-assigned TSC_AUX value, including any
        // topology bits that the host puts above the local processor number.
        let mut aux = 0;
        unsafe { __rdtscp(&mut aux) };
        return u64::from(aux);
    }
    let mut cpu = ProcessorNumber::default();
    unsafe { GetCurrentProcessorNumberEx(&mut cpu) };
    u64::from(cpu.group) * 64 + u64::from(cpu.number)
}

#[repr(usize)]
enum Kind {
    Clzero,
    Clwb,
    Rdpru,
    Monitorx,
    Mwaitx,
    Rdpid,
    Extrq,
    Insertq,
}

static COUNTS: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
static TRAPS: AtomicU64 = AtomicU64::new(0);
static LOOPS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
static NEXT_REPORT: AtomicU64 = AtomicU64::new(0);

fn count(kind: Kind, instructions: u64, bulk: u64) {
    COUNTS[kind as usize].fetch_add(instructions, Relaxed);
    if bulk != 0 {
        LOOPS.fetch_add(1, Relaxed);
        BYTES.fetch_add(bulk, Relaxed);
    }
    let traps = TRAPS.fetch_add(1, Relaxed) + 1;
    let now = unsafe { GetTickCount64() };
    let next = NEXT_REPORT.load(Relaxed);
    if now < next
        || NEXT_REPORT
            .compare_exchange(next, now + 5000, Relaxed, Relaxed)
            .is_err()
    {
        return;
    }
    let [z, w, r, m, h, p, e, i] = COUNTS.each_ref().map(|v| v.load(Relaxed));
    use std::io::Write;
    let _ = writeln!(
        std::io::stderr(),
        "console_cpu traps={traps} clzero={z} clwb={w} rdpru={r} monitorx={m} mwaitx={h} rdpid={p} extrq={e} insertq={i} bulk_loops={} bulk_bytes={}",
        LOOPS.load(Relaxed),
        BYTES.load(Relaxed)
    );
}

/// Overlap with the loop itself means a write could change the next instruction.
/// A complete copy resumes after the branch. A short copy resumes inside the loop.
unsafe fn bulk_zero(
    ctx: &mut CONTEXT,
    code: &[u8],
    head: usize,
    address32: bool,
    ask: bool,
) -> Option<(usize, u64)> {
    let body = counted::recognize(code, head)?;
    let exit = body.plan(ctx.Rax, address32, |i| *gpr(ctx, i))?;
    let after = ctx.Rip.checked_add(body.end as u64)?;
    if exit.start < after && ctx.Rip < exit.start.checked_add(exit.bytes)? {
        return None;
    }
    let written =
        unsafe { memory::zero(exit.start as usize, exit.bytes as usize, ask) } as u64 & !63;
    if written == 0 {
        return None;
    }
    // If protection changed during the copy, let the guest execute the ADD
    // after the last completed CLZERO and encounter the new protection itself.
    let (end, advanced) = if written == exit.bytes {
        (body.end, written)
    } else {
        (head, written - 64)
    };
    if advanced != 0 {
        let (address, flags) = exit.after(advanced);
        ctx.Rax = address;
        ctx.EFlags = (ctx.EFlags & !counted::STATUS) | flags;
    }
    Some((end, written))
}

/// Handles an illegal instruction at the saved RIP.
///
/// # Safety
/// The context must belong to the faulting thread. Guest writes must be valid
/// under the same conditions as executing the original guest instruction.
pub(super) unsafe fn emulate(ctx: &mut CONTEXT) -> i32 {
    unsafe { emulate_with(ctx, *OPTIONS.get().unwrap_or(&DEFAULT)) }
}

unsafe fn emulate_with(ctx: &mut CONTEXT, options: Options) -> i32 {
    // One instruction plus the increment, comparison and conditional branch.
    let mut buffer = [0; 4 * 15];
    let size = memory::fetch(ctx.Rip as usize, &mut buffer, options.ask);
    let code = &buffer[..size];
    let Some((op, mut len)) = decode::decode(code) else {
        return 0;
    };
    let can_skip = ctx.EFlags & 0x100 == 0 && ctx.Dr7 & 0xff == 0;
    let (mut instructions, mut bulk) = (1, 0);
    let kind = match op {
        Op::Zero { address32 } => {
            if options.bulk
                && can_skip
                && let Some((end, bytes)) =
                    (unsafe { bulk_zero(ctx, code, len, address32, options.ask) })
            {
                len = end;
                instructions = bytes / 64;
                bulk = bytes;
            } else {
                let address = if address32 {
                    u64::from(ctx.Rax as u32)
                } else {
                    ctx.Rax
                };
                if unsafe { memory::zero((address & !63) as usize, 64, options.ask) } != 64 {
                    return 0;
                }
            }
            Kind::Clzero
        }
        Op::WriteBack => {
            if can_skip && let Some((end, lines)) = advance_writebacks(ctx, code) {
                len = end;
                instructions = lines;
            }
            Kind::Clwb
        }
        Op::ReadCounter => {
            read_counter(ctx);
            Kind::Rdpru
        }
        Op::Monitor => Kind::Monitorx,
        Op::Wait => {
            std::hint::spin_loop();
            Kind::Mwaitx
        }
        Op::ProcessorId(dst) => {
            *gpr(ctx, dst) = processor_id();
            Kind::Rdpid
        }
        Op::Field {
            insert,
            dst,
            src,
            immediate,
        } => {
            field(
                unsafe { &mut ctx.Anonymous.FltSave.XmmRegisters },
                insert,
                dst,
                src,
                immediate,
            );
            if insert { Kind::Insertq } else { Kind::Extrq }
        }
    };
    ctx.Rip += len as u64;
    if options.stats {
        count(kind, instructions, bulk);
    }
    -1
}
