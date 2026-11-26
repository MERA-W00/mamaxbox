use super::*;
use decode::decode;
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_NOACCESS, PAGE_READONLY, PAGE_READWRITE,
    VirtualAlloc, VirtualFree, VirtualProtect,
};

const SYNTHETIC: Options = Options {
    bulk: true,
    ask: false,
    stats: false,
};
const LOOP: &[u8] = &[
    0x0f, 1, 0xfc, 0x48, 0x83, 0xc0, 64, 0x4c, 0x39, 0xd0, 0x72, 0xf4,
];

fn context(code: &[u8]) -> CONTEXT {
    let mut ctx: CONTEXT = unsafe { std::mem::zeroed() };
    ctx.Rip = code.as_ptr() as u64;
    ctx
}

#[test]
fn prefix_order_length_and_registers() {
    for (code, expected) in [
        (&[0x67, 0x0f, 1, 0xfc][..], Op::Zero { address32: true }),
        (
            &[0x66, 0x67, 0x41, 0x0f, 0xae, 0x34, 0x25, 8, 0, 0, 0],
            Op::WriteBack,
        ),
        (&[0x3e, 0xf3, 0x41, 0x0f, 0xc7, 0xfa], Op::ProcessorId(10)),
        (&[0x41, 0xf3, 0x0f, 0xc7, 0xfa], Op::ProcessorId(2)),
        (&[0xf3, 0x41, 0x40, 0x0f, 0xc7, 0xfa], Op::ProcessorId(2)),
        (
            &[0x66, 0x41, 0x0f, 0x78, 0xc7, 0, 0],
            Op::Field {
                insert: false,
                dst: 15,
                src: 15,
                immediate: Some([0, 0]),
            },
        ),
        (
            &[0xf2, 0x45, 0x0f, 0x79, 0xe2],
            Op::Field {
                insert: true,
                dst: 12,
                src: 10,
                immediate: None,
            },
        ),
    ] {
        assert_eq!(decode(code), Some((expected, code.len())));
        for n in 0..code.len() {
            assert_eq!(decode(&code[..n]), None);
        }
    }
    let mut full = [0x67; 16];
    full[13..].copy_from_slice(&[15, 1, 0xfc]);
    assert!(decode(&full).is_none());
    assert_eq!(decode(&full[1..]).unwrap().1, 15);
}

#[test]
fn rejects_other_opcodes_and_operand_classes() {
    for code in [
        &[0xf0, 15, 1, 0xfc][..],
        &[0xf3, 15, 1, 0xfa],
        &[15, 1, 0xf9],
        &[0xf2, 15, 0xc7, 0xf8],
        &[0xf3, 15, 0xc7, 0x38],
        &[0xf3, 15, 0xc7, 0xf0],
        &[0x66, 15, 0xae, 0xf0],
        &[0x66, 15, 0xae, 0x38],
        &[15, 0x79, 0xc0],
        &[0xf2, 15, 0x79, 0],
        &[0x66, 15, 0x78, 0xd0, 8, 0],
    ] {
        assert_eq!(decode(code), None, "{code:x?}");
    }
}

#[test]
fn bit_fields_clip_at_the_top_and_snapshot_aliases() {
    let mut xmm = [M128A { Low: 0, High: 0 }; 16];
    xmm[15] = M128A {
        Low: 0xabcd_0123_4567_89ef,
        High: -11,
    };
    field(&mut xmm, false, 15, 15, Some([0xc8, 0x7c]));
    assert_eq!((xmm[15].Low, xmm[15].High), (0xa, -11));
    xmm[9] = M128A {
        Low: 0x0307,
        High: 0x3c08,
    };
    field(&mut xmm, true, 9, 9, None);
    assert_eq!(xmm[9].Low, 0x7000_0000_0000_0307);
    field(&mut xmm, false, 9, 9, None);
    assert_eq!(xmm[9].Low, 0x60);
    xmm[2].Low = u64::MAX - 1;
    field(&mut xmm, true, 9, 2, Some([0, 0]));
    assert_eq!((xmm[9].Low, xmm[9].High), (u64::MAX - 1, 0x3c08));
}

