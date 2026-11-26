//! Host pipelines are made by workers, not by the title thread that first
//! records a draw with them. Translating a shader and the host driver's
//! compile take from milliseconds to seconds each (structurisation of the
//! biggest compute programs ~0.4 s, some driver compiles 3.4 s), and
//! the title's first world frame wants over 1,200 new pipelines: made
//! on its recording threads, that frame took more than 30 s and the
//! title's GPU watchdog ended the run (decision 0007, "Pipelines made off
//! the title's threads").
//!
//! A first use queues the pipeline and waits for it within a budget per
//! frame, so the cheap ones (menus, one-off passes such as a colour-grading
//! bake) are there at once as before; past the budget a draw or dispatch
//! whose pipeline is not ready is dropped, as an untranslated one is.
//!
//! With `VICTORY_PIPELINE_STANDINS=1`, a pending graphics draw first tries
//! a finished pipeline with the same shaders and active state. A hit binds
//! immediately. A miss keeps the budget, including the one for one-off passes.
//!
//! The default budget is 4 ms per title frame (it was half a second): a
//! frame of the world met pipelines still being made in most frames while
//! the driver compiled in the background, and the title begins two or
//! three frames per frame it presents, so frames of 1.5 to 3 s came one
//! after the other in the prologue on the RTX 5080. A draw whose pipeline
//! is not ready is dropped until it is, as a game streams in what it has
//! not compiled yet. The decoration bake's pipelines, a pass the title runs
//! once, still get half a second (`VICTORY_PIPELINE_WAIT_ONCE_MS`).

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use super::Shader;
use super::host::{GraphicsState, HostPipeline, TargetFormats};

/// What a host pipeline is made from, and the host pipeline once made.
pub(crate) struct Pending {
    pub(super) shaders: Vec<Arc<Shader>>,
    pub(crate) graphics: Option<Box<GraphicsState>>,
    queue_state: AtomicU8,
    made: OnceLock<Option<HostPipeline>>,
    /// What identifies it across runs (shader containers and state), and
    /// whether a draw or dispatch has requested it yet.
    pub(super) key: String,
    used: AtomicBool,
    /// The formats its state declares for what a draw draws into (zeros
    /// without a graphics state).
    pub(crate) formats: TargetFormats,
    /// The same shaders and state for other formats, bound at a draw
    /// ([`for_formats`]): a chain from the title's pipeline, one link per
    /// set of formats met, never shortened.
    variant: OnceLock<Arc<Pending>>,
    compatibility: OnceLock<blake3::Hash>,
    donor: OnceLock<Arc<Pending>>,
    usage: OnceLock<super::library::Usage>,
    replayed: AtomicBool,
}

// SAFETY: D3D12 objects are free-threaded; the graphics state (whose input
// elements point into its own names) is only read after creation.
unsafe impl Send for Pending {}
// SAFETY: as above.
unsafe impl Sync for Pending {}

impl Pending {
    pub(crate) fn new(shaders: Vec<Arc<Shader>>, graphics: Option<Box<GraphicsState>>) -> Pending {
        let mut key = blake3::Hasher::new();
        for s in &shaders {
            key.update(s.stage.as_bytes());
            key.update(s.hash.as_bytes());
        }
        key.update(graphics.as_ref().map(|g| g.describe()).unwrap_or_default().as_bytes());
        Pending {
            formats: graphics.as_ref().map_or([0; 9], |g| g.formats()),
            variant: OnceLock::new(),
            shaders,
            graphics,
            queue_state: AtomicU8::new(NOT_QUEUED),
            made: OnceLock::new(),
            key: key.finalize().to_hex()[..32].to_owned(),
            used: AtomicBool::new(false),
            compatibility: OnceLock::new(),
            donor: OnceLock::new(),
            usage: OnceLock::new(),
            replayed: AtomicBool::new(false),
        }
    }

    fn make(self: &Arc<Self>) {
        if self.made.get().is_none() {
            let should_skip = is_bad_key(&self.key) || super::is_skipped_shaders(&self.shaders);
            let made = if should_skip {
                SKIPPED_BAD.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(key = self.key, shaders = ?self.shaders, "not made: pipeline is skipped or crashed before");
                None
            } else {
                guard_start();
                let slot = current_or_caller_slot();
                let _compile = Compile::begin(slot, self);
                let res = super::library::precompile::build(self, false);
                drop(_compile);
                res
            };
            let _ = self.made.set(made);
            if standins()
                && self.made.get().is_some_and(Option::is_some)
                && let Some(key) = self.compatibility()
            {
                donors()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(key, Arc::downgrade(self));
            }
        }
        let (lock, done) = &DONE;
        let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
        done.notify_all();
    }
}

/// Pipelines not made for being on the list of those the driver's compiler
/// crashes on.
static SKIPPED_BAD: AtomicU64 = AtomicU64::new(0);

/// The keys of the pipelines the driver's compiler crashed on (`warm-bad.txt`
/// beside the pipeline library, one key per line; `#` starts a comment), loaded
/// on first use. `VICTORY_WARM_BAD=0` leaves the file unread.
static BAD_KEYS: Mutex<Option<std::collections::HashSet<String>>> = Mutex::new(None);

