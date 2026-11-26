//! Approach after XCL b86fba4: measure successful host flips and show pipeline
//! backlog alongside their cadence. Workers and the GPU overlap one another.
//! Approach after XCL 5a5c32b: keep guest frame tokens separate from flip times.
//! Deltas below cover the wall interval between flips, including work the
//! title has already recorded for a later frame.

use crate::{draw, frame_wait, host::Host, pipeline, shadow};
use kelvaroq_render::stats_ring::{ABSENT, FIELDS, Writer};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{LazyLock, Once};
use std::time::{Duration, Instant};

pub(crate) fn enabled() -> bool {
    static ON: LazyLock<bool> = LazyLock::new(|| {
        std::env::var_os("VICTORY_STATS_SHM").map_or(cfg!(feature = "diagnostics"), |v| v != "0")
    });
    *ON
}

/// Existing logs still count from line to line. Keeping the source total
/// lets the present thread sample it without racing a reset.
pub(crate) fn log_count(source: &AtomicU64, last: &AtomicU64) -> u64 {
    if !enabled() {
        return source.swap(0, Relaxed);
    }
    let n = source.load(Relaxed);
    n.saturating_sub(last.fetch_max(n, Relaxed))
}

static PRESENT_NS: AtomicU64 = AtomicU64::new(0);
static TITLE_NS: AtomicU64 = AtomicU64::new(0);
thread_local! {static RETURNED: Cell<Option<Instant>> = const { Cell::new(None) };}

pub(crate) fn title_return(start: Instant, ns: u64) {
    let end = start + Duration::from_nanos(ns);
    if let Some(previous) = RETURNED.replace(Some(end)) {
        TITLE_NS.fetch_add(
            start.saturating_duration_since(previous).as_nanos() as u64,
            Relaxed,
        );
    }
    PRESENT_NS.fetch_add(ns, Relaxed);
}

pub(crate) static PIPELINE_WAIT_US: AtomicU64 = AtomicU64::new(0);
static PAGES: AtomicU64 = AtomicU64::new(0);

/// A placed buffer may begin partway through a 64 KiB page. Each placement
/// owns its covering pages until its last reference goes away.
pub(crate) fn placed_pages(start: u64, bytes: u64, add: bool) {
    let pages = (u128::from(start % 65536) + u128::from(bytes.max(1))).div_ceil(65536) as u64;
    if add {
        PAGES.fetch_add(pages, Relaxed);
    } else {
        PAGES.fetch_sub(pages, Relaxed);
    }
}

#[cfg(feature = "diagnostics")]
static DROPS: [AtomicU64; 7] = [const { AtomicU64::new(0) }; 7];
#[cfg(any(test, feature = "diagnostics"))]
fn reason(why: &str) -> usize {
    if why == "being made" {
        0
    } else if why == "no pipeline set" {
        1
    } else if why.starts_with("refused") {
        2
    } else if why.starts_with("skipped shader") {
        3
    } else if why.starts_with("indirect") {
        4
    } else if why.contains("structure builder") {
        5
    } else {
        6
    } // translation failed; the engine log retains stage and hash
}
#[cfg(feature = "diagnostics")]
pub(crate) fn drop_reason(why: &str) {
    DROPS[reason(why)].fetch_add(1, Relaxed);
}

static MEMORY: [AtomicU64; 3] = [const { AtomicU64::new(ABSENT) }; 3];
fn memory_sampler() {
    use windows_sys::Win32::System::{
        ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
        Threading::GetCurrentProcess,
    };
    loop {
        if let Some([local, _]) = crate::host::host().and_then(Host::video_memory_groups) {
            MEMORY[0].store(local[0], Relaxed);
            MEMORY[1].store(local[1], Relaxed);
        } else {
            MEMORY[0].store(ABSENT, Relaxed);
            MEMORY[1].store(ABSENT, Relaxed);
        }
        let mut ram = PROCESS_MEMORY_COUNTERS {
            cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            ..Default::default()
        };
        // SAFETY: a correctly sized structure and our own process handle.
        let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut ram, ram.cb) };
        MEMORY[2].store(
            if ok != 0 {
                ram.WorkingSetSize as u64
            } else {
                ABSENT
            },
            Relaxed,
        );
        std::thread::sleep(Duration::from_secs(1));
    }
}