#[test]
fn counter_selector_width_carry_and_processor_id_destination() {
    for selector in [0, 1, 2, 0x1_0000_0000] {
        let mut c = context(&[15, 1, 0xfd]);
        c.Rcx = selector;
        c.EFlags = 0x247;
        assert_eq!(unsafe { emulate_with(&mut c, SYNTHETIC) }, -1);
        assert_eq!(c.Rcx, selector);
        assert_eq!(c.EFlags, 0x246 | u32::from(selector == 2));
        assert!(c.Rax <= u64::from(u32::MAX) && c.Rdx <= u64::from(u32::MAX));
        assert_eq!(c.Rax | c.Rdx == 0, selector == 2);
    }
    let mut c = context(&[0xf3, 0x41, 15, 0xc7, 0xfa]);
    c.R10 = u64::MAX;
    c.Rdx = 0xfeed;
    c.EFlags = 0x247;
    assert_eq!(unsafe { emulate_with(&mut c, SYNTHETIC) }, -1);
    assert!(c.R10 <= u64::from(u32::MAX));
    assert_eq!((c.Rdx, c.EFlags), (0xfeed, 0x247));
}

#[test]
fn counted_loop_changes_only_its_lines_cursor_flags_and_rip() {
    #[repr(align(64))]
    struct Block([u8; 384]);
    for (bulk, ask, debug) in [
        (true, false, 0),
        (true, true, 0),
        (false, false, 0),
        (true, false, 0x100),
    ] {
        let mut block = Block([0x77; 384]);
        let start = block.0.as_mut_ptr() as u64 + 64;
        let mut c = context(LOOP);
        let ip = c.Rip;
        c.Rax = start + 3;
        c.R10 = start + 256;
        c.EFlags = 0x202 | counted::STATUS | debug;
        let batched = bulk && debug == 0;
        let n = if batched { 256 } else { 64 };
        assert_eq!(
            unsafe {
                emulate_with(
                    &mut c,
                    Options {
                        bulk,
                        ask,
                        ..SYNTHETIC
                    },
                )
            },
            -1
        );
        assert!(
            block.0[..64]
                .iter()
                .chain(&block.0[64 + n..])
                .all(|&b| b == 0x77)
        );
        assert!(block.0[64..64 + n].iter().all(|&b| b == 0));
        assert_eq!(c.R10, start + 256);
        assert_eq!(
            (c.Rax, c.Rip - ip),
            if batched {
                (start + 259, 12)
            } else {
                (start + 3, 3)
            }
        );
        assert_eq!(
            c.EFlags,
            if batched {
                0x206
            } else {
                0x202 | counted::STATUS | debug
            }
        );
    }
}

#[test]
fn reversed_operands_immediate_limits_and_wraparound() {
    // LEA, CMP R10,RAX, then JA back using a near displacement.
    let reversed = [
        15, 1, 0xfc, 0x48, 0x8d, 0x40, 64, 0x49, 0x39, 0xc2, 15, 0x87, 0xf0, 0xff, 0xff, 0xff,
    ];
    let plan = counted::recognize(&reversed, 3).unwrap();
    let exit = plan
        .plan(0x1003, false, |i| {
            assert_eq!(i, 10);
            0x1100
        })
        .unwrap();
    assert_eq!(
        (plan.end, exit.after(exit.bytes), exit.bytes),
        (16, (0x1103, 0x91), 256)
    );
    // ADD EAX,64 / CMP EAX,0x100 / JBE. Equality takes one more iteration.
    let immediate = [
        0x67, 15, 1, 0xfc, 5, 64, 0, 0, 0, 0x3d, 0, 1, 0, 0, 0x76, 0xf0,
    ];
    let plan = counted::recognize(&immediate, 4).unwrap();
    let exit = plan.plan(0x1_0000_00c0, true, |_| unreachable!()).unwrap();
    assert_eq!(
        (exit.start, exit.after(exit.bytes), exit.bytes),
        (0xc0, (0x140, 0), 128)
    );
    assert!(plan.plan(0xffff_ffc0, true, |_| 0).is_none());
    assert!(plan.plan(0x1_0000_00c0, false, |_| 0).is_none());
    let mut unequal = LOOP.to_vec();
    unequal[10] = 0x75;
    let plan = counted::recognize(&unequal, 3).unwrap();
    assert!(plan.plan(0x1001, false, |_| 0x1100).is_none());
    let exit = plan.plan(0x1000, false, |_| 0x1100).unwrap();
    assert_eq!(exit.after(exit.bytes), (0x1100, 0x44));
}