fn bad_keys<R>(f: impl FnOnce(&mut std::collections::HashSet<String>) -> R) -> R {
    let mut keys = BAD_KEYS.lock().unwrap_or_else(|e| e.into_inner());
    let keys = keys.get_or_insert_with(|| {
        if std::env::var_os("VICTORY_WARM_BAD").is_some_and(|v| v == "0") {
            return Default::default();
        }
        let listed = super::library::folder()
            .and_then(|d| std::fs::read_to_string(d.join("warm-bad.txt")).ok())
            .unwrap_or_default();
        listed
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .filter_map(|l| l.split_whitespace().next())
            .map(str::to_owned)
            .collect()
    });
    f(keys)
}

fn is_bad_key(key: &str) -> bool {
    bad_keys(|keys| keys.contains(key))
}

/// Whether driver shader-compiler crashes on pipeline workers are caught
/// (`VICTORY_COMPILER_GUARD=0`: off).
fn guard_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_COMPILER_GUARD").is_none_or(|v| v != "0"))
}

const MAX_SLOTS: usize = 128;
static CRASHED: [AtomicBool; MAX_SLOTS] = [const { AtomicBool::new(false) }; MAX_SLOTS];
static ACTIVE_SLOTS: [AtomicBool; MAX_SLOTS] = [const { AtomicBool::new(false) }; MAX_SLOTS];
static FAULT_CODE: [AtomicU64; MAX_SLOTS] = [const { AtomicU64::new(0) }; MAX_SLOTS];
static FAULT_ADDR: [AtomicU64; MAX_SLOTS] = [const { AtomicU64::new(0) }; MAX_SLOTS];
static FAULT_RIP: [AtomicU64; MAX_SLOTS] = [const { AtomicU64::new(0) }; MAX_SLOTS];
static NEXT_SLOT: AtomicUsize = AtomicUsize::new(0);
static CRASH_EVENT: AtomicUsize = AtomicUsize::new(0);

struct ActiveJob {
    key: String,
    shaders: Vec<Arc<Shader>>,
    pending: Arc<Pending>,
}

static ACTIVE_JOBS: Mutex<Vec<Option<ActiveJob>>> = Mutex::new(Vec::new());

thread_local! {
    static WORKER_SLOT: std::cell::Cell<usize> = const { std::cell::Cell::new(usize::MAX) };
    static COMPILING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn current_or_caller_slot() -> usize {
    let mut slot = WORKER_SLOT.try_with(|s| s.get()).unwrap_or(usize::MAX);
    if slot >= MAX_SLOTS {
        slot = NEXT_SLOT.fetch_add(1, Ordering::Relaxed);
        if slot < MAX_SLOTS {
            let _ = WORKER_SLOT.try_with(|s| s.set(slot));
        }
    }
    slot
}

struct Compile {
    slot: usize,
}

impl Compile {
    fn begin(slot: usize, p: &Arc<Pending>) -> Compile {
        if slot < MAX_SLOTS && guard_on() {
            let mut jobs = ACTIVE_JOBS.lock().unwrap_or_else(|e| e.into_inner());
            if jobs.len() < MAX_SLOTS {
                jobs.resize_with(MAX_SLOTS, || None);
            }
            jobs[slot] = Some(ActiveJob {
                key: p.key.clone(),
                shaders: p.shaders.clone(),
                pending: p.clone(),
            });
            ACTIVE_SLOTS[slot].store(true, Ordering::Release);
            let _ = COMPILING.try_with(|c| c.set(true));
        }
        Compile { slot }
    }
}

impl Drop for Compile {
    fn drop(&mut self) {
        let compiling = COMPILING.try_with(|c| c.replace(false)).unwrap_or(false);
        if compiling && self.slot < MAX_SLOTS {
            ACTIVE_SLOTS[self.slot].store(false, Ordering::Release);
            let mut jobs = ACTIVE_JOBS.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(job) = jobs.get_mut(self.slot) {
                *job = None;
            }
        }
    }
}

/// Installs a lock-free Vectored Exception Handler that catches driver shader
/// compiler crashes on pipeline worker threads (NVIDIA `libnvidia-gpucomp.so`
/// `ILLEGAL_INSTRUCTION` / `ud2` under Proton/vkd3d-proton, and AMD
/// `ACCESS_VIOLATION` on low addresses). The handler does no allocation or
/// locking: it records the fault, wakes `victory-pipeline-watch`, and parks
/// the crashed worker while the watcher records the pipeline in `warm-bad.txt`
/// and spawns a replacement worker.
fn guard_start() {
    use windows_sys::Win32::System::Diagnostics::Debug::AddVectoredExceptionHandler;
    use windows_sys::Win32::System::Threading::CreateEventW;
    static ONCE: std::sync::Once = std::sync::Once::new();
    if !guard_on() {
        return;
    }
    ONCE.call_once(|| {
        // SAFETY: an unnamed auto-reset event, no security attributes.
        let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
        if event.is_null() {
            tracing::warn!("no event for driver compiler crashes: they are not caught");
            return;
        }
        CRASH_EVENT.store(event as usize, Ordering::Release);
        let spawned = std::thread::Builder::new().name("victory-pipeline-watch".into()).spawn(watch);
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "no watcher thread for driver compiler crashes: they are not caught");
            return;
        }
        // SAFETY: handler lives for the process lifetime.
        unsafe {
            AddVectoredExceptionHandler(1, Some(compiler_crashed));
        }
        tracing::info!("driver compiler crash guard active on pipeline workers (VICTORY_COMPILER_GUARD=0: off)");
    });
}