macro_rules! fields {
    ($($name:ident),+ $(,)?) => {
        #[allow(non_camel_case_types, dead_code)] // Some sources only exist with diagnostics.
        #[repr(usize)]
        enum Field { $($name),+ }
        const NAMES: &[&str] = &[$(stringify!($name)),+];
    };
}
fields!(
    frame,
    timestamp_us,
    frame_us,
    present_status,
    pipelines_making,
    textures_waiting,
    vram_budget_bytes,
    vram_usage_bytes,
    ram_bytes,
    monitor_prev_ns,
    title_us,
    present_us,
    submit_other_us,
    submit_idle_us,
    submit_lock_us,
    submit_card_us,
    relay_wait_us,
    submit_fetch_us,
    submit_dependency_us,
    descriptors_us,
    watch_us,
    deferred_us,
    snapshots_us,
    card_setup_us,
    execute_us,
    gpu_busy_us,
    gpu_idle_behind_us,
    draws,
    drawn,
    dispatches,
    dispatched,
    drop_pipeline,
    drop_indirect,
    pipeline_wait_us,
    texture_upload_bytes,
    to_card_bytes,
    from_card_bytes,
    relay_work_us,
    relay_card_us,
    relay_fetch_us,
    title_copy_bytes,
    refresh_copy_bytes,
    watch_calls,
    watch_pages,
    watch_probes,
    watch_query_us,
    watch_checked_bytes,
    drop_compiling,
    drop_unset,
    drop_refused,
    drop_skipped,
    drop_signature,
    drop_builder,
    drop_translation
);

/// Additional gauges appear in the reader and in CSV by name. Another branch
/// can append a source here without changing the protocol or the launcher.
const EXTRA: &[(&str, fn() -> u64)] = &[
    ("inplace_bytes", crate::inplace::monitor_bytes),
    ("inplace_pages", || PAGES.load(Relaxed)),
];
pub(crate) struct Recorder {
    ring: Writer,
    start: Instant,
    previous: Option<Instant>,
    totals: [u64; FIELDS],
    cost: u64,
}
impl Recorder {
    pub(crate) fn open() -> Option<Self> {
        if !enabled() {
            return None;
        }
        let names: Vec<_> = NAMES
            .iter()
            .copied()
            .chain(EXTRA.iter().map(|s| s.0))
            .collect();
        let ring = Writer::create(std::process::id(), &names)
            .map_err(|e| tracing::warn!(error=%e,"frame monitor mapping unavailable"))
            .ok()?;
        static START: Once = Once::new();
        START.call_once(|| {
            if let Err(e) = std::thread::Builder::new()
                .name("victory-memory-meter".into())
                .spawn(memory_sampler)
            {
                tracing::warn!(error=%e,"memory sampling unavailable");
            }
        });
        Some(Self {
            ring,
            start: Instant::now(),
            previous: None,
            totals: [ABSENT; FIELDS],
            cost: ABSENT,
        })
    }

