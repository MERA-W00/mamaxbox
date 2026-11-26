//! Approach after XCL 77905f6: give a CPU access its missing page and let
//! concurrent first touches retry. Here every guest address names a slice
//! of an existing section. Mapping that slice preserves writes through
//! other aliases, including writes made after the GPU mapping request.
//! The streaming trace had about 850 page replacements in one frame.
//! Keeping a section and offset per page avoids replacing neighbouring
//! CPU views when the next texture needs just one of those pages.

use super::*;
use std::cell::Cell;
use std::sync::atomic::AtomicU64;
use std::sync::{OnceLock, RwLock};
use windows_sys::Win32::Foundation::EXCEPTION_ACCESS_VIOLATION;
use windows_sys::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, EXCEPTION_CONTINUE_EXECUTION, EXCEPTION_CONTINUE_SEARCH,
    EXCEPTION_POINTERS,
};
use windows_sys::Win32::System::Memory::{PAGE_READONLY, VirtualQuery};

struct Alias {
    slice: Slice,
    host: usize,
    cpu_view: bool,
}

// GPU reads can hold the texture remap guard while a writer owns PHYSICAL
// and is waiting for that guard. Neither a lookup nor the fault handler
// may acquire PHYSICAL. No guest bytes are accessed under this lock.
#[derive(Default)]
struct Directory {
    pages: BTreeMap<usize, Alias>,
    /// A software-only page was unmapped. A later native VM operation
    /// still sees the page-sized placeholder left by an eager unmap.
    holes: std::collections::BTreeSet<usize>,
    // Number of software aliases of each pool page, including those with
    // CPU views. A write through the stable section bypasses guest guards.
    backings: BTreeMap<(usize, u64), usize>,
}

impl Directory {
    fn forget(&mut self, at: usize) -> Option<Alias> {
        let old = self.pages.remove(&at)?;
        let key = old.backing();
        let count = self.backings.get_mut(&key).expect("alias has a backing");
        *count -= 1;
        if *count == 0 {
            self.backings.remove(&key);
        }
        Some(old)
    }

    fn insert(&mut self, at: usize, alias: Alias) {
        self.holes.remove(&at);
        *self.backings.entry(alias.backing()).or_default() += 1;
        self.pages.insert(at, alias);
    }
}

impl Alias {
    fn backing(&self) -> (usize, u64) {
        (
            self.slice.section.handle as usize,
            (self.slice.index * PAGE) as u64,
        )
    }
}

static ALIASES: LazyLock<RwLock<Directory>> = LazyLock::new(Default::default);
static INSTALLED: OnceLock<bool> = OnceLock::new();
static STATS: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];
thread_local! { static KERNEL: Cell<Option<u64>> = const { Cell::new(None) }; }

pub(super) fn enabled() -> bool {
    static ON: LazyLock<bool> = LazyLock::new(|| {
        std::env::var_os("VICTORY_LAZY_REMAP").is_some_and(|v| v == "1") && install()
    });
    *ON
}

fn install() -> bool {
    *INSTALLED.get_or_init(|| {
        // SAFETY: the handler and its directory live for the entire process.
        let ok = !unsafe { AddVectoredExceptionHandler(1, Some(fault)) }.is_null();
        if ok {
            // SAFETY: lookup pins the section and clips each result to its
            // committed page and the caller's range.
            unsafe { victory_gpu::remap::register(lookup, backing_used, report) };
        } else {
            tracing::warn!("could not install the lazy page handler, using eager section views");
        }
        ok
    })
}