unsafe extern "system" fn compiler_crashed(
    info: *mut windows_sys::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS,
) -> i32 {
    use windows_sys::Win32::System::Threading::{SetEvent, Sleep};
    const ACCESS_VIOLATION: i32 = 0xc000_0005_u32 as i32;
    const ILLEGAL_INSTRUCTION: i32 = 0xc000_001d_u32 as i32;
    const CONTINUE_SEARCH: i32 = 0;
    // SAFETY: the OS passes a live exception record.
    let record = unsafe { &*(*info).ExceptionRecord };
    let is_av = record.ExceptionCode == ACCESS_VIOLATION;
    let is_sigill = record.ExceptionCode == ILLEGAL_INSTRUCTION;
    if !is_av && !is_sigill {
        return CONTINUE_SEARCH;
    }
    let mut slot = WORKER_SLOT.try_with(|s| s.get()).unwrap_or(usize::MAX);
    let compiling_on_thread = COMPILING.try_with(|c| c.get()).unwrap_or(false);
    if !compiling_on_thread || slot >= MAX_SLOTS {
        return CONTINUE_SEARCH;
    }
    let _ = COMPILING.try_with(|c| c.set(false));
    ACTIVE_SLOTS[slot].store(false, Ordering::Release);
    FAULT_CODE[slot].store(record.ExceptionCode as u32 as u64, Ordering::Relaxed);
    let addr = if record.NumberParameters >= 2 { record.ExceptionInformation[1] as u64 } else { 0 };
    FAULT_ADDR[slot].store(addr, Ordering::Relaxed);
    #[cfg(target_arch = "x86_64")]
    // SAFETY: the OS passes a live context record.
    FAULT_RIP[slot].store(unsafe { (*(*info).ContextRecord).Rip }, Ordering::Relaxed);
    CRASHED[slot].store(true, Ordering::Release);
    let event = CRASH_EVENT.load(Ordering::Acquire);
    if event != 0 {
        // SAFETY: event lives for the process lifetime.
        unsafe { SetEvent(event as windows_sys::Win32::Foundation::HANDLE) };
    }
    loop {
        // SAFETY: park this thread permanently so it never unwinds through corrupted driver state.
        unsafe { Sleep(u32::MAX - 1) };
    }
}

fn watch() {
    use windows_sys::Win32::System::Threading::{INFINITE, WaitForSingleObject};
    loop {
        let event = CRASH_EVENT.load(Ordering::Acquire);
        // SAFETY: event lives for the process lifetime.
        unsafe { WaitForSingleObject(event as windows_sys::Win32::Foundation::HANDLE, INFINITE) };
        for slot in 0..MAX_SLOTS {
            if CRASHED[slot].swap(false, Ordering::AcqRel) {
                lost(slot);
            }
        }
    }
}

fn lost(slot: usize) {
    let job = {
        let mut jobs = ACTIVE_JOBS.lock().unwrap_or_else(|e| e.into_inner());
        jobs.get_mut(slot).and_then(Option::take)
    };
    let Some(job) = job else {
        return;
    };
    tracing::error!(
        key = job.key,
        shaders = ?job.shaders,
        code = format_args!("{:#x}", FAULT_CODE[slot].load(Ordering::Relaxed)),
        rip = format_args!("{:#x}", FAULT_RIP[slot].load(Ordering::Relaxed)),
        address = format_args!("{:#x}", FAULT_ADDR[slot].load(Ordering::Relaxed)),
        "driver shader compiler crashed making a pipeline: skipping it and spawning replacement worker"
    );
    let known = bad_keys(|keys| !keys.insert(job.key.clone()));
    if !known && let Some(dir) = super::library::folder() {
        use std::io::Write;
        let _ = std::fs::create_dir_all(&dir);
        let file = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("warm-bad.txt"));
        if let Ok(mut f) = file {
            let _ = writeln!(f, "{} ended the compiler in a run: {:?}", job.key, job.shaders);
        }
    }
    let _ = job.pending.made.set(None);
    {
        let (lock, done) = &DONE;
        let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
        done.notify_all();
    }
    if dec_waiting() {
        drained();
    }
    if !spawn_worker() {
        tracing::warn!("failed to spawn replacement pipeline worker after compiler crash");
    }
}

fn standins() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_PIPELINE_STANDINS").is_some_and(|v| v == "1"))
}

/// Approach after XCL 625a0f3: share an existing driver pipeline across
/// compatible title objects. Here the donor actually draws. The same interned
/// shaders give it the same root bindings, resource accesses and mesh launch
/// convention. Only state that D3D12 ignores can differ.
impl Pending {
    fn compatibility(&self) -> Option<blake3::Hash> {
        let graphics = self.graphics.as_ref()?;
        if self.shaders.is_empty()
            || self
                .shaders
                .iter()
                .any(|s| !matches!(s.stage, "vertex" | "mesh" | "pixel"))
        {
            return None;
        }
        Some(*self.compatibility.get_or_init(|| {
            let mut hash = blake3::Hasher::new();
            hash.update(&self.shaders.len().to_le_bytes());
            for shader in &self.shaders {
                hash.update(&(Arc::as_ptr(shader) as usize).to_le_bytes());
            }
            hash.update(&graphics.cache_bytes(true));
            hash.finalize()
        }))
    }

