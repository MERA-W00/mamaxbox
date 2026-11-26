//! Wall time spent on each side of a frame handoff. The two workers have
//! one clock each. Reading a clock splits even a long sleep at the meter's
//! one-second boundary. Title times sum completed calls across threads.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
pub(crate) enum Stage {
    Work,
    Idle,
    Lock,
    Card,
    Landing,
    Fetch,
    Dependency,
    Descriptors,
    Watch,
    Deferred,
    Snapshots,
    CardSetup,
    Execute,
}

struct Clock {
    since: Instant,
    stage: Stage,
    spent: [Duration; 13],
    archived: [Duration; 13],
}

impl Clock {
    fn tick(&mut self) {
        self.tick_at(Instant::now());
    }

    fn tick_at(&mut self, now: Instant) {
        self.spent[self.stage as usize] += now - self.since;
        self.since = now;
    }
}

#[cfg(test)]
#[test]
fn a_submit_stage_excludes_a_nested_wait_at_the_frame_boundary() {
    let start = Instant::now();
    let mut clock = Clock { since: start, stage: Stage::Deferred, spent: Default::default(), archived: Default::default() };
    clock.tick_at(start + Duration::from_millis(3));
    clock.stage = Stage::Landing;
    clock.tick_at(start + Duration::from_millis(8));
    let first = std::mem::take(&mut clock.spent);
    clock.tick_at(start + Duration::from_millis(10));
    clock.stage = Stage::Deferred;
    clock.tick_at(start + Duration::from_millis(14));
    assert_eq!(first[Stage::Deferred as usize], Duration::from_millis(3));
    assert_eq!(first[Stage::Landing as usize], Duration::from_millis(5));
    assert_eq!(clock.spent[Stage::Landing as usize], Duration::from_millis(2));
    assert_eq!(clock.spent[Stage::Deferred as usize], Duration::from_millis(4));
}

type Worker = Mutex<Option<Clock>>;
static SUBMIT: Worker = Mutex::new(None);
static RELAY: Worker = Mutex::new(None);
thread_local! {
    static WORKER: Cell<Option<&'static Worker>> = const { Cell::new(None) };
    static HELPER: Cell<bool> = const { Cell::new(false) };
    static IN_PRESENT: Cell<bool> = const { Cell::new(false) };
}

pub(crate) fn start_worker(relay: bool) {
    let worker = if relay { &RELAY } else { &SUBMIT };
    *worker.lock().unwrap() = Some(Clock {
        since: Instant::now(),
        stage: Stage::Work,
        spent: Default::default(),
        archived: Default::default(),
    });
    WORKER.set(Some(worker));
}

pub(crate) fn start_helper() {
    HELPER.set(true);
}

fn stage(next: Stage) -> Option<Stage> {
    let worker = WORKER.get()?;
    let mut clock = worker.lock().unwrap();
    let clock = clock.as_mut()?;
    clock.tick();
    Some(std::mem::replace(&mut clock.stage, next))
}

fn submit_stages() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_SUBMIT_STAGES").is_none_or(|v| v != "0"))
}

pub(crate) struct SubmitStage(Option<Stage>);

impl Drop for SubmitStage {
    fn drop(&mut self) {
        if let Some(previous) = self.0 {
            stage(previous);
        }
    }
}

/// The submit worker accounted for 86 ms of work per heavy frame. Use
/// its existing clock so a nested fence wait is charged only to the wait.
pub(crate) fn submit_stage(next: Stage) -> SubmitStage {
    let on_worker = WORKER.get().is_some_and(|worker| std::ptr::eq(worker, &SUBMIT));
    SubmitStage(if on_worker && submit_stages() { stage(next) } else { None })
}

/// Read the worker without interrupting a stage or waiting on a busy lock.
pub(crate) fn monitor_worker(relay: bool) -> Option<[u64; 13]> {
    let worker = if relay { &RELAY } else { &SUBMIT };
    let held = worker.try_lock().ok()?;
    let clock = held.as_ref()?;
    let mut spent = clock.spent;
    spent[clock.stage as usize] += clock.since.elapsed();
    Some(std::array::from_fn(|i| (spent[i] + clock.archived[i]).as_micros() as u64))
}

static DRIVER_BATCHES: Mutex<(u64, u64)> = Mutex::new((0, 0));

pub(crate) fn executing(lists: usize) -> SubmitStage {
    let timer = submit_stage(Stage::Execute);
    if timer.0.is_some() {
        let mut counts = DRIVER_BATCHES.lock().unwrap();
        counts.0 += 1;
        counts.1 += lists as u64;
    }
    timer
}

pub(crate) fn during<T>(next: Stage, f: impl FnOnce() -> T) -> T {
    let previous = stage(next);
    let result = f();
    if let Some(previous) = previous {
        stage(previous);
    }
    result
}

/// Only a contended queue, flight or submission lock starts a timer.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.try_lock() {
        Ok(guard) => guard,
        Err(TryLockError::Poisoned(e)) => e.into_inner(),
        Err(TryLockError::WouldBlock) => during(Stage::Lock, || {
            mutex.lock().unwrap_or_else(|e| e.into_inner())
        }),
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Call {
    Present,
    Drain,
    Releaser,
    Pacing,
    QueueFence,
    FenceLanding,
    ValueLanding,
    EarlyBarriers,
    Show,
    PresentTurn,
}

