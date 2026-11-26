//! Private host copies for the next queued run. The GPU can still be reading
//! the live shadow, so only its ordinary submission-time refresh publishes bytes.

use super::{BLOCK, guards};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, mpsc};

const SLOTS: usize = 32;
const PAGES: usize = 16;
const MIN_BYTES: usize = 4096;

pub(crate) fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_REFRESH_AHEAD").is_some_and(|v| v == "1"))
}

/// The submit profile includes copies of mapped memory. Keep a bounded
/// second copy on the CPU while the driver consumes the preceding lists.
/// Approach after XCL 78fba7b: give work in flight its own storage.
#[derive(Default)]
struct Page {
    ticket: u64,
    identity: Option<(usize, u64)>,
    bytes: Vec<u8>,
}

struct Slot {
    generation: AtomicU64,
    page: Mutex<Page>,
}

impl Slot {
    fn copy(&self, ticket: u64, block: usize, offset: usize, dst: *mut u8, len: usize) -> bool {
        let Ok(page) = self.page.try_lock() else {
            return false;
        };
        if page.ticket != ticket
            || page.identity != Some((block, self.generation.load(Ordering::Acquire)))
        {
            return false;
        }
        let Some(src) = offset
            .checked_add(len)
            .and_then(|end| page.bytes.get(offset..end))
        else {
            return false;
        };
        // SAFETY: the caller owns len writable bytes. The source belongs to
        // this slot and stays locked throughout the copy.
        unsafe { std::ptr::copy_nonoverlapping(src.as_ptr(), dst, len) };
        true
    }
}

static CACHE: [Slot; SLOTS] = [const {
    Slot {
        generation: AtomicU64::new(0),
        page: Mutex::new(Page {
            ticket: 0,
            identity: None,
            bytes: Vec::new(),
        }),
    }
}; SLOTS];
static COUNTS: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
static ACTIVE: AtomicBool = AtomicBool::new(false);
static CONSUMER: AtomicU64 = AtomicU64::new(0);

/// The host owns the submission lock from here through the last worker
/// refresh. Other submissions, repeated recordings and cancelled runs
/// cannot consume this recording's speculative reads.
pub(crate) struct Consumption;

impl Drop for Consumption {
    fn drop(&mut self) {
        CONSUMER.store(0, Ordering::Release);
    }
}

pub(crate) fn consuming(ticket: u64) -> Consumption {
    CONSUMER.store(ticket, Ordering::Release);
    Consumption
}

pub(crate) fn unknown_write() {
    if enabled() {
        for slot in &CACHE {
            slot.generation.fetch_add(1, Ordering::Release);
        }
    }
}

/// An opened or replaced guard ends the lifetime of every snapshot of that
/// block, even if another refresh arms it again before the consumer runs.
pub(super) fn invalidate(block: usize) {
    // This also runs in the write-fault handler. Never initialize a
    // setting or acquire another lock there.
    if ACTIVE.load(Ordering::Acquire) {
        CACHE[block % SLOTS]
            .generation
            .fetch_add(1, Ordering::Release);
    }
}

#[derive(Default)]
pub(crate) struct Request {
    pages: [usize; PAGES],
    count: usize,
}

impl Request {
    pub(crate) fn add(&mut self, address: usize, len: usize) {
        let Some(end) = address.checked_add(len) else {
            return;
        };
        if len < MIN_BYTES {
            return;
        }
        for block in (address / BLOCK..end.div_ceil(BLOCK)).take(PAGES) {
            if self.full() {
                break;
            }
            if !self.pages[..self.count].contains(&block) {
                self.pages[self.count] = block;
                self.count += 1;
            }
        }
    }

    pub(crate) fn full(&self) -> bool {
        self.count == PAGES
    }

    pub(crate) fn send(self) -> u64 {
        if self.count == 0 {
            return 0;
        }
        static NEXT: AtomicU64 = AtomicU64::new(1);
        static WORK: OnceLock<Option<mpsc::SyncSender<(u64, Request)>>> = OnceLock::new();
        let worker = WORK.get_or_init(|| {
            // One waiting request. A full queue or a failed start leaves
            // the entire refresh to the submitting thread.
            let (send, receive) = mpsc::sync_channel::<(u64, Request)>(1);
            std::thread::Builder::new()
                .name("victory-refresh".into())
                .spawn(move || {
                    for (ticket, request) in receive {
                        for &block in &request.pages[..request.count] {
                            prepare(ticket, block);
                        }
                    }
                })
                .ok()
                .map(|_| send)
        });
        let ticket = NEXT.fetch_add(1, Ordering::Relaxed);
        if worker
            .as_ref()
            .is_some_and(|w| w.try_send((ticket, self)).is_ok())
        {
            ticket
        } else {
            0
        }
    }
}

fn prepare(ticket: u64, block: usize) {
    if !super::view_guard() || super::page_writes() & 1 == 0 || crate::inplace::level() != 0 {
        return;
    }
    let Some(_mapping) = super::title_memory_reading_now() else {
        return;
    };
    ACTIVE.store(true, Ordering::Release);
    let at = block * BLOCK;
    // The runtime identifies this physical page and every view of it.
    // A private allocation can be freed and reused at the same address.
    // A shared page can also be changed through its other views.
    if crate::inplace::pages_of(at).as_slice() != [at] {
        return;
    }
    // A writable page may already be the destination of N's kernel copy.
    // Only borrow a guard an ordinary refresh established in queue order.
    let slot = &CACHE[block % SLOTS];
    let stamp = {
        let g = guards();
        if !g.protected.contains(&block) {
            return;
        }
        slot.generation.load(Ordering::Acquire)
    };
    let Ok(mut page) = slot.page.try_lock() else {
        return;
    };
    if page.ticket == ticket && page.identity == Some((block, stamp)) {
        return;
    }
    page.ticket = ticket;
    page.identity = None;
    page.bytes.resize(BLOCK, 0);
    if crate::host::read_into(at as *const u8, &mut page.bytes)
        && stamp == slot.generation.load(Ordering::Acquire)
    {
        page.identity = Some((block, stamp));
        COUNTS[0].fetch_add(1, Ordering::Relaxed);
    }
}