    fn standin(&self) -> Option<&HostPipeline> {
        if let Some(real) = self.made.get() {
            return real.as_ref();
        }
        let key = self.compatibility()?;
        let donor = if let Some(p) = self.donor.get() {
            p
        } else {
            let mut table = donors().lock().unwrap_or_else(|e| e.into_inner());
            if let Some(real) = self.made.get() {
                return real.as_ref();
            }
            let Some(p) = table.get(&key)?.upgrade() else {
                table.remove(&key);
                return None;
            };
            self.donor.get_or_init(|| p)
        };
        let host = donor.made.get()?.as_ref()?;
        STANDIN_HITS.fetch_add(1, Ordering::Relaxed);
        Some(host)
    }

    /// The short key in the old early list was only a scheduling hint.
    /// Sharing a built object also requires matching the complete inputs.
    pub(super) fn same_inputs(&self, other: &Self) -> bool {
        self.shaders.len() == other.shaders.len()
            && self
                .shaders
                .iter()
                .zip(&other.shaders)
                .all(|(a, b)| Arc::ptr_eq(a, b))
            && match (&self.graphics, &other.graphics) {
                (None, None) => true,
                (Some(a), Some(b)) => a.cache_bytes(false) == b.cache_bytes(false),
                _ => false,
            }
    }
}

fn donors() -> &'static Mutex<HashMap<blake3::Hash, Weak<Pending>>> {
    static DONORS: OnceLock<Mutex<HashMap<blake3::Hash, Weak<Pending>>>> = OnceLock::new();
    DONORS.get_or_init(Mutex::default)
}

pub(super) static STANDIN_HITS: AtomicU64 = AtomicU64::new(0);
pub(super) static STANDIN_MISSES: AtomicU64 = AtomicU64::new(0);

/// Why a pipeline has no host pipeline (diagnostics): its shaders that do
/// not translate, else "refused" or "being made"; `None` if it has one.
#[cfg(feature = "diagnostics")]
pub(crate) fn missing(p: &Pending) -> Option<String> {
    let p = p.usage.get().and_then(|u| u.shared.as_deref()).unwrap_or(p);
    match p.made.get() {
        Some(Some(_)) => None,
        None => Some("being made".into()),
        Some(None) => {
            let failed: Vec<String> = p
                .shaders
                .iter()
                .filter(|s| matches!(s.translated.get(), Some(None)))
                .map(|s| format!("{} {}", s.stage, s.hash))
                .collect();
            Some(if failed.is_empty() {
                format!("refused {:?}", p.shaders)
            } else {
                failed.join(" + ")
            })
        }
    }
}

/// Signalled whenever a worker has made a pipeline.
static DONE: (Mutex<()>, Condvar) = (Mutex::new(()), Condvar::new());

/// Waiting allowed per title frame, over all threads
/// (`VICTORY_PIPELINE_WAIT_MS`, 4).
fn wait_budget() -> Duration {
    static BUDGET: OnceLock<Duration> = OnceLock::new();
    *BUDGET.get_or_init(|| {
        let millis = std::env::var("VICTORY_PIPELINE_WAIT_MS")
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(4);
        tracing::info!(millis, "host pipeline wait budget per frame");
        Duration::from_millis(millis)
    })
}

/// Waiting allowed per title frame for the pipelines of a pass the title
/// runs once (the decoration bake, `crate::draw::BAKE_PIXEL_SHADERS`):
/// `VICTORY_PIPELINE_WAIT_ONCE_MS`, 500.
fn once_budget() -> Duration {
    static BUDGET: OnceLock<Duration> = OnceLock::new();
    *BUDGET.get_or_init(|| {
        let millis = std::env::var("VICTORY_PIPELINE_WAIT_ONCE_MS")
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(500);
        Duration::from_millis(millis)
    })
}

impl Pending {
    /// Whether this is a pipeline of a pass the title runs once.
    fn runs_once(&self) -> bool {
        self.shaders.iter().any(|s| crate::draw::BAKE_PIXEL_SHADERS.contains(&s.hash.as_str()))
    }
}

/// Pipelines queued and not made yet.
static WAITING: AtomicU64 = AtomicU64::new(0);

fn dec_waiting() -> bool {
    let mut current = WAITING.load(Ordering::Relaxed);
    loop {
        if current == 0 {
            return false;
        }
        match WAITING.compare_exchange_weak(current, current - 1, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return current == 1,
            Err(actual) => current = actual,
        }
    }
}

/// Pipelines queued and not made yet (for the frame line).
pub(crate) fn waiting() -> u64 {
    WAITING.load(Ordering::Relaxed)
}