static NS: [AtomicU64; 10] = [const { AtomicU64::new(0) }; 10];

// These are title thread IDs from the same clock window as NS. Summing
// two blocked title threads can exceed a whole frame, so show them apart.
static TITLE_THREADS: Mutex<Vec<(u32, [u64; 4])>> = Mutex::new(Vec::new());

pub(crate) struct Timer(Call, Instant);

impl Drop for Timer {
    fn drop(&mut self) {
        let ns = self.1.elapsed().as_nanos() as u64;
        NS[self.0 as usize].fetch_add(ns, Ordering::Relaxed);
        if matches!(self.0, Call::Present) {
            IN_PRESENT.set(false);
            if crate::monitor::enabled() { crate::monitor::title_return(self.1, ns); }
        }
        let field = match self.0 {
            Call::Present => 0,
            Call::QueueFence => 1,
            Call::FenceLanding => 2,
            Call::ValueLanding => 3,
            _ => return,
        };
        let id = crate::host::thread_id();
        let mut threads = TITLE_THREADS.lock().unwrap();
        match threads.iter_mut().find(|t| t.0 == id) {
            Some((_, sums)) => sums[field] += ns,
            None => {
                let mut sums = [0; 4];
                sums[field] = ns;
                threads.push((id, sums));
            }
        }
    }
}

pub(crate) fn time(call: Call) -> Timer {
    if matches!(call, Call::Present) {
        IN_PRESENT.set(true);
    }
    Timer(call, Instant::now())
}

pub(crate) fn present_releaser() -> Option<Timer> {
    IN_PRESENT.get().then(|| time(Call::Releaser))
}

pub(crate) fn title_value() -> Option<Timer> {
    (WORKER.get().is_none() && !HELPER.get()).then(|| time(Call::ValueLanding))
}

static QUEUED: AtomicU64 = AtomicU64::new(0);

pub(crate) fn queued() {
    QUEUED.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn line(frames: f64) {
    let queued = QUEUED.swap(0, Ordering::Relaxed) as f64 / frames;
    let ms = |ns: u64| ns as f64 / 1e6 / frames;
    let [
        present,
        drain,
        releaser,
        pacing,
        queue_fence,
        fence,
        value,
        barriers,
        show,
        turn,
    ] = NS.each_ref().map(|n| ms(n.swap(0, Ordering::Relaxed)));
    tracing::info!(
        "title calls per frame ({queued:.2} presents returned early per displayed frame), in ms: present {present:.1}, of which queue drain or capture {drain:.1} \
         (waiting for the releaser {releaser:.1}) and frame limit {pacing:.1}, previous caller {turn:.1}, host present {show:.1}; \
         queue fence wait call {queue_fence:.1}, blocking fence {fence:.1}, \
         memory-value settle {value:.1} (may be inside the drain)"
    );
    let callers = std::mem::take(&mut *TITLE_THREADS.lock().unwrap());
    for (id, ns) in callers {
        let times = ns.map(ms);
        if times.iter().any(|&t| t >= 0.5) {
            let [present, queue, fence, value] = times;
            tracing::info!(
                "title thread {id} per frame in ms: present {present:.1}, queue fence call {queue:.1}, \
                 blocking fence {fence:.1}, memory-value settle {value:.1}"
            );
        }
    }
    for (name, worker) in [("submit", &SUBMIT), ("relay", &RELAY)] {
        let mut guard = worker.lock().unwrap();
        let Some(clock) = guard.as_mut() else {
            continue;
        };
        clock.tick();
        let times = std::mem::take(&mut clock.spent);
        if crate::monitor::enabled() {
            for (sum, t) in clock.archived.iter_mut().zip(times) { *sum += t; }
        }
        drop(guard);
        let [other, idle, locks, card, landing, fetch, dependency, descriptors, watch, deferred, snapshots, setup, execute] =
            times.map(|t| t.as_secs_f64() * 1e3 / frames);
        let work = other + descriptors + watch + deferred + snapshots + setup + execute;
        let stages = if name == "submit" && submit_stages() {
            let (calls, lists) = std::mem::take(&mut *DRIVER_BATCHES.lock().unwrap());
            format!(", work by stage: descriptors {descriptors:.2}, watch and coherence {watch:.2}, deferred copies and barriers {deferred:.2}, snapshots {snapshots:.2}, card setup {setup:.2}, ExecuteCommandLists {execute:.2} ({:.1} calls/frame, {:.2} lists/call), other {other:.2}", calls as f64 / frames, lists as f64 / calls.max(1) as f64)
        } else {
            String::new()
        };
        tracing::info!(
            "{name} thread per frame in ms: work and other time {work:.1}, asleep awaiting work {idle:.1}, \
             asleep with an unmet queue fence or value {dependency:.1}, queue and submission locks {locks:.1}, card fence {card:.1}, awaiting relay {landing:.1}, fetch fence {fetch:.1}{stages}"
        );
    }
    if barriers > 0.0 {
        tracing::info!(
            "recording barriers before waiting for the relay took {barriers:.2} ms per frame"
        );
    }
}
