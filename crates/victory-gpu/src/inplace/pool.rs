//! A pool buffer belongs to a section. Its GPU address stays valid after
//! a view moves. Resolve the title address again at each submission to
//! pick the offset that view shows now.

use super::*;
use std::sync::{Arc, OnceLock};

/// The hardware probe charged the entire section to shared GPU memory.
/// Four 64 MiB slabs bound that cost while this path is being measured.
fn limit() -> u64 {
    static BYTES: OnceLock<u64> = OnceLock::new();
    *BYTES.get_or_init(|| {
        std::env::var("VICTORY_UMA_POOL_OPEN_MB")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(256)
            .saturating_mul(1 << 20)
    })
}
static FIXED: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
static REFUSED: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];
static VERSION: AtomicU64 = AtomicU64::new(1);
static LIVE: AtomicU64 = AtomicU64::new(0);
static CACHE: Mutex<BTreeMap<usize, Arc<PoolBuffer>>> = Mutex::new(BTreeMap::new());

static COMMITTED: AtomicU64 = AtomicU64::new(0);

/// Windows charges a pagefile section until its last owner lets go. A GPU
/// heap can outlive the title's handle and every CPU view of the section.
pub struct PoolCommit(u64);

impl PoolCommit {
    /// Reserve one slab of system commit before requesting the section.
    pub fn reserve(bytes: u64) -> Option<Self> {
        static LIMIT: OnceLock<u64> = OnceLock::new();
        let limit = *LIMIT.get_or_init(|| {
            std::env::var("VICTORY_UMA_POOL_COMMIT_MB")
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(8192)
                .saturating_mul(1 << 20)
        });
        COMMITTED
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |live| {
                live.checked_add(bytes).filter(|n| *n <= limit)
            })
            .ok()?;
        Some(Self(bytes))
    }

    /// Transfer the charge to the newly created Windows section.
    pub fn attach(self, handle: usize) {
        views().charges.insert(handle, Arc::new(self));
    }
}

impl Drop for PoolCommit {
    fn drop(&mut self) {
        COMMITTED.fetch_sub(self.0, Ordering::Relaxed);
    }
}

/// Openable physical sections are opt-in because Windows charges them at allocation.
pub fn uma_pool() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("VICTORY_UMA").is_some_and(|v| v == "1")
            && std::env::var_os("VICTORY_UMA_POOL").is_some_and(|v| v == "1")
    })
}

pub(crate) fn raw_reads() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        uma_pool() && std::env::var_os("VICTORY_UMA_POOL_RAW").is_none_or(|v| v == "1")
    })
}

pub(crate) fn constants() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        uma_pool() && std::env::var_os("VICTORY_UMA_POOL_CBV").is_none_or(|v| v == "1")
    })
}

pub(crate) fn changed() {
    if uma_pool() {
        VERSION.fetch_add(1, Ordering::Release);
    }
}

pub(crate) fn version() -> u64 {
    VERSION.load(Ordering::Acquire)
}

pub(crate) struct PoolBuffer {
    pub(crate) buffer: ID3D12Resource,
    _heap: ID3D12Heap,
    _charge: Option<Arc<PoolCommit>>,
    _opening: Opening,
    pub(crate) bytes: u64,
    pub(crate) gpu: u64,
}

struct Opening(u64);
impl Drop for Opening {
    fn drop(&mut self) {
        LIVE.fetch_sub(self.0, Ordering::Relaxed);
    }
}

/// Forget the handle before Windows can hand its value to another
/// section. Submitted lists own their resources through the GPU fence.
pub fn title_memory_section_closed(handle: usize) -> Option<Arc<PoolCommit>> {
    let mut v = views();
    v.sections.retain(|s| s.0 != handle);
    CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&handle);
    v.charges.remove(&handle)
}

/// An unbroken range of one committed section. A hole, a section
/// boundary or an exhausted budget leaves the caller its usual copy.
pub(crate) fn resolve(
    device: &ID3D12Device,
    at: usize,
    bytes: usize,
) -> Option<(Arc<PoolBuffer>, u64)> {
    lookup(device, at, bytes, true)
}