/// The host pipeline if it is made (or is made within the frame's budget);
/// queues it on first use.
pub(crate) fn ready(p: &Arc<Pending>) -> Option<&HostPipeline> {
    let p = if super::library::warm_enabled() {
        let usage = p.usage.get_or_init(|| super::library::usage(p));
        usage.calls.fetch_add(1, Ordering::Relaxed);
        usage.shared.as_ref().unwrap_or(p)
    } else {
        p
    };
    // A still-pending pipeline was requested by the title too. Prewarm it
    // on the next launch, even if this run never got to use it.
    if !p.used.load(Ordering::Relaxed) && !p.used.swap(true, Ordering::Relaxed) {
        super::library::note_used(&p.key);
        if p.replayed.load(Ordering::Relaxed) {
            match p.made.get() {
                Some(Some(_)) => {
                    REPLAY_READY.fetch_add(1, Ordering::Relaxed);
                }
                None => {
                    REPLAY_LATE.fetch_add(1, Ordering::Relaxed);
                }
                Some(None) => {} // Reported separately by missing().
            }
        }
    }
    if p.made.get().is_none() {
        start(p, REQUESTED);
        if standins() && p.compatibility().is_some() {
            if let Some(real) = p.made.get() {
                return real.as_ref();
            }
            if let Some(host) = p.standin() {
                return Some(host);
            }
            STANDIN_MISSES.fetch_add(1, Ordering::Relaxed);
        }
        wait(p, crate::frame::current());
    }
    p.made.get()?.as_ref()
}

/// Pipelines made again for the formats bound at a draw, since the start.
static VARIANTS: AtomicU64 = AtomicU64::new(0);

/// Whether a draw into views of other formats than its pipeline declares
/// gets a host pipeline for those formats ([`for_formats`]). On by
/// default; `VICTORY_TARGET_VARIANTS=0` sends such a draw to the host with
/// the title's pipeline, as before.
fn variants_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_TARGET_VARIANTS").is_none_or(|v| v != "0"))
}

/// The host pipeline for a draw with `p` into views of the formats
/// `bound`, which `p` does not declare (`host::formats_differ`): that of
/// the same shaders and state for those formats. It is a pipeline like the
/// title's own: queued at its first draw, made by a worker, waited for
/// within the frame's budget, and the draw is dropped while it is not
/// there ([`ready`]).
#[cold]
pub(crate) fn for_formats<'a>(p: &'a Arc<Pending>, bound: &TargetFormats) -> Option<&'a HostPipeline> {
    match p.graphics.as_deref() {
        Some(g) if variants_on() => ready(variant(p, g, &super::host::formats_for(&p.formats, bound))),
        _ => ready(p),
    }
}

/// The pipeline of `p`'s shaders and its state `g` for the formats
/// `bound`: found in the chain that starts at `p`, or added to it (logged
/// then: once per pipeline and set of formats).
fn variant<'a>(p: &'a Arc<Pending>, g: &GraphicsState, bound: &TargetFormats) -> &'a Arc<Pending> {
    let mut at = p;
    // (`bound` is what `host::formats_for` gives, and a state made for it
    // declares exactly it, so the walk ends at the link made for it; the
    // count is a bound all the same: past it the title's own pipeline.)
    for _ in 0..64 {
        let next = at.variant.get_or_init(|| {
            let so_far = VARIANTS.fetch_add(1, Ordering::Relaxed) + 1;
            tracing::info!(
                pipeline = ?p.shaders,
                declared = ?p.formats,
                bound = ?bound,
                so_far,
                "a pipeline is made again for the render target formats bound at its draw (eight render targets, then depth; VICTORY_TARGET_VARIANTS=0: the draw goes out with the title's pipeline)"
            );
            Arc::new(Pending::new(p.shaders.clone(), Some(Box::new(g.with_formats(bound)))))
        });
        // (A state made for `bound` declares exactly `bound`.)
        if next.formats == *bound {
            return next;
        }
        at = next;
    }
    p
}

/// Makes `p` on this thread if it is not made yet (waiting for a worker
/// that is making it); whether it has a host pipeline.
pub(crate) fn now(p: &Arc<Pending>) -> bool {
    let p = if super::library::warm_enabled() {
        let usage = p.usage.get_or_init(|| super::library::usage(p));
        usage.shared.as_ref().unwrap_or(p)
    } else {
        p
    };
    p.make();
    p.made.get().is_some_and(|made| made.is_some())
}

/// At the title's creation of a pipeline: starts it at once if an earlier
/// run drew with it (during loading, so the world's first frames find it).
pub(crate) fn early(mut p: Arc<Pending>) -> Arc<Pending> {
    if super::library::replay_enabled() {
        super::library::start_warmup();
    }
    if !p.shaders.is_empty() && super::library::wanted_early(&p.key) {
        if super::library::replay_enabled() {
            p = super::library::shared(p);
        }
        super::library::EARLY.fetch_add(1, Ordering::Relaxed);
        start(&p, EARLY);
    } else if super::library::replay_enabled()
        && let Some(shared) = super::library::replayed(&p)
    {
        return shared;
    }
    p
}

pub(super) fn replay(p: &Arc<Pending>) {
    p.replayed.store(true, Ordering::Relaxed);
    start(p, REPLAY);
}

pub(super) static REPLAY_READY: AtomicU64 = AtomicU64::new(0);
pub(super) static REPLAY_LATE: AtomicU64 = AtomicU64::new(0);

