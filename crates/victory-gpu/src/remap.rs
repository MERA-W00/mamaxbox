//! Guest texture pages can live in a pool section while their CPU addresses
//! are still placeholders. Shader shadows and texture uploads use the same
//! lookup, so a residency probe and the copy that follows agree.

use std::sync::{Arc, OnceLock};

/// A piece of a guest range in a stable section mapping. Keeping the section
/// alive lets a copy finish even if the guest subsequently releases the pool.
pub struct Span {
    pub guest: usize,
    pub host: usize,
    pub len: usize,
    pub owner: Arc<dyn Send + Sync>,
}

type Lookup = fn(usize, usize) -> Option<Span>;
static LOOKUP: OnceLock<Lookup> = OnceLock::new();
type BackingUsed = fn(usize, u64, usize) -> bool;
static BACKING_USED: OnceLock<BackingUsed> = OnceLock::new();
static COUNTERS: OnceLock<fn(f64)> = OnceLock::new();

/// # Safety
/// Each returned span must be within the requested range with positive
/// length. Its owner must keep that many readable and writable bytes alive
/// at the host address until the span is dropped.
pub unsafe fn register(lookup: Lookup, backing_used: BackingUsed, counters: fn(f64)) {
    let _ = BACKING_USED.set(backing_used);
    let _ = LOOKUP.set(lookup);
    register_counters(counters);
}

pub use crate::host::read_into;

pub fn register_counters(counters: fn(f64)) {
    let _ = COUNTERS.set(counters);
}

pub(crate) fn report(seconds: f64) {
    if let Some(report) = COUNTERS.get() {
        report(seconds);
    }
}

pub(crate) fn overlaps(start: usize, len: usize) -> bool {
    LOOKUP.get().and_then(|lookup| lookup(start, len)).is_some()
}

pub(crate) fn active() -> bool {
    LOOKUP.get().is_some()
}

pub(crate) fn section_writes_may_bypass_guards(start: usize, end: usize) -> bool {
    BACKING_USED
        .get()
        .is_some_and(|used| crate::inplace::any_view(start, end, *used))
}

/// Visits translated pieces and ordinary memory in guest address order.
/// None leaves an untranslated request on its original bulk path.
pub(crate) fn pieces(
    start: usize,
    len: usize,
    mut visit: impl FnMut(usize, usize, usize, bool) -> bool,
) -> Option<bool> {
    let lookup = LOOKUP.get()?;
    let end = start.checked_add(len)?;
    let mut page = lookup(start, len)?;
    let mut at = start;
    loop {
        if at < page.guest && !visit(at, at, page.guest - at, false) {
            return Some(false);
        }
        if !visit(page.guest, page.host, page.len, true) {
            return Some(false);
        }
        at = page.guest + page.len;
        if at == end {
            return Some(true);
        }
        let Some(next) = lookup(at, end - at) else {
            return Some(visit(at, at, end - at, false));
        };
        page = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use windows_sys::Win32::System::Memory::*;

    const PAGE: usize = 65536;
    struct Region(usize);
    impl Drop for Region {
        fn drop(&mut self) {
            unsafe {
                VirtualFree(self.0 as _, 0, MEM_RELEASE);
            }
        }
    }
    fn region(pages: usize, kind: u32) -> Arc<Region> {
        let at = unsafe {
            VirtualAlloc(
                std::ptr::null(),
                pages * PAGE,
                MEM_RESERVE | kind,
                PAGE_READWRITE,
            )
        };
        assert!(!at.is_null());
        Arc::new(Region(at as usize))
    }
    static TEST_MAP: Mutex<Option<(usize, Arc<Region>)>> = Mutex::new(None);

    fn lookup(start: usize, len: usize) -> Option<Span> {
        let table = TEST_MAP.lock().unwrap();
        let (base, pool) = table.as_ref()?;
        let end = start.checked_add(len)?;
        // Two nonconsecutive guests share one physical page.
        for page in [base + PAGE, base + PAGE * 3] {
            let guest = start.max(page);
            let stop = end.min(page + PAGE);
            if guest < stop {
                return Some(Span {
                    guest,
                    host: pool.0 + guest - page,
                    len: stop - guest,
                    owner: pool.clone(),
                });
            }
        }
        None
    }

    #[test]
    fn texture_uploads_and_shader_writeback_share_translated_pages() {
        let guest = region(5, 0);
        let pool = region(1, MEM_COMMIT);
        let at = guest.0;
        // SAFETY: the first page is native, the third is a hole. The
        // software directory pins the shared backing of the other two.
        unsafe {
            assert_eq!(
                VirtualAlloc(at as _, PAGE, MEM_COMMIT, PAGE_READWRITE) as usize,
                at
            );
            std::ptr::write_bytes(at as *mut u8, 0x21, PAGE);
            std::ptr::write_bytes(pool.0 as *mut u8, 0xb3, PAGE);
            register(lookup, |_, _, _| false, |_| {});
        }
        *TEST_MAP.lock().unwrap() = Some((at, pool));
        assert!(crate::texture::committed(at as _, 2 * PAGE));
        assert!(!crate::texture::committed(at as _, 3 * PAGE));
        assert_eq!(
            crate::texture::mapped_bytes(at as _, 4 * PAGE),
            (3 * PAGE) as u64
        );
        let mut upload = vec![0xff; 4 * PAGE - 4];
        crate::host::copy_mapped(upload.as_mut_ptr(), (at + 3) as _, upload.len(), false);
        for (i, &v) in upload.iter().enumerate() {
            assert_eq!(v, [0x21, 0xb3, 0, 0xb3][(i + 3) / PAGE]);
        }
        let bytes = [13, 37, 41, 97];
        crate::host::write_mapped((at + 3 * PAGE + 17) as _, bytes.as_ptr(), 4);
        let mut seen = [0; 4];
        assert!(read_into((at + PAGE + 17) as _, &mut seen));
        assert_eq!(seen, bytes);
        crate::host::copy_mapped((at + PAGE + 49) as _, (at + 3 * PAGE + 17) as _, 4, true);
        assert!(read_into((at + 3 * PAGE + 49) as _, &mut seen));
        assert_eq!(seen, bytes);
        let mut info = MEMORY_BASIC_INFORMATION::default();
        let last = at + 4 * PAGE;
        assert_eq!(
            unsafe { VirtualAlloc(last as _, PAGE, MEM_COMMIT, PAGE_READWRITE) } as usize,
            last
        );
        crate::host::copy_mapped((last - PAGE) as _, at as _, 2 * PAGE, true);
        assert!(read_into((last + 17) as _, &mut seen));
        assert_eq!(
            seen, bytes,
            "the first destination aliases the second source"
        );
        assert_ne!(
            unsafe { VirtualQuery((at + PAGE) as _, &mut info, size_of_val(&info)) },
            0
        );
        assert_eq!(info.State, MEM_RESERVE);
        *TEST_MAP.lock().unwrap() = None;
    }
}