pub(crate) fn lookup(
    device: &ID3D12Device,
    at: usize,
    bytes: usize,
    open_new: bool,
) -> Option<(Arc<PoolBuffer>, u64)> {
    if bytes == 0 || at.checked_add(bytes).is_none() {
        return None;
    }
    let v = views();
    let (handle, offset) = match cover_in(&v.views, at, bytes) {
        Cover::Run(h, o) => (h, o),
        other => {
            let reason = match other {
                Cover::None => 0,
                Cover::Holes => 1,
                _ => 2,
            };
            REFUSED[reason].fetch_add(1, Ordering::Relaxed);
            return None;
        }
    };
    let &(_, size, true) = v.sections.iter().find(|s| s.0 == handle)? else {
        REFUSED[3].fetch_add(1, Ordering::Relaxed);
        return None;
    };
    let budget = limit();
    if size > budget {
        REFUSED[4].fetch_add(1, Ordering::Relaxed);
        return None;
    }
    if offset.checked_add(bytes as u64)? > size {
        return None;
    }
    // Holding the view table across open prevents close and handle reuse.
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(buffer) = cache.get(&handle) {
        return Some((buffer.clone(), offset));
    }
    if !open_new {
        return None;
    }
    while LIVE.load(Ordering::Relaxed) > budget - size {
        let Some(victim) = cache
            .iter()
            .find(|(_, b)| Arc::strong_count(b) == 1)
            .map(|(&h, _)| h)
        else {
            REFUSED[4].fetch_add(1, Ordering::Relaxed);
            return None;
        };
        cache.remove(&victim);
    }
    let result = open(device, From::Section(handle))
        .and_then(|h| place(device, &h, 0, size).map(|b| (h, b)));
    let Ok((heap, buffer)) = result else {
        REFUSED[5].fetch_add(1, Ordering::Relaxed);
        return None;
    };
    let gpu = unsafe { buffer.GetGPUVirtualAddress() };
    LIVE.fetch_add(size, Ordering::Relaxed);
    let buffer = Arc::new(PoolBuffer {
        buffer,
        _heap: heap,
        _charge: v.charges.get(&handle).cloned(),
        _opening: Opening(size),
        bytes: size,
        gpu,
    });
    cache.insert(handle, buffer.clone());
    Some((buffer, offset))
}

static STATS: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

/// Recording a fixed GPU address would bind the old physical offset
/// permanently. Count newly created copies for which that matters.
pub(crate) fn fixed_copy(at: u64, bytes: u64) {
    if uma_pool() {
        let v = views();
        let end = at.saturating_add(bytes) as usize;
        let reason = match cover_in(&v.views, at as usize, bytes as usize) {
            Cover::Run(..) => 0,
            Cover::Holes => 1,
            Cover::Scattered => 2,
            Cover::None if v.views.range(at as usize..end).next().is_some() => 1,
            Cover::None => return,
        };
        FIXED[reason].fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn note_directory(pages: usize) {
    STATS[0].store(pages as u64, Ordering::Relaxed);
}

pub(crate) fn note_read(bytes: usize) {
    STATS[1].fetch_add(bytes as u64, Ordering::Relaxed);
}

pub(crate) fn note_constant(bytes: usize) {
    STATS[2].fetch_add(bytes as u64, Ordering::Relaxed);
}

static RAW_TIMES: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

/// Count fence waits separately from page translation so a cheaper refresh
/// cannot hide a longer wait on the submitting thread.
#[cfg(feature = "diagnostics")]
pub(crate) fn note_preparation(settle: std::time::Duration, directory: std::time::Duration) {
    RAW_TIMES[0].fetch_add(1, Ordering::Relaxed);
    RAW_TIMES[1].fetch_add(settle.as_micros() as u64, Ordering::Relaxed);
    RAW_TIMES[2].fetch_add(directory.as_micros() as u64, Ordering::Relaxed);
}

pub(super) fn say() {
    let fixed = FIXED.each_ref().map(|n| n.swap(0, Ordering::Relaxed));
    let misses = REFUSED.each_ref().map(|n| n.swap(0, Ordering::Relaxed));
    let committed = views()
        .sections
        .iter()
        .filter(|s| s.2)
        .map(|s| s.1)
        .sum::<u64>();
    tracing::info!(
        committed_section_mib = committed >> 20,
        pool_charge_mib = COMMITTED.load(Ordering::Relaxed) >> 20,
        pool_budget_mib = limit() >> 20,
        fixed_remappable = fixed[0],
        fixed_partial = fixed[1],
        fixed_scattered = fixed[2],
        outside_views = misses[0],
        partial_ranges = misses[1],
        scattered_ranges = misses[2],
        reserved_sections = misses[3],
        over_budget = misses[4],
        driver_refusals = misses[5],
        pool_open_mib = LIVE.load(Ordering::Relaxed) >> 20,
        raw_submits = RAW_TIMES[0].swap(0, Ordering::Relaxed),
        raw_settle_us = RAW_TIMES[1].swap(0, Ordering::Relaxed),
        raw_directory_us = RAW_TIMES[2].swap(0, Ordering::Relaxed),
        raw_direct_pages = STATS[0].load(Ordering::Relaxed),
        raw_skipped_bytes = STATS[1].swap(0, Ordering::Relaxed),
        constant_fill_skipped_bytes = STATS[2].swap(0, Ordering::Relaxed),
        "UMA section reads since the previous sample"
    );
}