/// Whether the frame's budget is one for all threads together: a thread
/// that waits takes what is left of it and gives back what it did not
/// use. Counted only when a wait had ended, the budget was one per thread:
/// each of the title's five recording threads began its first wait of a
/// frame with nothing counted yet and waited the whole half second, 2.5 s
/// of waiting in every frame in which pipelines were being made (runs
/// 34-49: "2504 ms waiting for pipelines" in each such frame).
/// On by default; `VICTORY_PIPELINE_WAIT_SHARED=0` counts as before.
fn shared_budget() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_PIPELINE_WAIT_SHARED").is_none_or(|v| v != "0"))
}

/// Waits for `p` while frame `frame` has budget left.
fn wait(p: &Pending, frame: u64) {
    let budget = if p.runs_once() { once_budget().max(wait_budget()) } else { wait_budget() };
    if budget.is_zero() {
        return;
    }
    // The frame the budget belongs to, and what was waited of it (or is
    // taken by a thread waiting now).
    static SPENT: Mutex<(u64, Duration)> = Mutex::new((u64::MAX, Duration::ZERO));
    let shared = shared_budget();
    let left = {
        let mut spent = SPENT.lock().unwrap_or_else(|e| e.into_inner());
        if spent.0 != frame {
            *spent = (frame, Duration::ZERO);
        }
        let left = budget.saturating_sub(spent.1);
        if shared {
            spent.1 += left;
        }
        left
    };
    if left.is_zero() {
        return;
    }
    let started = Instant::now();
    let (lock, done) = &DONE;
    let mut held = lock.lock().unwrap_or_else(|e| e.into_inner());
    while p.made.get().is_none() {
        let rest = left.saturating_sub(started.elapsed());
        if rest.is_zero() {
            break;
        }
        held = done.wait_timeout(held, rest).unwrap_or_else(|e| e.into_inner()).0;
    }
    drop(held);
    let waited = started.elapsed().min(left);
    {
        let mut spent = SPENT.lock().unwrap_or_else(|e| e.into_inner());
        // A frame that began meanwhile has its own budget.
        if spent.0 == frame {
            if shared {
                spent.1 = spent.1.saturating_sub(left - waited);
            } else {
                spent.1 += waited;
            }
        }
    }
    if crate::monitor::enabled() {
        crate::monitor::PIPELINE_WAIT_US.fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
    }
    crate::framelog::add(crate::framelog::T::PipelineWaitUs, started.elapsed().as_micros() as u64);
    #[cfg(feature = "diagnostics")]
    crate::copystat::add(
        crate::copystat::C::PipelineWaitUs,
        started.elapsed().as_micros() as u64,
    );
}

const NOT_QUEUED: u8 = 0;
const EARLY: u8 = 1;
const REPLAY: u8 = 2;
const REQUESTED: u8 = 3;
const MAKING: u8 = 4;

struct Queue {
    early: VecDeque<Arc<Pending>>,
    replay: VecDeque<Arc<Pending>>,
    requested: VecDeque<Arc<Pending>>,
}

impl Queue {
    const fn new() -> Self {
        Self {
            early: VecDeque::new(),
            replay: VecDeque::new(),
            requested: VecDeque::new(),
        }
    }

    /// Queues an unrequested pipeline or promotes one queued at creation.
    /// Returns whether it is new to the queue, or `None` if nothing changed.
    fn push(&mut self, p: &Arc<Pending>, priority: u8) -> Option<bool> {
        let prev = p.queue_state.load(Ordering::Relaxed);
        if prev >= priority {
            return None;
        }
        p.queue_state.store(priority, Ordering::Relaxed);
        if priority == REQUESTED {
            self.requested.push_back(p.clone());
        } else if priority == REPLAY {
            self.replay.push_back(p.clone());
        } else {
            self.early.push_back(p.clone());
        }
        Some(prev == NOT_QUEUED)
    }

    fn pop(&mut self) -> Option<Arc<Pending>> {
        let p = self.requested.pop_front().or_else(|| {
            std::iter::from_fn(|| self.replay.pop_front())
                .find(|p| p.queue_state.load(Ordering::Relaxed) == REPLAY)
        }).or_else(|| {
            // An early entry can have been promoted and claimed already.
            std::iter::from_fn(|| self.early.pop_front())
                .find(|p| p.queue_state.load(Ordering::Relaxed) == EARLY)
        })?;
        p.queue_state.store(MAKING, Ordering::Relaxed);
        Some(p)
    }
}

static QUEUE: Mutex<Queue> = Mutex::new(Queue::new());
static AVAILABLE: Condvar = Condvar::new();

/// Start or promote work for a draw without waiting behind the entire early
/// list. Queue changes and worker claims happen under the same lock.
fn start(p: &Arc<Pending>, priority: u8) {
    if p.queue_state.load(Ordering::Relaxed) >= priority {
        return;
    }
    if !workers() {
        if p.queue_state.swap(MAKING, Ordering::Relaxed) == NOT_QUEUED {
            p.make();
        }
        return;
    }
    let mut queue = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(new) = queue.push(p, priority) {
        if new {
            PEAK.fetch_max(WAITING.fetch_add(1, Ordering::Relaxed) + 1, Ordering::Relaxed);
        }
        AVAILABLE.notify_one();
    }
}