    pub(crate) fn present(&mut self, host: &Host, token: u64, status: i32, now: Instant) {
        use Field::*;
        if status != 0 {
            self.previous = None;
            // Keep occluded and failed presents in the trace. Neither end
            // of an interval crossing one measures consecutive flips.
            let mut row = [ABSENT; FIELDS];
            row[frame as usize] = token;
            row[timestamp_us as usize] = (now - self.start).as_micros() as u64;
            row[present_status as usize] = u64::from(status as u32);
            row[monitor_prev_ns as usize] = self.cost;
            self.ring.publish(&row[..NAMES.len() + EXTRA.len()]);
            self.cost = now.elapsed().as_nanos() as u64;
            return;
        }
        let elapsed = self
            .previous
            .replace(now)
            .map(|t| (now - t).as_micros() as u64);
        let mut row = [ABSENT; FIELDS];
        let mut put = |f: Field, n| row[f as usize] = n;
        put(frame, token);
        put(present_status, 0);
        put(timestamp_us, (now - self.start).as_micros() as u64);
        put(frame_us, elapsed.unwrap_or(ABSENT));
        put(monitor_prev_ns, self.cost);
        put(title_us, TITLE_NS.load(Relaxed) / 1000);
        put(present_us, PRESENT_NS.load(Relaxed) / 1000);
        if let Some(times) = frame_wait::monitor_worker(false) {
            for (field, n) in [
                submit_other_us,
                submit_idle_us,
                submit_lock_us,
                submit_card_us,
                relay_wait_us,
                submit_fetch_us,
                submit_dependency_us,
                descriptors_us,
                watch_us,
                deferred_us,
                snapshots_us,
                card_setup_us,
                execute_us,
            ]
            .into_iter()
            .zip(times)
            {
                put(field, n);
            }
        }
        if let Some(times) = frame_wait::monitor_worker(true) {
            put(relay_work_us, times[frame_wait::Stage::Work as usize]);
            put(relay_card_us, times[frame_wait::Stage::Card as usize]);
            put(relay_fetch_us, times[frame_wait::Stage::Fetch as usize]);
        }
        if let Some([n, pages]) = crate::watch::monitor_asks() {
            put(watch_calls, n);
            put(watch_pages, pages);
        }
        if let Some([idle, behind]) = host.monitor_card_idle() {
            put(gpu_busy_us, idle);
            put(gpu_idle_behind_us, behind);
        }
        let [issued_draw, issued_dispatch, indirect] = crate::list::monitor_counts();
        put(draws, issued_draw);
        put(dispatches, issued_dispatch);
        put(drawn, draw::drawn());
        put(dispatched, draw::dispatched());
        put(drop_indirect, indirect);
        put(drop_pipeline, draw::DROPPED.load(Relaxed));
        put(pipelines_making, pipeline::making());
        put(pipeline_wait_us, PIPELINE_WAIT_US.load(Relaxed));
        put(textures_waiting, draw::HEAP_TEXTURES_QUEUE[2].load(Relaxed));
        put(texture_upload_bytes, draw::HEAP_TEXTURE_BYTES.load(Relaxed));
        put(to_card_bytes, shadow::CARD_STATS[0].load(Relaxed));
        put(from_card_bytes, shadow::CARD_STATS[4].load(Relaxed));
        for (field, v) in [vram_budget_bytes, vram_usage_bytes, ram_bytes]
            .into_iter()
            .zip(&MEMORY)
        {
            put(field, v.load(Relaxed));
        }
        #[cfg(feature = "diagnostics")]
        {
            use crate::copystat::{C, monitor_count};
            put(title_copy_bytes, monitor_count(C::CopyBytes));
            put(refresh_copy_bytes, monitor_count(C::RefreshBytes));
            put(watch_probes, monitor_count(C::Watches));
            put(watch_query_us, monitor_count(C::WatchUs));
            put(watch_checked_bytes, monitor_count(C::Checked));
            for (f, n) in [
                drop_compiling,
                drop_unset,
                drop_refused,
                drop_skipped,
                drop_signature,
                drop_builder,
                drop_translation,
            ]
            .into_iter()
            .zip(&DROPS)
            {
                put(f, n.load(Relaxed));
            }
        }
        for (i, v) in row
            .iter_mut()
            .enumerate()
            .take(NAMES.len())
            .skip(title_us as usize)
        {
            let prior = std::mem::replace(&mut self.totals[i], *v);
            *v = delta(*v, prior, elapsed.is_some());
        }
        if row[gpu_busy_us as usize] != ABSENT {
            row[gpu_busy_us as usize] = elapsed.unwrap().saturating_sub(row[gpu_busy_us as usize]);
        }
        for (i, (_, read)) in EXTRA.iter().enumerate() {
            row[NAMES.len() + i] = read();
        }
        self.ring.publish(&row[..NAMES.len() + EXTRA.len()]);
        self.cost = now.elapsed().as_nanos() as u64;
    }
}
fn delta(value: u64, prior: u64, continuous: bool) -> u64 {
    if !continuous || value == ABSENT || prior == ABSENT {
        ABSENT
    } else {
        value.saturating_sub(prior)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn placement_pages_balance_across_unaligned_ends() {
        let base = PAGES.load(Relaxed);
        placed_pages(65535, 2, true);
        assert_eq!(PAGES.load(Relaxed), base + 2);
        placed_pages(65535, 2, false);
        assert_eq!(PAGES.load(Relaxed), base);
    }
    #[test]
    fn missing_and_disjoint_intervals_stay_missing() {
        assert_eq!(delta(8, 3, true), 5);
        for (v, p, ok) in [(8, ABSENT, true), (ABSENT, 3, true), (8, 3, false)] {
            assert_eq!(delta(v, p, ok), ABSENT);
        }
    }
    #[test]
    fn synthetic_flip_samples_include_only_successful_intervals() {
        // Diagnostics or an explicit opt-in exercises the same collector as DXGI.
        if !enabled() {
            assert!(Recorder::open().is_none());
            return;
        }
        let Some(host) = crate::host::host() else {
            return;
        };
        let mut monitor = Recorder::open().expect("open synthetic publisher");
        let reader = kelvaroq_render::stats_ring::Reader::open(std::process::id()).unwrap();
        let mut times = Vec::new();
        frame_wait::start_worker(false);
        for i in 1..=4096 {
            monitor.present(host, i, 0, Instant::now());
            times.push(monitor.cost);
        }
        monitor.present(host, 4097, 1, Instant::now());
        let failed = reader.read(4097).unwrap();
        assert_eq!(failed[Field::present_status as usize], 1);
        assert_eq!(failed[Field::frame_us as usize], ABSENT);
        monitor.present(host, 4098, 0, Instant::now());
        let row = reader.read(4098).unwrap();
        assert_eq!(row[Field::frame as usize], 4098);
        assert_eq!(row[Field::frame_us as usize], ABSENT);
        times.sort_unstable();
        println!(
            "Synthetic present sample + publish: mean {} ns, p99 {} ns (4096 frames)",
            times.iter().sum::<u64>() / 4096,
            times[4055]
        );
    }
    #[test]
    fn drop_categories_match_the_recording_sites() {
        for (i, s) in [
            "being made",
            "no pipeline set",
            "refused []",
            "skipped shader 00",
            "indirect kind 1",
            "a stage of the ray-tracing structure builder, which is off",
            "vertex 00",
        ]
        .iter()
        .enumerate()
        {
            assert_eq!(reason(s), i);
        }
    }
}