#[test]
fn rejects_truncation_wrong_stride_back_edge_and_mutable_bounds() {
    for n in 0..LOOP.len() {
        assert!(counted::recognize(&LOOP[..n], 3).is_none());
    }
    for (offset, byte) in [
        (6, 32),
        (5, 0xc1),
        (9, 0xd1),
        (9, 0x10),
        (11, 0xf5),
        (10, 0xeb),
    ] {
        let mut code = LOOP.to_vec();
        code[offset] = byte;
        assert!(counted::recognize(&code, 3).is_none(), "{code:x?}");
    }
    // REX.R selects R8 as a stable bound. Clearing it compares RAX to itself.
    let mut same = LOOP.to_vec();
    same[9] = 0xc0;
    assert!(counted::recognize(&same, 3).is_some());
    same[7] = 0x48;
    assert!(counted::recognize(&same, 3).is_none());
}

struct Pages(*mut u8);
impl Pages {
    fn new() -> Self {
        Self::with_size(8192)
    }
    fn with_size(size: usize) -> Self {
        let p = unsafe {
            VirtualAlloc(
                std::ptr::null(),
                size,
                MEM_RESERVE | MEM_COMMIT,
                PAGE_READWRITE,
            )
        };
        assert!(!p.is_null());
        Self(p.cast())
    }
    fn protect_second(&self, protect: u32) {
        let mut previous = 0;
        assert_ne!(
            unsafe { VirtualProtect(self.0.add(4096).cast(), 4096, protect, &mut previous) },
            0
        );
    }
}
impl Drop for Pages {
    fn drop(&mut self) {
        unsafe {
            VirtualFree(self.0.cast(), 0, MEM_RELEASE);
        }
    }
}

#[test]
fn inaccessible_code_boundaries_and_revoked_cache_entries() {
    let pages = Pages::new();
    let at = pages.0 as usize + 4094;
    unsafe {
        std::ptr::copy_nonoverlapping([15, 1, 0xfd].as_ptr(), at as *mut u8, 3);
    }
    let mut out = [0; 3];
    assert_eq!(memory::fetch(at, &mut out, false), 3);
    assert_eq!(decode(&out), Some((Op::ReadCounter, 3)));
    assert_eq!(unsafe { memory::zero(at + 66, 64, false) }, 64);
    pages.protect_second(PAGE_NOACCESS);
    assert_eq!(memory::fetch(at, &mut out, false), 2);
    let mut c = context(&[]);
    c.Rip = at as u64;
    assert_eq!(unsafe { emulate_with(&mut c, SYNTHETIC) }, 0);
    assert_eq!(c.Rip, at as u64);
    pages.protect_second(PAGE_READONLY);
    assert_eq!(unsafe { memory::zero(at + 66, 64, false) }, 0);
    assert_eq!(unsafe { memory::zero(at + 66, 64, true) }, 0);
    assert_eq!(memory::fetch(usize::MAX, &mut out, false), 0);
}