/// The normal refresh has already excluded pending GPU writes and found
/// a stale range. An absent, busy or invalidated slot takes the usual read.
pub(crate) fn copy(dst: *mut u8, from: usize, len: usize) -> bool {
    if !enabled() || len < MIN_BYTES || len > BLOCK - from % BLOCK {
        return false;
    }
    let ticket = CONSUMER.load(Ordering::Acquire);
    if ticket == 0 {
        return false;
    }
    #[cfg(feature = "diagnostics")]
    if crate::copystat::mode() != 0 {
        return false;
    }
    let Some(_mapping) = super::title_memory_reading_now() else {
        return false;
    };
    let block = from / BLOCK;
    let hit = CACHE[block % SLOTS].copy(ticket, block, from % BLOCK, dst, len);
    COUNTS[1].fetch_add(1, Ordering::Relaxed);
    if hit {
        COUNTS[2].fetch_add(len as u64, Ordering::Relaxed);
    }
    hit
}

pub(crate) fn line(frames: f64) {
    if enabled() {
        let [prepared, reads, bytes] = COUNTS
            .each_ref()
            .map(|n| n.swap(0, Ordering::Relaxed) as f64 / frames);
        tracing::info!(
            prepared,
            reads,
            bytes,
            "guarded host copies prepared ahead per frame, eligible reads and reused bytes"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_managed_page_can_be_staged_until_a_store_or_remap() {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, FILE_MAP_WRITE, MapViewOfFile, PAGE_READWRITE, UnmapViewOfFile,
        };
        if !super::super::view_guard()
            || super::super::page_writes() & 1 == 0
            || crate::inplace::level() != 0
        {
            return;
        }
        // SAFETY: one fresh pagefile-backed view, owned until the end of this test.
        let (section, view) = unsafe {
            let section = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                BLOCK as u32,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            (section, MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, BLOCK))
        };
        let at = view.Value as usize;
        assert_ne!(at, 0);
        crate::watch::title_memory_remapped(at, BLOCK);
        crate::inplace::title_memory_view(at, BLOCK, section as usize, 0);
        let block = at / BLOCK;
        let slot = &CACHE[block % SLOTS];
        let mut byte = 0;
        // SAFETY: the test owns this writable view. A write to an armed
        // block goes through the same fault handler as a title store.
        unsafe { (at as *mut u8).write_volatile(17) };
        prepare(1, block);
        assert!(!slot.copy(1, block, 0, &mut byte, 1));
        assert!(super::super::writable(at));
        assert!(crate::watch::writes(at, at + BLOCK, true).is_some());
        prepare(1, block);
        assert!(slot.copy(1, block, 0, &mut byte, 1));
        assert_eq!(byte, 17);
        if enabled() {
            let mut request = Request::default();
            request.add(at, BLOCK);
            let ticket = request.send();
            assert_ne!(ticket, 0);
            let mut out = [0u8; MIN_BYTES];
            {
                let _owner = consuming(ticket);
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                while !copy(out.as_mut_ptr(), at, out.len()) {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "worker did not publish the guarded page"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
            assert_eq!(out[0], 17);
            assert!(!copy(out.as_mut_ptr(), at, out.len()));
        }
        unsafe { (at as *mut u8).write_volatile(32) };
        assert!(!slot.copy(1, block, 0, &mut byte, 1));
        assert!(crate::watch::writes(at, at + BLOCK, true).is_some());
        prepare(2, block);
        assert!(slot.copy(2, block, 0, &mut byte, 1));
        assert!(!slot.copy(1, block, 0, &mut byte, 1));
        assert_eq!(byte, 32);
        crate::watch::title_memory_remapped(at, BLOCK);
        assert!(!slot.copy(2, block, 0, &mut byte, 1));
        // SAFETY: release the view and its remaining handle.
        unsafe {
            UnmapViewOfFile(view);
            CloseHandle(section);
        }
    }

    #[test]
    fn lookahead_is_bounded_and_deduplicates_partial_pages() {
        let mut work = Request::default();
        work.add(usize::MAX - 4, BLOCK);
        work.add(1, 16);
        work.add(BLOCK - 1, MIN_BYTES);
        work.add(0, 100 * BLOCK);
        assert_eq!(&work.pages[..work.count], &(0..PAGES).collect::<Vec<_>>());
    }

    #[test]
    fn an_opened_guard_cannot_reuse_a_snapshot_after_rearming() {
        let slot = Slot {
            generation: AtomicU64::new(8),
            page: Mutex::new(Page {
                ticket: 3,
                identity: Some((10, 8)),
                bytes: vec![21, 22, 23, 24],
            }),
        };
        let mut bytes = [0; 2];
        assert!(slot.copy(3, 10, 1, bytes.as_mut_ptr(), 2));
        assert_eq!(bytes, [22, 23]);
        assert!(!slot.copy(3, 10 + SLOTS, 0, bytes.as_mut_ptr(), 2));
        slot.generation.fetch_add(1, Ordering::Release);
        assert!(!slot.copy(3, 10, 0, bytes.as_mut_ptr(), 2));
        assert_eq!(bytes, [22, 23]);
    }
}
