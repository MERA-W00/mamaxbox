//! Checks guest memory without taking a second exception in the handler.

use std::{cell::Cell, ffi::c_void, mem::MaybeUninit};
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE,
    PAGE_EXECUTE_WRITECOPY, PAGE_GUARD, PAGE_READONLY, PAGE_READWRITE, PAGE_WRITECOPY,
    VirtualQuery,
};

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtReadVirtualMemory(
        process: *mut c_void,
        address: *const c_void,
        buffer: *mut u8,
        size: usize,
        copied: *mut usize,
    ) -> i32;
    fn NtWriteVirtualMemory(
        process: *mut c_void,
        address: *mut c_void,
        buffer: *const u8,
        size: usize,
        copied: *mut usize,
    ) -> i32;
}

const CURRENT_PROCESS: *mut c_void = -1isize as _;

#[derive(Clone, Copy, Default)]
struct Region {
    start: usize,
    end: usize,
}

thread_local! {
    static CODE: Cell<Region> = const { Cell::new(Region { start: 0, end: 0 }) };
    static DATA: Cell<Region> = const { Cell::new(Region { start: 0, end: 0 }) };
}

/// VirtualQuery can take milliseconds on a large region. Cache only successful
/// queries, separately for code and data so a loop can reuse both answers.
fn region_end(at: usize, write: bool, ask: bool) -> Option<usize> {
    let slot = if write { &DATA } else { &CODE };
    let cached = slot.get();
    if !ask && (cached.start..cached.end).contains(&at) {
        return Some(cached.end);
    }
    let mut info = MaybeUninit::<MEMORY_BASIC_INFORMATION>::uninit();
    // SAFETY: the address is only queried. The output has the specified size.
    if unsafe {
        VirtualQuery(
            at as _,
            info.as_mut_ptr(),
            size_of::<MEMORY_BASIC_INFORMATION>(),
        )
    } != size_of::<MEMORY_BASIC_INFORMATION>()
    {
        return None;
    }
    let info = unsafe { info.assume_init() };
    let writable = matches!(
        info.Protect & 0xff,
        PAGE_READWRITE | PAGE_WRITECOPY | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY
    );
    let readable = writable || matches!(info.Protect & 0xff, PAGE_READONLY | PAGE_EXECUTE_READ);
    if info.State != MEM_COMMIT
        || info.Protect & PAGE_GUARD != 0
        || !readable
        || (write && !writable)
    {
        return None;
    }
    let start = info.BaseAddress as usize;
    let end = start.checked_add(info.RegionSize)?;
    if !(start..end).contains(&at) {
        return None;
    }
    if !ask {
        slot.set(Region { start, end });
    }
    Some(end)
}

/// The kernel checks the current mappings during each copy. A stale cached
/// region can never cause a raw load from an inaccessible instruction page.
pub(super) fn fetch(at: usize, out: &mut [u8], ask: bool) -> usize {
    let mut used = 0;
    while used < out.len() {
        let Some(address) = at.checked_add(used) else {
            break;
        };
        let Some(end) = region_end(address, false, ask) else {
            break;
        };
        // Keep bytes preceding a page that was unmapped since the query.
        let n = (out.len() - used)
            .min(end - address)
            .min(4096 - (address & 4095));
        let mut copied = 0;
        let status = unsafe {
            NtReadVirtualMemory(
                CURRENT_PROCESS,
                address as _,
                out[used..].as_mut_ptr(),
                n,
                &mut copied,
            )
        };
        used += copied.min(n);
        if status < 0 || copied != n {
            CODE.set(Region::default());
            break;
        }
    }
    used
}

/// Preflight the whole interval before the first write. The native copy also
/// checks protection at write time, including on a cache hit. Debugger-style
/// writes can temporarily make executable pages writable and would be wrong here.
///
/// # Safety
/// The caller must own the guest writes to this interval.
pub(super) unsafe fn zero(start: usize, bytes: usize, ask: bool) -> usize {
    let Some(end) = start.checked_add(bytes) else {
        return 0;
    };
    let mut address = start;
    while address < end {
        let Some(region) = region_end(address, true, ask) else {
            return 0;
        };
        address = region;
    }
    static ZEROES: [u8; 65536] = [0; 65536];
    address = start;
    while address < end {
        let n = (end - address).min(ZEROES.len());
        let mut copied = 0;
        let status = unsafe {
            NtWriteVirtualMemory(
                CURRENT_PROCESS,
                address as _,
                ZEROES.as_ptr(),
                n,
                &mut copied,
            )
        };
        address += copied.min(n);
        if status < 0 || copied != n {
            DATA.set(Region::default());
            break;
        }
    }
    address - start
}