fn next() -> Arc<Pending> {
    let mut queue = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if let Some(p) = queue.pop() {
            return p;
        }
        queue = AVAILABLE.wait(queue).unwrap_or_else(|e| e.into_inner());
    }
}

/// Starts the workers (half the logical processors, 2 to 8, or VICTORY_PIPELINE_WORKERS) on first use.
fn workers() -> bool {
    static RUNNING: OnceLock<bool> = OnceLock::new();
    *RUNNING.get_or_init(|| {
        let mut running = false;
        let workers = if let Some(val) = std::env::var_os("VICTORY_PIPELINE_WORKERS") {
            val.to_str().and_then(|s| s.parse::<usize>().ok()).unwrap_or(2).clamp(1, 16)
        } else {
            std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).clamp(2, 8))
        };
        guard_start();
        tracing::info!(workers, "starting pipeline workers");
        for _ in 0..workers {
            if spawn_worker() {
                running = true;
            }
        }
        running
    })
}

fn spawn_worker() -> bool {
    let slot = NEXT_SLOT.fetch_add(1, Ordering::Relaxed);
    let spawned = std::thread::Builder::new()
        .name(format!("victory-pipelines-{slot}"))
        .stack_size(32 * 1024 * 1024)
        .spawn(move || {
            WORKER_SLOT.set(slot);
            // Below the title's own threads (loading, recording).
            // SAFETY: a plain call on our own thread.
            unsafe {
                use windows::Win32::System::Threading::*;
                let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
            }
            loop {
                let p = next();
                p.make();
                if dec_waiting() {
                    drained();
                } else {
                    super::library::save(false);
                }
            }
        });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "no pipeline worker");
        false
    } else {
        true
    }
}

/// The most pipelines waiting at once since the workers last ran out.
static PEAK: AtomicU64 = AtomicU64::new(0);