#[test]
fn failed_bulk_preflight_falls_back_to_exactly_one_line() {
    let pages = Pages::new();
    unsafe { pages.0.write_bytes(0x77, 8192) };
    pages.protect_second(PAGE_READONLY);
    let start = pages.0 as u64 + 4096 - 128;
    let mut c = context(LOOP);
    let ip = c.Rip;
    c.Rax = start;
    c.R10 = start + 256;
    assert_eq!(
        unsafe {
            emulate_with(
                &mut c,
                Options {
                    ask: true,
                    ..SYNTHETIC
                },
            )
        },
        -1
    );
    assert_eq!((c.Rip, c.Rax), (ip + 3, start));
    let data = unsafe { std::slice::from_raw_parts(start as *const u8, 256) };
    assert!(data[..64].iter().all(|&b| b == 0));
    assert!(data[64..].iter().all(|&b| b == 0x77));
}

#[test]
fn changed_protection_resumes_after_the_last_completed_line() {
    let pages = Pages::with_size(131072);
    let start = pages.0 as usize;
    unsafe { pages.0.write_bytes(0x99, 131072) };
    assert_eq!(unsafe { memory::zero(start, 64, false) }, 64);
    let mut old = 0;
    assert_ne!(
        unsafe { VirtualProtect(pages.0.add(65536).cast(), 65536, PAGE_READONLY, &mut old) },
        0
    );
    let mut c = context(LOOP);
    let ip = c.Rip;
    c.Rax = start as u64;
    c.R10 = c.Rax + 65600;
    c.EFlags = 0x202;
    assert_eq!(unsafe { emulate_with(&mut c, SYNTHETIC) }, -1);
    assert_eq!((c.Rip - ip, c.Rax), (3, start as u64 + 65536 - 64));
    // The previous comparison subtracted a bound 128 bytes ahead.
    assert_eq!(c.EFlags, 0x283);
    let data = unsafe { std::slice::from_raw_parts(pages.0, 131072) };
    assert!(data[..65536].iter().all(|&b| b == 0));
    assert!(data[65536..].iter().all(|&b| b == 0x99));
}

#[test]
fn writing_the_loop_itself_executes_one_clzero() {
    let pages = Pages::new();
    unsafe {
        pages.0.write_bytes(0x88, 128);
        pages.0.copy_from_nonoverlapping(LOOP.as_ptr(), LOOP.len());
    }
    let mut c = context(LOOP);
    c.Rip = pages.0 as u64;
    c.Rax = c.Rip;
    c.R10 = c.Rip + 128;
    assert_eq!(unsafe { emulate_with(&mut c, SYNTHETIC) }, -1);
    assert_eq!((c.Rip, c.Rax), (pages.0 as u64 + 3, pages.0 as u64));
    assert!(
        unsafe { std::slice::from_raw_parts(pages.0.add(64), 64) }
            .iter()
            .all(|&b| b == 0x88)
    );
}

#[test]
fn signed_branches_match_scalar_iteration_and_host_cmp_flags() {
    for wide in [false, true] {
        let mut code = LOOP.to_vec();
        if !wide {
            code[3] = 0x40;
            code[7] = 0x44;
        }
        code[10] = 0x7c;
        let body = counted::recognize(&code, 3).unwrap();
        let negative = if wide { 1u64 << 63 } else { 1u64 << 31 };
        for (start, limit) in [
            (3, 256),
            (3, 67),
            (3, negative + 1),
            (negative + 3, negative + 256),
        ] {
            let exit = body.plan(start, !wide, |_| limit).unwrap();
            let mut scalar = start;
            loop {
                scalar += 64;
                if (scalar ^ negative) >= (limit ^ negative) {
                    break;
                }
            }
            let actual: u64;
            unsafe {
                if wide {
                    std::arch::asm!("cmp {lhs}, {rhs}", "pushfq", "pop {out}",
                        lhs = in(reg) scalar, rhs = in(reg) limit, out = lateout(reg) actual);
                } else {
                    std::arch::asm!("cmp {lhs:e}, {rhs:e}", "pushfq", "pop {out}",
                        lhs = in(reg) scalar, rhs = in(reg) limit, out = lateout(reg) actual);
                }
            }
            assert_eq!(exit.bytes, scalar - start);
            assert_eq!(
                exit.after(exit.bytes),
                (scalar, actual as u32 & counted::STATUS)
            );
        }
    }
}
