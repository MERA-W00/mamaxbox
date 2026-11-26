//! A region query or release must see the boundaries of the guest views.
//! Windows otherwise sees one placeholder containing several live aliases.

use super::*;
use windows_sys::Win32::System::{
    Memory as mem,
    Threading::{GetCurrentProcessId, GetProcessId},
};

fn local(process: HANDLE) -> bool {
    // SAFETY: a pseudo handle or a real process handle can name us.
    unsafe { process == GetCurrentProcess() || GetProcessId(process) == GetCurrentProcessId() }
}

fn boundaries(process: HANDLE, at: *const c_void) -> bool {
    let mut info = MEMORY_BASIC_INFORMATION::default();
    // SAFETY: only inspects our own allocation.
    if unsafe { mem::VirtualQueryEx(process, at, &mut info, size_of_val(&info)) } != 0 {
        let lo = info.AllocationBase as usize;
        let end = (info.BaseAddress as usize).saturating_add(info.RegionSize);
        if lo != 0 && info.State == MEM_RESERVE {
            return lazy::boundaries(lo, end, at as usize);
        }
    }
    true
}

pub(super) extern "system" fn query(
    process: HANDLE,
    at: *const c_void,
    out: *mut MEMORY_BASIC_INFORMATION,
    size: usize,
) -> usize {
    if lazy::enabled() && local(process) && !boundaries(process, at) {
        return 0;
    }
    // SAFETY: unchanged host arguments, including the caller's output size.
    unsafe { mem::VirtualQueryEx(process, at, out, size) }
}

extern "system" fn VirtualQuery(
    at: *const c_void,
    out: *mut MEMORY_BASIC_INFORMATION,
    size: usize,
) -> usize {
    query(unsafe { GetCurrentProcess() }, at, out, size)
}

extern "system" fn VirtualQueryEx(
    process: HANDLE,
    at: *const c_void,
    out: *mut MEMORY_BASIC_INFORMATION,
    size: usize,
) -> usize {
    query(process, at, out, size)
}

pub(super) extern "system" fn VirtualFree(at: *mut c_void, size: usize, kind: u32) -> BOOL {
    VirtualFreeEx(unsafe { GetCurrentProcess() }, at, size, kind)
}

extern "system" fn VirtualFreeEx(process: HANDLE, at: *mut c_void, size: usize, kind: u32) -> BOOL {
    if !local(process) {
        return unsafe { mem::VirtualFreeEx(process, at, size, kind) };
    }
    let mut p = physical();
    if !boundaries(process, at) || !lazy::prepare(at as usize, size) {
        return 0;
    }
    lazy::split_holes(at as usize, size);
    let ok = unsafe { mem::VirtualFreeEx(process, at, size, kind) };
    if ok != 0 && kind == MEM_RELEASE {
        retire(&mut p, at as usize, size.max(1));
    }
    ok
}

/// Losing eligibility only costs eager mappings on a later use of the
/// surviving part. Never install a software entry over a reused allocation.
pub(super) fn retire(p: &mut Physical, at: usize, len: usize) {
    let end = at.saturating_add(len);
    p.reservations
        .retain(|&base, stop| *stop <= at || base >= end);
}

extern "system" fn VirtualProtect(
    at: *const c_void,
    size: usize,
    protect: u32,
    old: *mut u32,
) -> BOOL {
    VirtualProtectEx(unsafe { GetCurrentProcess() }, at, size, protect, old)
}

extern "system" fn VirtualProtectEx(
    process: HANDLE,
    at: *const c_void,
    size: usize,
    protect: u32,
    old: *mut u32,
) -> BOOL {
    if local(process) && !lazy::prepare(at as usize, size) {
        return 0;
    }
    unsafe { mem::VirtualProtectEx(process, at, size, protect, old) }
}

extern "system" fn VirtualAlloc2(
    process: HANDLE,
    at: *const c_void,
    size: usize,
    kind: u32,
    protect: u32,
    parameters: *mut MEM_EXTENDED_PARAMETER,
    count: u32,
) -> *mut c_void {
    replace(process, at, size, kind, || unsafe {
        mem::VirtualAlloc2(process, at, size, kind, protect, parameters, count)
    })
}

extern "system" fn MapViewOfFile3(
    section: HANDLE,
    process: HANDLE,
    at: *const c_void,
    offset: u64,
    size: usize,
    kind: u32,
    protect: u32,
    parameters: *mut MEM_EXTENDED_PARAMETER,
    count: u32,
) -> MEMORY_MAPPED_VIEW_ADDRESS {
    MEMORY_MAPPED_VIEW_ADDRESS {
        Value: replace(process, at, size, kind, || unsafe {
            mem::MapViewOfFile3(
                section, process, at, offset, size, kind, protect, parameters, count,
            )
            .Value
        }),
    }
}

fn replace(
    process: HANDLE,
    at: *const c_void,
    size: usize,
    kind: u32,
    call: impl FnOnce() -> *mut c_void,
) -> *mut c_void {
    if at.is_null() || !(process.is_null() || local(process)) || kind & MEM_REPLACE_PLACEHOLDER == 0
    {
        return call();
    }
    let mut p = physical();
    let current = unsafe { GetCurrentProcess() };
    if !boundaries(current, at) {
        return std::ptr::null_mut();
    }
    if !lazy::prepare(at as usize, size) {
        return std::ptr::null_mut();
    }
    lazy::split_holes(at as usize, size);
    let result = call();
    if !result.is_null() {
        retire(&mut p, at as usize, size.max(1));
    }
    result
}

pub(in crate::modules) fn module() -> Option<HleModule> {
    lazy::enabled().then(|| HleModule {
        name: "guest page mappings",
        dlls: &["kernel32.dll", "kernelbase.dll", "api-ms-win-core-memory-*"],
        exports: vec![
            hle_export!(VirtualFree),
            hle_export!(VirtualFreeEx),
            hle_export!(VirtualQuery),
            hle_export!(VirtualQueryEx),
            hle_export!(VirtualProtect),
            hle_export!(VirtualProtectEx),
            hle_export!(VirtualAlloc2),
            hle_export!(MapViewOfFile3),
        ],
    })
}