pub(super) fn measure(len: usize, run: impl FnOnce() -> bool) -> bool {
    victory_gpu::remap::register_counters(report);
    let start = std::time::Instant::now();
    let outer = KERNEL.replace(Some(0));
    let ok = run();
    let calls = KERNEL.replace(outer).unwrap_or(0);
    if let Some(n) = outer {
        KERNEL.set(Some(n + calls));
    }
    STATS[1].fetch_add(calls, Ordering::Relaxed);
    STATS[0].fetch_add(len.div_ceil(PAGE) as u64, Ordering::Relaxed);
    STATS[4].fetch_add(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
    ok
}

pub(super) fn kernel_call() {
    if let Some(n) = KERNEL.get() {
        KERNEL.set(Some(n + 1));
    }
}

pub(super) fn report(seconds: f64) {
    let [requested, kernel, views, faults, nanos] = STATS
        .each_ref()
        .map(|v| v.swap(0, Ordering::Relaxed) as f64 / seconds);
    tracing::info!(
        remaps_requested_s = requested,
        kernel_remaps_s = kernel,
        lazy_views_s = views,
        remap_faults_s = faults,
        title_mapping_ms_s = nanos / 1e6,
        "GPU page mappings"
    );
}

fn lookup(at: usize, len: usize) -> Option<victory_gpu::remap::Span> {
    let end = at.checked_add(len)?;
    if len == 0 {
        return None;
    }
    let table = ALIASES.read().unwrap_or_else(|e| e.into_inner());
    let (&base, entry) = table.pages.range((at & !(PAGE - 1))..end).next()?;
    let guest = at.max(base);
    Some(victory_gpu::remap::Span {
        guest,
        host: entry.host + (guest - base),
        len: end.min(base + PAGE) - guest,
        owner: entry.slice.section.clone(),
    })
}

fn backing_used(section: usize, offset: u64, len: usize) -> bool {
    let table = ALIASES.read().unwrap_or_else(|e| e.into_inner());
    let first = offset & !(PAGE as u64 - 1);
    table
        .backings
        .range((section, first)..(section, offset + len as u64))
        .next()
        .is_some()
}

fn materialise(base: usize, entry: &mut Alias) -> bool {
    if entry.cpu_view {
        return true;
    }
    // SAFETY: the caller owns this placeholder under ALIASES. The pool
    // mapping already committed this exact section page. Never install a
    // longer view, even if both neighbouring slices are consecutive.
    let view = unsafe {
        VirtualFree(base as _, PAGE, MEM_RELEASE | MEM_PRESERVE_PLACEHOLDER);
        MapViewOfFile3(
            entry.slice.section.handle,
            GetCurrentProcess(),
            base as _,
            (entry.slice.index * PAGE) as u64,
            PAGE,
            MEM_REPLACE_PLACEHOLDER,
            PAGE_READWRITE,
            std::ptr::null_mut(),
            0,
        )
    };
    entry.cpu_view = view.Value as usize == base;
    if entry.cpu_view {
        STATS[1].fetch_add(1, Ordering::Relaxed);
        STATS[2].fetch_add(1, Ordering::Relaxed);
    }
    entry.cpu_view
}

unsafe extern "system" fn fault(info: *mut EXCEPTION_POINTERS) -> i32 {
    // SAFETY: Windows owns the exception record during this callback.
    let record = unsafe { &*(*info).ExceptionRecord };
    let access = record.ExceptionInformation[0];
    if record.ExceptionCode != EXCEPTION_ACCESS_VIOLATION
        || record.NumberParameters < 2
        || access > 1
    {
        return EXCEPTION_CONTINUE_SEARCH;
    }
    let at = record.ExceptionInformation[1];
    let base = at & !(PAGE - 1);
    let mut table = ALIASES.write().unwrap_or_else(|e| e.into_inner());
    let Some(entry) = table.pages.get_mut(&base) else {
        return EXCEPTION_CONTINUE_SEARCH;
    };
    let retry = if entry.cpu_view {
        // A second fault may already have been delivered, or a write guard
        // may have since closed the page. Only retry an allowed access.
        let mut mbi = MEMORY_BASIC_INFORMATION::default();
        // SAFETY: querying the status of the faulting byte.
        (unsafe { VirtualQuery(at as _, &mut mbi, size_of_val(&mbi)) }) != 0
            && mbi.State == MEM_COMMIT
            && (mbi.Protect == PAGE_READWRITE || (access == 0 && mbi.Protect == PAGE_READONLY))
    } else {
        materialise(base, entry)
    };
    if retry {
        STATS[3].fetch_add(1, Ordering::Relaxed);
        EXCEPTION_CONTINUE_EXECUTION
    } else {
        EXCEPTION_CONTINUE_SEARCH
    }
}

/// Kernel I/O checks its buffer inside the system call. It cannot trigger
/// the user-mode handler. Establish only the views intersecting that I/O.
pub(crate) fn prepare(at: usize, len: usize) -> bool {
    if INSTALLED.get() != Some(&true) || len == 0 {
        return true;
    }
    let mut table = ALIASES.write().unwrap_or_else(|e| e.into_inner());
    table
        .pages
        .range_mut((at & !(PAGE - 1))..at.saturating_add(len))
        .all(|(&base, page)| materialise(base, page))
}

fn split(at: usize) {
    // SAFETY: a placeholder left by a software page. Already exactly one
    // page is also fine. The real VM operation decides whether it can use it.
    unsafe { VirtualFree(at as _, PAGE, MEM_RELEASE | MEM_PRESERVE_PLACEHOLDER) };
}

pub(super) fn split_holes(at: usize, len: usize) {
    if INSTALLED.get() != Some(&true) || len == 0 {
        return;
    }
    let mut table = ALIASES.write().unwrap_or_else(|e| e.into_inner());
    let range = (at & !(PAGE - 1))..at.saturating_add(len);
    while let Some(&base) = table.holes.range(range.clone()).next() {
        split(base);
        table.holes.remove(&base);
    }
}

/// Split the nearest guest boundaries on either side of a VM query.
/// Only the page actually queried needs a CPU view.
pub(super) fn boundaries(lo: usize, end: usize, query: usize) -> bool {
    let mut table = ALIASES.write().unwrap_or_else(|e| e.into_inner());
    let at = query & !(PAGE - 1);
    if lo > at || end <= at {
        return true;
    }
    let before = table
        .pages
        .range(lo..=at)
        .next_back()
        .map(|(&p, _)| p)
        .max(table.holes.range(lo..=at).next_back().copied());
    let after = table
        .pages
        .range(at + 1..end)
        .next()
        .map(|(&p, _)| p)
        .into_iter()
        .chain(table.holes.range(at + 1..end).next().copied())
        .min();
    for base in [before, after].into_iter().flatten() {
        if let Some(page) = table.pages.get_mut(&base) {
            if base == at {
                if !materialise(base, page) {
                    return false;
                }
            } else if !page.cpu_view {
                split(base);
            }
        } else {
            split(base);
            table.holes.remove(&base);
        }
    }
    true
}

fn revoke(base: usize, entry: &Alias) -> bool {
    if !entry.cpu_view {
        return true;
    }
    // SAFETY: the table serializes faults and replacement of this single view.
    let ok = unsafe {
        UnmapViewOfFile2(
            GetCurrentProcess(),
            MEMORY_MAPPED_VIEW_ADDRESS { Value: base as _ },
            MEM_PRESERVE_PLACEHOLDER,
        ) != 0
    };
    if ok {
        kernel_call();
    }
    ok
}

pub(super) fn remove(at: usize) -> Option<bool> {
    if INSTALLED.get() != Some(&true) {
        return None;
    }
    let mut table = ALIASES.write().unwrap_or_else(|e| e.into_inner());
    let entry = table.pages.get(&at)?;
    if !revoke(at, entry) {
        return Some(false);
    }
    if !entry.cpu_view {
        table.holes.insert(at);
    }
    table.forget(at);
    Some(true)
}

fn entry(slice: &Slice) -> Option<Alias> {
    Some(Alias {
        host: slice.section.pool_view()? + slice.index * PAGE,
        slice: slice.clone(),
        cpu_view: false,
    })
}

pub(super) fn contains(at: usize) -> bool {
    ALIASES
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .pages
        .contains_key(&at)
}

fn replace(p: &mut Physical, at: usize, slice: Slice) -> bool {
    let eligible = p
        .reservations
        .range(..=at)
        .next_back()
        .is_some_and(|(_, end)| at + PAGE <= *end);
    if eligible && let Some(new) = entry(&slice) {
        let old = {
            let mut table = ALIASES.write().unwrap_or_else(|e| e.into_inner());
            if let Some(old) = table.pages.get(&at)
                && !revoke(at, old)
            {
                return false;
            }
            let old = table.forget(at);
            table.insert(at, new);
            old
        };
        if let Some(old) = old {
            forget_address(p, old.slice.key(0), at);
        }
        publish(p, at, PAGE, slice);
        true
    } else {
        unmap(p, at, PAGE) && map(p, at, PAGE, slice)
    }
}

pub(super) fn map_alias(to: usize, from: usize, len: usize) -> bool {
    if len == 0 {
        return true;
    }
    if to == 0 && from != 0 {
        return false;
    }
    if !to.is_multiple_of(PAGE)
        || !from.is_multiple_of(PAGE)
        || !len.is_multiple_of(PAGE)
        || to.checked_add(len).is_none()
        || from.checked_add(len).is_none()
    {
        return false;
    }
    let mut p = physical();
    let _changing = victory_gpu::title_memory_remapping_in(|| remapping_extent(&p, to, len));
    if from == 0 {
        return unmap(&mut p, to, len);
    }
    // Preserve the eager prefix-on-failure and overlapping-source rules.
    // Independent software replacements stay installed until their turn:
    // racing CPU faults resolve either generation even in a mixed batch.
    let independent = to.abs_diff(from) >= len;
    if !(independent && len == PAGE && contains(to)) && !unmap_except(&mut p, to, len, independent)
    {
        return false;
    }
    for k in (0..len).step_by(PAGE) {
        let at = to + k;
        if !shown(&p, from + k).is_some_and(|slice| replace(&mut p, at, slice)) {
            unmap(&mut p, at, len - k);
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests;