/// The workers ran out of work: logged after a batch of 10 or more, and
/// the cache saved (at most every 20 s).
fn drained() {
    use super::library::{EARLY, LOADED, MADE};
    if PEAK.swap(0, Ordering::Relaxed) >= 10 {
        tracing::info!(
            made = MADE.load(Ordering::Relaxed),
            loaded = LOADED.load(Ordering::Relaxed),
            started_early = EARLY.load(Ordering::Relaxed),
            "host pipelines all made"
        );
    }
    super::library::save(false);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A first use queues the pipeline and, within the budget, gets the
    /// worker's answer (here: no host pipeline for no shaders).
    #[test]
    fn first_use_is_made_by_a_worker() {
        let p = Arc::new(Pending::new(Vec::new(), None));
        assert!(ready(&p).is_none());
        assert_ne!(p.queue_state.load(Ordering::Relaxed), NOT_QUEUED);
        let started = Instant::now();
        while p.made.get().is_none() && started.elapsed() < Duration::from_secs(10) {
            std::thread::yield_now();
        }
        assert!(matches!(p.made.get(), Some(None)));
    }

    /// A draw into other formats than its pipeline declares finds the
    /// pipeline for them, which exists once per set of formats and is
    /// queued by its first draw like any other.
    #[test]
    fn a_pipeline_for_bound_formats_is_made_once() {
        // The title's pipeline declares one render target, format 24.
        let p = Arc::new(Pending::new(Vec::new(), Some(Box::new(GraphicsState::declaring(24)))));
        assert_eq!(p.formats, [24, 0, 0, 0, 0, 0, 0, 0, 0]);
        let g = p.graphics.as_deref().unwrap();
        let (a, b) = ([26, 0, 0, 0, 0, 0, 0, 0, 0], [28, 0, 0, 0, 0, 0, 0, 0, 0]);
        let first = variant(&p, g, &a);
        assert_eq!(first.formats, a);
        assert_ne!(first.key, p.key);
        assert!(Arc::ptr_eq(first, variant(&p, g, &a)));
        let second = variant(&p, g, &b);
        assert_eq!(second.formats, b);
        assert!(!Arc::ptr_eq(first, second));
        assert_ne!(second.key, first.key);
        assert!(Arc::ptr_eq(first, variant(&p, g, &a)));
        assert!(Arc::ptr_eq(second, variant(&p, g, &b)));
        assert_eq!(first.queue_state.load(Ordering::Relaxed), NOT_QUEUED);
        assert_eq!(p.queue_state.load(Ordering::Relaxed), NOT_QUEUED);
    }

    #[test]
    fn pending_pipeline_is_recorded_for_the_next_launch() {
        let mut p = Pending::new(Vec::new(), None);
        p.key = "pending-pipeline-test".to_owned();
        // Simulate a worker that has taken the job but cannot finish it yet.
        p.queue_state.store(MAKING, Ordering::Relaxed);
        let p = Arc::new(p);

        assert!(!super::super::library::wanted_early(&p.key));
        assert!(ready(&p).is_none());
        assert!(p.made.get().is_none());
        assert!(super::super::library::wanted_early(&p.key));
    }

    #[test]
    fn draws_promote_early_pipelines_without_claiming_them_twice() {
        let earlier = Arc::new(Pending::new(Vec::new(), None));
        let promoted = Arc::new(Pending::new(Vec::new(), None));
        let requested = Arc::new(Pending::new(Vec::new(), None));
        let mut queue = Queue::new();

        assert_eq!(queue.push(&earlier, EARLY), Some(true));
        assert_eq!(queue.push(&promoted, EARLY), Some(true));
        assert_eq!(queue.push(&requested, REQUESTED), Some(true));
        assert_eq!(queue.push(&promoted, REQUESTED), Some(false));
        assert_eq!(queue.push(&promoted, REQUESTED), None);

        assert!(Arc::ptr_eq(&queue.pop().unwrap(), &requested));
        assert!(Arc::ptr_eq(&queue.pop().unwrap(), &promoted));
        assert_eq!(queue.push(&promoted, REQUESTED), None);
        assert!(Arc::ptr_eq(&queue.pop().unwrap(), &earlier));
        assert!(queue.pop().is_none());
    }

    #[test]
    fn requested_work_precedes_ranked_replay_then_creation_order() {
        let pending = || Arc::new(Pending::new(Vec::new(), None));
        let (early, hot, cold, live) = (pending(), pending(), pending(), pending());
        let mut q = Queue::new();
        q.push(&early, EARLY);
        q.push(&live, EARLY);
        q.push(&hot, REPLAY);
        q.push(&cold, REPLAY);
        assert_eq!(q.push(&live, REPLAY), Some(false));
        assert_eq!(q.push(&live, REQUESTED), Some(false));
        for p in [&live, &hot, &cold, &early] {
            assert!(Arc::ptr_eq(&q.pop().unwrap(), p));
        }
        assert!(q.pop().is_none());
    }

    #[test]
    fn pending_graphics_can_bind_the_donor_then_the_real_pipeline() {
        use victory_shader::{HwStage, emit::HostShader};
        use windows::Win32::Graphics::Direct3D12::*;
        use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_D32_FLOAT;
        use windows::core::s;

        if crate::host::host().is_none() {
            return;
        }
        let dxil = crate::host::compile(
            "float4 main(uint i : SV_VertexID) : SV_Position { return float4(i,0,0,1); }",
            s!("main"),
            s!("vs_5_1"),
        )
        .unwrap();
        let shader = Arc::new(Shader {
            stage: "vertex",
            hash: "synthetic-standin".into(),
            container: vec![],
            translated: OnceLock::from(Some(super::super::Translated {
                used: vec![],
                stores: false,
                host: HostShader {
                    stage: HwStage::Vs,
                    dxil,
                    user_data: vec![],
                    root_cbvs: vec![],
                    root_srvs: vec![],
                    root_uavs: vec![],
                    root_indexed_views: vec![],
                    samplers: vec![],
                    static_samplers: vec![],
                },
            })),
        });
        let make = |stencil_mask| {
            let mut g = GraphicsState::default();
            g.topology = D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE;
            let format = [16u32, DXGI_FORMAT_D32_FLOAT.0 as u32];
            #[repr(C)]
            struct Depth(u32, D3D12_DEPTH_STENCIL_DESC);
            let keep = D3D12_DEPTH_STENCILOP_DESC {
                StencilFailOp: D3D12_STENCIL_OP_KEEP,
                StencilDepthFailOp: D3D12_STENCIL_OP_KEEP,
                StencilPassOp: D3D12_STENCIL_OP_KEEP,
                StencilFunc: D3D12_COMPARISON_FUNC_ALWAYS,
            };
            let depth = Depth(
                11,
                D3D12_DEPTH_STENCIL_DESC {
                    DepthEnable: true.into(),
                    DepthWriteMask: D3D12_DEPTH_WRITE_MASK_ALL,
                    DepthFunc: D3D12_COMPARISON_FUNC_LESS,
                    StencilWriteMask: stencil_mask,
                    FrontFace: keep,
                    BackFace: keep,
                    ..Default::default()
                },
            );
            // SAFETY: a complete depth-format subobject.
            unsafe { g.read(16, format.as_ptr().cast()) };
            // SAFETY: a complete depth-stencil subobject.
            unsafe { g.read(11, (&raw const depth).cast()) };
            Arc::new(Pending::new(vec![shader.clone()], Some(Box::new(g))))
        };
        let donor = make(0xff);
        assert!(now(&donor));
        let p = make(1);
        assert!(!p.same_inputs(&donor));
        let key = p.compatibility().unwrap();
        assert_eq!(Some(key), donor.compatibility());
        donors().lock().unwrap().insert(key, Arc::downgrade(&donor));
        let fallback = p.standin().unwrap() as *const HostPipeline;
        assert!(std::ptr::eq(
            fallback,
            donor.made.get().unwrap().as_ref().unwrap()
        ));
        if standins() {
            p.queue_state.store(MAKING, Ordering::Relaxed);
            assert!(std::ptr::eq(fallback, ready(&p).unwrap()));
            assert!(p.made.get().is_none());
        }
        drop(donor);
        assert!(std::ptr::eq(fallback, p.standin().unwrap()));
        assert!(now(&p));
        assert!(!std::ptr::eq(fallback, p.standin().unwrap()));
        let refused = make(2);
        assert!(refused.made.set(None).is_ok());
        assert!(refused.standin().is_none());
        assert!(Pending::new(vec![shader], None).compatibility().is_none());
    }
}
