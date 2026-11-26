//! Compiled host pipelines kept on disk, and the title pipelines worth
//! starting early (decision 0007, "Pipeline cache").
//!
//! 1. Every host pipeline goes into the host's pipeline library, named by
//!    a hash of all that its host description holds (the translated
//!    shaders' code, which changes when a translation does, our root
//!    signature, the graphics state). The library is saved under the cache
//!    folder; the host refuses a library of another adapter or driver, and
//!    an entry whose description differs, so neither is ever used.
//! 2. The title pipelines a run drew or dispatched with are listed by a key
//!    known when the title creates them (their shader containers and
//!    graphics state); the next run hands those to the workers at
//!    creation, during loading. `VICTORY_PIPELINE_EARLY_WITHOUT_LIBRARY=1`
//!    also saves these keys on hosts without a D3D12 pipeline library.
//! 3. Unless `VICTORY_PIPELINE_WARMUP=0`, a run saves the source containers
//!    and graphics state of requested pipelines. With `VICTORY_PIPELINE_WARMUP=1` a
//!    reader feeds these recipes to the workers, most used first. A change
//!    to the translator rebuilds from the original containers.
//!
//! Library calls overlap: the host documents the library as safe to use
//! from several threads ("The pipeline library is thread-safe to use, and
//! will internally synchronize as necessary", `CreatePipelineLibrary`,
//! Thread Safety), with one exception kept here: two loads of the same
//! pipeline are made one after the other ([`same_name`]). Under one lock of
//! ours for every call, the eight workers made pipelines one at a time:
//! 87 to 96 of 110 looks at each worker found it waiting for that lock
//! (run 49), and an earlier run's 2,300 pipelines, all in the library, were
//! still being loaded 46 s into the opening cutscene.
//! `VICTORY_LIBRARY_LOCK=1`: one call at a time, as before.
//!
//! The library's file is mapped, not read into memory ([`Blob`]): the host
//! keeps pointing into the bytes it was made from for as long as it lives,
//! and read into a buffer they were 1,352 MB of the process's own memory
//! for the whole run (run 60), all of it in physical memory. Mapped, the
//! pages are the file's: only those the host reads are in memory, and the
//! system can drop them without writing them anywhere. A save writes the
//! new library through a mapping of the new file in the same way (a buffer
//! of the library's size was made, filled and written out at each save).
//! `VICTORY_LIBRARY_MAP=0`: read and written through buffers, as before.
//!
//! The mapped file's handle stays open for as long as the mapping lives
//! ([`holds`]). A save renames the new library over the file this run has
//! mapped, and with the handle closed Windows then gives up the old file's
//! contents: every page of the mapping that is not in memory at that
//! moment, or leaves memory later (the trim below, or Windows taking pages
//! back), reads as zeros from then on. The host's library reads such pages
//! as stored pipelines; the driver, handed a pipeline of zeros, removes the
//! device with DXGI_ERROR_DRIVER_INTERNAL_ERROR (runs 72, 73 and 82, each
//! within 5 ms of the first library call after a save and a trim; and the
//! library run 77 saved from such pages was refused by run 78). Measured
//! with a 64 MB file written past the system's cache, mapped and replaced:
//! 16,352 of 16,384 pages read zeros with the handle closed, none with it
//! open (the test below). `VICTORY_LIBRARY_HANDLE=0`: closed, as before.

pub(super) mod precompile;

use std::collections::{HashMap, HashSet};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, RwLock};

use super::make::Pending;

use windows::Win32::Graphics::Direct3D12::*;
use windows::core::{HSTRING, Interface, PCWSTR};

static DIR: OnceLock<PathBuf> = OnceLock::new();

/// Keeps the pipeline library and the early list in `dir` from now on.
pub fn use_pipeline_cache(dir: PathBuf) {
    let _ = DIR.set(dir);
}

/// Host pipelines created, loaded from the library, and title pipelines
/// started at creation, since the start.
pub(crate) static MADE: AtomicU64 = AtomicU64::new(0);
pub(crate) static LOADED: AtomicU64 = AtomicU64::new(0);
pub(crate) static EARLY: AtomicU64 = AtomicU64::new(0);
/// For the memory lines: the bytes of the library's file, kept in memory
/// while the library lives, and the bytes the last save wrote (a buffer of
/// that size is made, filled and written every time pipelines were added,
/// at most every 20 s).
pub(crate) static BLOB: AtomicU64 = AtomicU64::new(0);
pub(crate) static SAVED: AtomicU64 = AtomicU64::new(0);

/// Whether the library's file is mapped ([`Blob`]) and saved through a
/// mapping. On by default; `VICTORY_LIBRARY_MAP=0` uses buffers, as before.
fn mapped() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_LIBRARY_MAP").is_none_or(|v| v != "0"))
}

/// Whether the mapped library file's handle is kept open while it is
/// mapped (see the module's notes: without it a save in the same run turns
/// the mapping's pages into zeros as they leave memory). On by default;
/// `VICTORY_LIBRARY_HANDLE=0` closes it after mapping, as before.
fn holds() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_LIBRARY_HANDLE").is_none_or(|v| v != "0"))
}

/// A whole file mapped into memory: read-only (the library's file) or
/// writable (the file a save fills). Unmapped when dropped.
struct Mapped {
    at: windows_sys::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS,
    len: usize,
    /// The file, for a mapping made by [`Mapped::open`] ([`holds`]): open
    /// until after the view is unmapped, so that the file a rename took
    /// the name from keeps its contents for the view.
    _file: Option<std::fs::File>,
}

impl Mapped {
    /// Maps `file` (`len` bytes, not zero) for reading, or for reading and
    /// writing.
    fn map(file: &std::fs::File, len: usize, write: bool) -> Option<Mapped> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, FILE_MAP_READ, FILE_MAP_WRITE, MapViewOfFile, PAGE_READONLY, PAGE_READWRITE,
        };
        if len == 0 {
            return None;
        }
        // SAFETY: a file we hold open; the view keeps the file's pages
        // after the mapping object and the file are closed.
        let at = unsafe {
            let mapping = CreateFileMappingW(
                file.as_raw_handle(),
                std::ptr::null(),
                if write { PAGE_READWRITE } else { PAGE_READONLY },
                0,
                0,
                std::ptr::null(),
            );
            if mapping.is_null() {
                return None;
            }
            let at = MapViewOfFile(mapping, if write { FILE_MAP_WRITE } else { FILE_MAP_READ }, 0, 0, 0);
            CloseHandle(mapping);
            at
        };
        (!at.Value.is_null()).then_some(Mapped { at, len, _file: None })
    }

    /// The file at `path`, mapped for reading; `None` if it is not there,
    /// is empty or cannot be mapped.
    fn open(path: &std::path::Path) -> Option<Mapped> {
        Mapped::open_holding(path, holds())
    }

    /// As [`Mapped::open`]; `hold` says whether the file stays open with
    /// the mapping.
    fn open_holding(path: &std::path::Path, hold: bool) -> Option<Mapped> {
        // Opened as the standard library opens a file to read: others may
        // read, write, rename and delete it meanwhile, so a save still
        // renames the new library over this one.
        let file = std::fs::File::open(path).ok()?;
        let len = usize::try_from(file.metadata().ok()?.len()).ok()?;
        let mut mapped = Mapped::map(&file, len, false)?;
        if hold {
            mapped._file = Some(file);
        }
        Some(mapped)
    }

    /// A new file of `len` bytes at `path` (replacing what is there),
    /// mapped for writing.
    fn create(path: &std::path::Path, len: usize) -> Option<Mapped> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .ok()?;
        file.set_len(len as u64).ok()?;
        Mapped::map(&file, len, true)
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: the view holds `len` readable bytes until dropped.
        unsafe { std::slice::from_raw_parts(self.at.Value.cast(), self.len) }
    }

    /// Only for a view made by [`Mapped::create`].
    fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: a writable view of `len` bytes, ours alone.
        unsafe { std::slice::from_raw_parts_mut(self.at.Value.cast(), self.len) }
    }
}

impl Drop for Mapped {
    fn drop(&mut self) {
        // SAFETY: our view.
        unsafe { windows_sys::Win32::System::Memory::UnmapViewOfFile(self.at) };
    }
}

/// The bytes a library is made from: the host reads them for as long as
/// the library lives, so they are kept beside it.
enum Blob {
    /// No file (an empty library).
    None,
    /// The file read into memory (`VICTORY_LIBRARY_MAP=0`, tests, or a file
    /// that could not be mapped).
    Held(Vec<u8>),
    /// The file itself, mapped.
    Mapped(Mapped),
}

impl Blob {
    /// The library file in `dir`: a library a save left beside it
    /// ([`NEXT_FILE`]) takes its place first.
    fn of(dir: &std::path::Path) -> Blob {
        let mut path = dir.join(LIBRARY_FILE);
        let next = dir.join(NEXT_FILE);
        if next.exists() && std::fs::rename(&next, &path).is_err() {
            tracing::warn!("the host pipeline library saved last could not take the old one's place");
            if precompile::tracking() {
                // The completion list describes this newer generation.
                path = next;
            }
        }
        if mapped()
            && let Some(file) = Mapped::open(&path)
        {
            return Blob::Mapped(file);
        }
        match std::fs::read(&path) {
            Ok(bytes) if !bytes.is_empty() => Blob::Held(bytes),
            _ => Blob::None,
        }
    }

    fn bytes(&self) -> &[u8] {
        match self {
            Blob::None => &[],
            Blob::Held(bytes) => bytes,
            Blob::Mapped(file) => file.bytes(),
        }
    }
}

/// The mapped library file's view (address, bytes; 0: not mapped), when a
/// pipeline was last loaded from the library (`crate::host::now_ms`), and
/// whether the view's pages were let go since ([`trim`]).
static VIEW: [std::sync::atomic::AtomicUsize; 2] = [const { std::sync::atomic::AtomicUsize::new(0) }; 2];
static LOADED_AT: AtomicU64 = AtomicU64::new(0);
static TRIMMED: AtomicBool = AtomicBool::new(false);
/// The bytes of the view let go so far, for the memory line.
pub(crate) static TRIMMED_BYTES: AtomicU64 = AtomicU64::new(0);
/// When the view's pages were last let go (`crate::host::now_ms`), and how
/// often and when a save of this run last renamed a new library over the
/// mapped file: for [`removal_note`].
static TRIMMED_AT: AtomicU64 = AtomicU64::new(0);
static REPLACED: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];

/// The library calls running now: thread, what (load, make, store,
/// serialise), the pipeline, and since when (`crate::host::now_us`).
static CALLS: Mutex<Vec<(u32, &'static str, String, u64)>> = Mutex::new(Vec::new());

thread_local! {
    /// The pipeline this thread is making (its shaders), for [`CALLS`].
    static LABEL: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

/// Names the pipeline this thread makes next.
pub(crate) fn label(text: String) {
    LABEL.with(|l| *l.borrow_mut() = text);
}

/// A library call of this thread, listed in [`CALLS`] until dropped.
struct Call(u32, u64);

fn call(what: &'static str, name: &str) -> Call {
    let (thread, since) = (crate::host::thread_id(), crate::host::now_us());
    let label = LABEL.with(|l| l.borrow().clone());
    CALLS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((thread, what, format!("{name} {label}"), since));
    Call(thread, since)
}

impl Drop for Call {
    fn drop(&mut self) {
        CALLS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|c| (c.0, c.3) != (self.0, self.1));
    }
}

/// For the report when the host device is removed: the library calls
/// running at that moment, whether this run replaced the mapped file and
/// let its pages go, and how many pages of the mapping read as zeros now.
/// A library holds no page of zeros (the 259 MB one of run 83 has no run of
/// 2,048 zero bytes): each such page is one whose contents were lost (see
/// the module's notes), and a pipeline stored there is what the driver was
/// handed.
pub(crate) fn removal_note() -> String {
    use std::fmt::Write;
    let now = crate::host::now_us();
    let mut text = String::from("pipeline library: ");
    {
        let calls = CALLS.lock().unwrap_or_else(|e| e.into_inner());
        if calls.is_empty() {
            text.push_str("no call running");
        }
        for (i, (thread, what, name, since)) in calls.iter().enumerate() {
            let _ = write!(
                text,
                "{}{what} of {name} running for {:.1} ms on thread {thread}",
                if i == 0 { "" } else { ", " },
                now.saturating_sub(*since) as f64 / 1000.0
            );
        }
    }
    let (at, len) = (VIEW[0].load(Ordering::Relaxed), VIEW[1].load(Ordering::Relaxed));
    if at == 0 {
        text.push_str("; its file is not mapped");
        return text;
    }
    let ago = |ms: u64| crate::host::now_ms().saturating_sub(ms) as f64 / 1000.0;
    let _ = write!(
        text,
        "; its file is mapped ({len} bytes), the handle {}",
        if holds() { "kept open" } else { "closed (VICTORY_LIBRARY_HANDLE=0)" }
    );
    match REPLACED[0].load(Ordering::Relaxed) {
        0 => text.push_str("; this run has not replaced the file"),
        n => {
            let _ = write!(
                text,
                "; this run replaced the file under the mapping {n} times, last {:.1} s ago",
                ago(REPLACED[1].load(Ordering::Relaxed))
            );
        }
    }
    if TRIMMED_BYTES.load(Ordering::Relaxed) > 0 {
        let _ = write!(text, "; its pages were last let go {:.1} s ago", ago(TRIMMED_AT.load(Ordering::Relaxed)));
    }
    // Within two seconds: pages not in memory come from the disk.
    let started = std::time::Instant::now();
    // SAFETY: the view lives as long as the library, which is never dropped.
    let bytes = unsafe { std::slice::from_raw_parts(at as *const u8, len) };
    let (mut zero, mut looked, mut first) = (0u64, 0u64, None);
    for (n, page) in bytes.chunks(4096).enumerate() {
        if n % 4096 == 0 && started.elapsed() > std::time::Duration::from_secs(2) {
            break;
        }
        looked += 1;
        if page.iter().all(|&b| b == 0) {
            zero += 1;
            if first.is_none() {
                first = Some(n);
            }
        }
    }
    let _ = write!(text, "; {zero} of the {looked} pages looked at read as zeros now");
    if let Some(first) = first {
        let _ = write!(text, " (the first is page {first}): the library's contents were lost there");
    }
    text
}

/// Lets the mapped library file's pages go from the process's memory once
/// no pipeline has been loaded for ten seconds. The host reads nearly the
/// whole file while the early pipelines load (run 65: all 1,490 MB of it
/// were in the process's memory a minute in, and 1,300 MB still at the
/// end, on a PC with 2 GB free); after that it reads the file again only
/// when a new pipeline loads. Pages let go stay in Windows' file cache
/// while nothing else wants the memory, and come back on the next read.
/// Called once a second by the sweep; does its work on a thread of its own
/// (hundreds of thousands of pages), once after each burst of loads.
/// `VICTORY_LIBRARY_TRIM=0`: the pages stay until Windows takes them.
pub(crate) fn trim() {
    report_warmup();
    static ON: OnceLock<bool> = OnceLock::new();
    let (at, len) = (VIEW[0].load(Ordering::Relaxed), VIEW[1].load(Ordering::Relaxed));
    if at == 0
        || TRIMMED.load(Ordering::Relaxed)
        || !*ON.get_or_init(|| std::env::var_os("VICTORY_LIBRARY_TRIM").is_none_or(|v| v != "0"))
    {
        return;
    }
    let quiet_ms = crate::host::ram_idle_ms(crate::host::available_physical_ram());
    if crate::host::now_ms() < LOADED_AT.load(Ordering::Relaxed) + quiet_ms
        || TRIMMED.swap(true, Ordering::Relaxed)
    {
        return;
    }
    let work = move || {
        // SAFETY: the view lives as long as the library, which is never
        // dropped. Unlocking pages that are not locked fails, as expected,
        // and takes them out of the working set: the documented way to let
        // pages of a mapped file go.
        unsafe { windows_sys::Win32::System::Memory::VirtualUnlock(at as *const _, len) };
        TRIMMED_BYTES.fetch_add(len as u64, Ordering::Relaxed);
        TRIMMED_AT.store(crate::host::now_ms(), Ordering::Relaxed);
        crate::host::trail(|| format!("pipeline library: the mapped file's pages let go from memory ({len} bytes)"));
    };
    if std::thread::Builder::new().name("victory-library-trim".into()).spawn(work).is_err() {
        TRIMMED.store(false, Ordering::Relaxed);
    }
}

struct Library {
    library: ID3D12PipelineLibrary1,
    /// The blob the library was made from: it must outlive the library.
    _blob: Blob,
    /// Pipelines stored since the last save.
    added: AtomicU64,
    /// Stores share it and a save takes it alone: the size the library
    /// asks for must still be its size when it is written out.
    saving: RwLock<()>,
}

// SAFETY: D3D12 objects are free-threaded, and the library synchronises
// its own calls (see the module's notes); what it does not, two loads of
// one name, is serialised by `same_name`.
unsafe impl Send for Library {}
// SAFETY: as above; the other fields are an atomic and a lock.
unsafe impl Sync for Library {}

/// Held by a load of the pipeline named `name`: loads of the same name
/// wait for each other (the host's one exception to the library's thread
/// safety: such a load may change that pipeline's entry). Names share 64
/// locks, so two different names seldom wait.
fn same_name(name: &str) -> MutexGuard<'static, ()> {
    static LOADING: [Mutex<()>; 64] = [const { Mutex::new(()) }; 64];
    let at = name.bytes().fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(usize::from(b)));
    LOADING[at % LOADING.len()].lock().unwrap_or_else(|e| e.into_inner())
}

/// With `VICTORY_LIBRARY_LOCK=1`: held by every library call, as before.
fn one_call() -> Option<MutexGuard<'static, ()>> {
    static ON: OnceLock<bool> = OnceLock::new();
    static CALLING: Mutex<()> = Mutex::new(());
    ON.get_or_init(|| std::env::var_os("VICTORY_LIBRARY_LOCK").is_some_and(|v| v == "1"))
        .then(|| CALLING.lock().unwrap_or_else(|e| e.into_inner()))
}

const LIBRARY_FILE: &str = "host-pipelines.bin";
/// A saved library that could not replace [`LIBRARY_FILE`] while this run
/// had it mapped (a system that refuses to replace a mapped file): the next
/// start renames it over the library first ([`Blob::of`]).
const NEXT_FILE: &str = "host-pipelines.next";
const EARLY_FILE: &str = "early-pipelines.txt";
/// Title pipelines listed for an early start, at most.
const EARLY_MAX: usize = 8192;

fn library() -> Option<&'static Library> {
    static LIBRARY: OnceLock<Option<Library>> = OnceLock::new();
    LIBRARY
        .get_or_init(|| {
            let device = crate::host::host()?.device.cast::<ID3D12Device1>().ok()?;
            if precompile::tracking() && !precompile::writer(DIR.get()?) {
                return None;
            }
            let blob = DIR.get().map_or(Blob::None, |d| Blob::of(d));
            let library = open(&device, blob);
            if library.is_none() {
                tracing::warn!("no host pipeline library: pipelines are not kept");
            }
            if let (Some(lib), Some(dir)) = (&library, DIR.get())
                && precompile::tracking() && !lib._blob.bytes().is_empty()
            {
                precompile::restore(dir);
            }
            library
        })
        .as_ref()
}

/// The library in `blob`, or an empty one if the host rejects the blob (an
/// adapter or driver change, damage).
fn open(device: &ID3D12Device1, blob: Blob) -> Option<Library> {
    let make = |bytes: &[u8]| -> windows::core::Result<ID3D12PipelineLibrary1> {
        // SAFETY: the bytes outlive the library (kept beside it).
        unsafe {
            device.CreatePipelineLibrary::<ID3D12PipelineLibrary>(bytes)?.cast()
        }
    };
    if !blob.bytes().is_empty() {
        match make(blob.bytes()) {
            Ok(library) => {
                let bytes = blob.bytes().len();
                BLOB.store(bytes as u64, Ordering::Relaxed);
                tracing::info!(bytes, mapped = matches!(blob, Blob::Mapped(_)), "host pipeline library loaded");
                if let Blob::Mapped(file) = &blob {
                    VIEW[1].store(file.len, Ordering::Relaxed);
                    VIEW[0].store(file.at.Value as usize, Ordering::Relaxed);
                    LOADED_AT.store(crate::host::now_ms(), Ordering::Relaxed);
                }
                return Some(Library {
                    library,
                    _blob: blob,
                    added: AtomicU64::new(0),
                    saving: RwLock::new(()),
                });
            }
            Err(e) => tracing::info!(error = %e, "host pipeline library rejected; starting an empty one"),
        }
    } else {
        tracing::info!("no host pipeline library yet; starting an empty one");
    }
    // The rejected bytes are let go (a mapped file is unmapped) before the
    // empty library is made: nothing reads them any more.
    drop(blob);
    let library = make(&[]).ok()?;
    Some(Library {
        library,
        _blob: Blob::None,
        added: AtomicU64::new(0),
        saving: RwLock::new(()),
    })
}

/// A pipeline's name in the library: the hash of what its host
/// description holds.
pub(crate) fn name(kind: &str, root: &[u8], code: &[&[u8]], state: &str) -> String {
    let mut h = blake3::Hasher::new();
    for part in [kind.as_bytes(), root].into_iter().chain(code.iter().copied()).chain([state.as_bytes()]) {
        h.update(&(part.len() as u64).to_le_bytes());
        h.update(part);
    }
    h.finalize().to_hex()[..32].to_owned()
}

/// The pipeline named `name`: loaded from the library by `load` if it is
/// there with the same description, else made by `create` and stored.
pub(crate) fn pipeline(
    name: &str,
    load: impl Fn(&ID3D12PipelineLibrary1, PCWSTR) -> windows::core::Result<ID3D12PipelineState>,
    create: impl FnOnce() -> windows::core::Result<ID3D12PipelineState>,
) -> windows::core::Result<ID3D12PipelineState> {
    let wide = HSTRING::from(name);
    let Some(library) = library() else {
        MADE.fetch_add(1, Ordering::Relaxed);
        return create();
    };
    let began = std::time::Instant::now();
    let loaded = {
        let _all = one_call();
        let _one = same_name(name);
        let _call = call("load", name);
        load(&library.library, PCWSTR(wide.as_ptr()))
    };
    // The load read the library's file (see `trim`).
    LOADED_AT.store(crate::host::now_ms(), Ordering::Relaxed);
    TRIMMED.store(false, Ordering::Relaxed);
    let refused = match loaded {
        Ok(pso) => {
            precompile::hit();
            LOADED.fetch_add(1, Ordering::Relaxed);
            crate::host::trail(|| {
                let label = LABEL.with(|l| l.borrow().clone());
                format!("pipeline library: loaded {label} in {:.1} ms", began.elapsed().as_secs_f64() * 1e3)
            });
            return Ok(pso);
        }
        Err(e) => e,
    };
    // Compiled outside any lock: compiles take up to seconds.
    let pso = {
        let _call = call("make", name);
        create()?
    };
    MADE.fetch_add(1, Ordering::Relaxed);
    crate::host::trail(|| {
        let label = LABEL.with(|l| l.borrow().clone());
        format!(
            "pipeline library: made {label} in {:.1} ms (the library did not load it: {refused})",
            began.elapsed().as_secs_f64() * 1e3
        )
    });
    let _all = one_call();
    let _stores = library.saving.read().unwrap_or_else(|e| e.into_inner());
    let _call = call("store", name);
    // SAFETY: a live pipeline; a name already stored (a description that
    // differs) is refused, which leaves the library as it was.
    if unsafe { library.library.StorePipeline(PCWSTR(wide.as_ptr()), &pso) }.is_ok() {
        precompile::hit();
        library.added.fetch_add(1, Ordering::Relaxed);
    } else if precompile::tracking() {
        // Two recipes can translate to the same host description. When
        // another worker stored it first, verify the entry before counting
        // this recipe as complete. A genuine description mismatch stays pending.
        let _one = same_name(name);
        if load(&library.library, PCWSTR(wide.as_ptr())).is_ok() {
            precompile::hit();
        }
    }
    Ok(pso)
}

/// Writes the library and the early list if pipelines were added since the
/// last save and `SAVE_EVERY` has passed (or `now`).
pub(crate) fn save(now: bool) {
    static LAST: Mutex<Option<std::time::Instant>> = Mutex::new(None);
    const SAVE_EVERY: std::time::Duration = std::time::Duration::from_secs(20);
    let Some(dir) = DIR.get() else { return };
    // Only one publisher may use the two temporary files. Periodic callers
    // are pipeline workers and can leave an in-progress save to its owner.
    let mut last = if now {
        LAST.lock().unwrap_or_else(|e| e.into_inner())
    } else {
        match LAST.try_lock() {
            Ok(last) => last,
            Err(std::sync::TryLockError::WouldBlock) => return,
            Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
        }
    };
    if !now && last.is_some_and(|t| t.elapsed() < SAVE_EVERY) {
        return;
    }
    *last = Some(std::time::Instant::now());
    if precompile::tracking() && !precompile::writer(dir) { return; }
    if warm_enabled()
        && let Err(error) = save_warm(dir)
    {
        tracing::warn!(%error, "pipeline warm-up recipes could not be saved");
    }
    static EARLY_WITHOUT_LIBRARY: OnceLock<bool> = OnceLock::new();
    let early_without_library = *EARLY_WITHOUT_LIBRARY.get_or_init(|| {
        std::env::var_os("VICTORY_PIPELINE_EARLY_WITHOUT_LIBRARY").is_some_and(|value| value == "1")
    });
    let lib = library();
    let snapshot = precompile::snapshot();
    save_files(dir, lib, &USED, early_without_library);
    if let Some(keys) = snapshot && lib.is_some_and(|l| l.added.load(Ordering::Relaxed) == 0)
        && let Err(error) = precompile::publish_keys(dir, &keys)
    {
        tracing::warn!(%error, "pipeline completion list could not be saved");
    }
}

fn save_files(dir: &Path, library: Option<&Library>, early: &EarlyKeys, early_without_library: bool) {
    if library.is_none() && !early_without_library {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(dir) {
        tracing::warn!(error = %e, "host pipeline library not saved");
        return;
    }
    match early.save(dir) {
        Ok(Some(count)) => tracing::info!(early = count, "title pipelines saved for the next run"),
        Err(e) => tracing::warn!(error = %e, "title pipelines not saved for the next run"),
        Ok(None) => {}
    }
    let Some(library) = library else { return };
    let tmp = dir.join(LIBRARY_FILE).with_extension("tmp");
    let (bytes, added) = {
        let _all = one_call();
        // No store between asking the size and writing; loads go on.
        let _alone = library.saving.write().unwrap_or_else(|e| e.into_inner());
        let added = library.added.load(Ordering::Relaxed);
        if added == 0 {
            return;
        }
        // SAFETY: a plain query.
        let size = unsafe { library.library.GetSerializedSize() };
        let _call = call("serialise", "the whole library");
        if let Err(e) = serialise(&library.library, &tmp, size) {
            tracing::warn!(error = %e, "host pipeline library not serialised");
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        (size, added)
    };
    if let Err(e) = replace_library(dir, &tmp) {
        tracing::warn!(error = %e, "host pipeline library not saved");
        return;
    }
    library.added.fetch_sub(added, Ordering::Relaxed);
    SAVED.store(bytes as u64, Ordering::Relaxed);
    let in_place = !dir.join(NEXT_FILE).exists();
    if in_place && VIEW[0].load(Ordering::Relaxed) != 0 {
        REPLACED[1].store(crate::host::now_ms(), Ordering::Relaxed);
        REPLACED[0].fetch_add(1, Ordering::Relaxed);
    }
    crate::host::trail(|| {
        format!(
            "pipeline library: saved {bytes} bytes, {}",
            if in_place { "renamed over the library's file" } else { "left beside the library's file for the next start" }
        )
    });
    tracing::info!(
        bytes,
        made = MADE.load(Ordering::Relaxed),
        loaded = LOADED.load(Ordering::Relaxed),
        started_early = EARLY.load(Ordering::Relaxed),
        "host pipeline library saved"
    );
}

/// Writes the library's `size` serialised bytes to the file `tmp`: through
/// a mapping of that file, so that the bytes are the file's pages and not
/// 1.3 GB more of the process's own memory for the seconds a save takes;
/// through a buffer where the file cannot be mapped (and with
/// `VICTORY_LIBRARY_MAP=0`).
fn serialise(library: &ID3D12PipelineLibrary1, tmp: &std::path::Path, size: usize) -> std::io::Result<()> {
    let failed = |e: windows::core::Error| std::io::Error::other(e.to_string());
    if mapped()
        && let Some(mut file) = Mapped::create(tmp, size)
    {
        // SAFETY: a buffer of the size the library asks for.
        return unsafe { library.Serialize(file.bytes_mut()) }.map_err(failed);
    }
    let mut bytes = vec![0u8; size];
    // SAFETY: a buffer of the size the library asks for.
    unsafe { library.Serialize(&mut bytes) }.map_err(failed)?;
    std::fs::write(tmp, &bytes)
}

/// Renames the written library `tmp` over the library's file in `dir`. This
/// run may have that file mapped: Windows 11 replaces a mapped file, and
/// the mapping goes on reading the old one for as long as a handle on it
/// is open ([`holds`]; with none, only the pages in memory at that moment
/// stay: the rest read as zeros). Where a system refuses, the new library
/// is left beside it as [`NEXT_FILE`] for the next start.
fn replace_library(dir: &std::path::Path, tmp: &std::path::Path) -> std::io::Result<()> {
    if precompile::tracking() && dir.join(NEXT_FILE).exists() {
        // A previous save could not replace the mapped file. The reader
        // prefers its successor, so publish subsequent additions there too.
        return std::fs::rename(tmp, dir.join(NEXT_FILE));
    }
    std::fs::rename(tmp, dir.join(LIBRARY_FILE)).or_else(|_| std::fs::rename(tmp, dir.join(NEXT_FILE)))
}

/// Writes `bytes` to a temporary file beside `path`, then renames it over
/// `path`: a reader never sees a part.
fn write_atomically(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// Title pipeline keys requested by draws or dispatches (this run and the last).
static USED: EarlyKeys = EarlyKeys::new();

struct EarlyKeys {
    keys: OnceLock<Mutex<HashSet<String>>>,
    changed: AtomicBool,
}

impl EarlyKeys {
    const fn new() -> Self {
        Self { keys: OnceLock::new(), changed: AtomicBool::new(false) }
    }

    fn keys(&self, dir: Option<&Path>) -> MutexGuard<'_, HashSet<String>> {
        self.keys.get_or_init(|| {
            let listed = dir.and_then(|d| std::fs::read_to_string(d.join(EARLY_FILE)).ok()).unwrap_or_default();
            let set: HashSet<String> = listed.lines().filter(|l| !l.is_empty()).map(str::to_owned).collect();
            tracing::info!(pipelines = set.len(), "title pipelines to start early");
            Mutex::new(set)
        }).lock().unwrap_or_else(|e| e.into_inner())
    }

    fn note(&self, dir: Option<&Path>, key: &str) {
        let mut keys = self.keys(dir);
        if keys.len() < EARLY_MAX && keys.insert(key.to_owned()) {
            self.changed.store(true, Ordering::Relaxed);
        }
    }

    fn save(&self, dir: &Path) -> std::io::Result<Option<usize>> {
        if !self.changed.swap(false, Ordering::Relaxed) {
            return Ok(None);
        }
        let mut keys: Vec<_> = self.keys(Some(dir)).iter().cloned().collect();
        keys.sort_unstable();
        if let Err(e) = write_atomically(&dir.join(EARLY_FILE), keys.join("\n").as_bytes()) {
            self.changed.store(true, Ordering::Relaxed);
            return Err(e);
        }
        Ok(Some(keys.len()))
    }
}

/// Whether the title pipeline with `key` was requested in this or an earlier run.
pub(crate) fn wanted_early(key: &str) -> bool {
    USED.keys(DIR.get().map(PathBuf::as_path)).contains(key)
}

/// A draw or dispatch requested the title pipeline with `key`.
pub(crate) fn note_used(key: &str) {
    USED.note(DIR.get().map(PathBuf::as_path), key)
}

pub(super) fn warm_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    DIR.get().is_some()
        && *ON.get_or_init(|| std::env::var_os("VICTORY_PIPELINE_WARMUP").is_none_or(|v| v != "0"))
}

/// Recording needs one relaxed increment per lookup. Starting the reader
/// also creates live host objects during play, so that work stays opt-in.
pub(super) fn replay_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    warm_enabled() && *ON.get_or_init(|| std::env::var_os("VICTORY_PIPELINE_WARMUP").is_some_and(|v| v == "1"))
}

/// Approach after XCL 354ce0c: record the pipelines requested by a bind.
/// The old early list held 2,752 keys, but a key alone cannot start a build
/// before the title creates the object. Keep the construction inputs and
/// rank them by host pipeline lookups during the last recorded run.
struct WarmEntry {
    pipeline: Arc<Pending>,
    calls: Arc<AtomicU64>,
    stored: AtomicBool,
}

fn catalog() -> &'static Mutex<HashMap<String, Arc<WarmEntry>>> {
    static CATALOG: OnceLock<Mutex<HashMap<String, Arc<WarmEntry>>>> = OnceLock::new();
    CATALOG.get_or_init(Mutex::default)
}

fn register(p: Arc<Pending>) -> Arc<WarmEntry> {
    let mut table = catalog().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(entry) = table.get(&p.key) {
        return entry.clone();
    }
    let entry = Arc::new(WarmEntry {
        pipeline: p,
        calls: Arc::new(AtomicU64::new(0)),
        stored: AtomicBool::new(false),
    });
    if table.len() < EARLY_MAX {
        table.insert(entry.pipeline.key.clone(), entry.clone());
    }
    entry
}

pub(super) fn shared(p: Arc<Pending>) -> Arc<Pending> {
    let entry = register(p.clone());
    if Arc::ptr_eq(&p, &entry.pipeline) || !p.same_inputs(&entry.pipeline) {
        p
    } else {
        entry.pipeline.clone()
    }
}

pub(super) fn replayed(p: &Pending) -> Option<Arc<Pending>> {
    let previous = catalog()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&p.key)?
        .pipeline
        .clone();
    p.same_inputs(&previous).then_some(previous)
}

pub(super) struct Usage {
    pub(super) calls: Arc<AtomicU64>,
    pub(super) shared: Option<Arc<Pending>>,
}

/// A title can create its object while the reader is still opening its
/// saved recipe. Whichever object registered first owns the build. Only
/// the other object retains it, so the owner never holds its own Arc.
pub(super) fn usage(p: &Arc<Pending>) -> Usage {
    let entry = register(p.clone());
    let shared = (!Arc::ptr_eq(p, &entry.pipeline) && p.same_inputs(&entry.pipeline))
        .then(|| entry.pipeline.clone());
    Usage {
        calls: entry.calls.clone(),
        shared,
    }
}

const RECIPE_DIR: &str = "warm-v1";
const ORDER_FILE: &str = "order.txt";
const STAGES: [&str; 8] = [
    "vertex",
    "pixel",
    "compute",
    "mesh",
    "amplification",
    "geometry",
    "hull",
    "domain",
];

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid pipeline recipe")
}

fn read_bounded(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > limit as u64 {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(invalid());
    }
    Ok(bytes)
}

fn ranking(dir: &Path) -> io::Result<Vec<(String, u64)>> {
    let bytes = match read_bounded(&dir.join(ORDER_FILE), 1 << 20) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        result => result?,
    };
    let mut rows = HashMap::new();
    for line in std::str::from_utf8(&bytes).map_err(|_| invalid())?.lines() {
        let Some((count, key)) = line.split_once(' ') else {
            continue;
        };
        if key.len() == 32
            && key.bytes().all(|b| b.is_ascii_hexdigit())
            && let Ok(count) = count.parse::<u64>()
        {
            rows.insert(key.to_owned(), count);
        }
    }
    Ok(rank(rows))
}

fn rank(rows: HashMap<String, u64>) -> Vec<(String, u64)> {
    let mut rows: Vec<_> = rows.into_iter().collect();
    rows.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    rows.truncate(EARLY_MAX);
    rows
}

/// Save complete inputs before publishing their key in the order file.
/// Each distinct shader container takes space once across all recipes.
fn write_recipe(dir: &Path, p: &Pending, shaders: &mut HashSet<blake3::Hash>) -> io::Result<u64> {
    let mut written = 0;
    let count = u8::try_from(p.shaders.len()).map_err(|_| invalid())?;
    if count == 0 || usize::from(count) > STAGES.len() {
        return Err(invalid());
    }
    let mut bytes = vec![count];
    for s in &p.shaders {
        let kind = STAGES
            .iter()
            .position(|&kind| kind == s.stage)
            .ok_or_else(invalid)?;
        let hash = blake3::hash(&s.container);
        if !shaders.contains(&hash) {
            let path = dir.join(hash.to_hex().as_str());
            if !matches_file(&path, &s.container) {
                publish(&path, &s.container)?;
                written += s.container.len() as u64;
            }
            shaders.insert(hash);
        }
        bytes.push(kind as u8);
        bytes.extend_from_slice(hash.as_bytes());
    }
    if let Some(g) = &p.graphics {
        bytes.extend(g.cache_bytes(false));
    }
    let checksum = blake3::hash(&bytes);
    bytes.extend_from_slice(checksum.as_bytes());
    let path = dir.join(&p.key);
    if !matches_file(&path, &bytes) {
        publish(&path, &bytes)?;
        written += bytes.len() as u64;
    }
    Ok(written)
}

/// Reopening a recorded title should not rewrite its shader containers.
/// A bounded comparison also repairs an incomplete file on the next save.
fn matches_file(path: &Path, bytes: &[u8]) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.len() == bytes.len() as u64)
        && read_bounded(path, bytes.len()).is_ok_and(|v| v == bytes)
}

fn publish(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(tmp, path)
}

fn read_recipe(
    dir: &Path,
    key: &str,
    verified: &mut HashMap<blake3::Hash, Arc<super::Shader>>,
) -> io::Result<Pending> {
    let file = read_bounded(&dir.join(key), 1 << 20)?;
    let (mut bytes, checksum) = file.split_last_chunk::<32>().ok_or_else(invalid)?;
    if blake3::hash(bytes).as_bytes() != checksum {
        return Err(invalid());
    }
    let count = usize::from(*bytes.split_off_first().ok_or_else(invalid)?);
    if count == 0 || count > STAGES.len() {
        return Err(invalid());
    }
    let mut shaders = Vec::with_capacity(count);
    for _ in 0..count {
        let kind = *bytes.split_off_first().ok_or_else(invalid)? as usize;
        let stage = *STAGES.get(kind).ok_or_else(invalid)?;
        let digest = *bytes
            .split_off(..32)
            .and_then(<[u8]>::as_array::<32>)
            .ok_or_else(invalid)?;
        let hash = blake3::Hash::from_bytes(digest);
        let shader = match verified.get(&hash) {
            Some(shader) => shader.clone(),
            None => {
                let source = read_bounded(&dir.join(hash.to_hex().as_str()), 64 << 20)?;
                if blake3::hash(&source) != hash {
                    return Err(invalid());
                }
                let shader = super::shader(stage, &source);
                verified.insert(hash, shader.clone());
                shader
            }
        };
        if shader.stage != stage {
            return Err(invalid());
        }
        shaders.push(shader);
    }
    let graphics = if bytes.is_empty() {
        None
    } else {
        Some(Box::new(
            super::host::GraphicsState::from_cache(bytes).ok_or_else(invalid)?,
        ))
    };
    let p = Pending::new(shaders, graphics);
    if p.key != key {
        return Err(invalid());
    }
    Ok(p)
}

static RESTORED: AtomicU64 = AtomicU64::new(0);
static UNREADABLE: AtomicU64 = AtomicU64::new(0);

/// Start at the first title pipeline creation, after the shader cache and
/// device configuration are in place. File reads and compilation run off
/// the recording threads. A live request can promote a replay already queued.
pub(super) fn start_warmup() {
    static START: std::sync::Once = std::sync::Once::new();
    let Some(dir) = DIR.get() else { return };
    START.call_once(|| {
        let dir = dir.join(RECIPE_DIR);
        let worker = std::thread::Builder::new()
            .name("victory-pipeline-warmup".into())
            .spawn(move || {
                let rows = match ranking(&dir) {
                    Ok(rows) => rows,
                    Err(error) => {
                        tracing::warn!(%error, "pipeline warm-up order unreadable");
                        return;
                    }
                };
                let mut verified = HashMap::new();
                for (key, count) in rows {
                    if count == 0 {
                        break;
                    }
                    match read_recipe(&dir, &key, &mut verified) {
                        Ok(p) => {
                            let entry = register(Arc::new(p));
                            entry.stored.store(true, Ordering::Relaxed);
                            super::make::replay(&entry.pipeline);
                            RESTORED.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(_) => {
                            UNREADABLE.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                tracing::info!(
                    restored = RESTORED.load(Ordering::Relaxed),
                    unreadable = UNREADABLE.load(Ordering::Relaxed),
                    "pipeline warm-up recipes queued"
                );
            });
        if let Err(error) = worker {
            tracing::warn!(%error, "pipeline warm-up reader unavailable");
        }
    });
}

fn save_warm(cache: &Path) -> io::Result<()> {
    let entries: Vec<_> = catalog()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .filter(|e| e.calls.load(Ordering::Relaxed) != 0)
        .cloned()
        .collect();
    if entries.is_empty() {
        return Ok(());
    }
    save_entries(&cache.join(RECIPE_DIR), entries)
}

fn save_entries(dir: &Path, entries: Vec<Arc<WarmEntry>>) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let started = std::time::Instant::now();
    let mut payload_bytes = 0;
    let prior = match ranking(dir) {
        Err(e) if e.kind() == io::ErrorKind::InvalidData => Vec::new(),
        result => result?,
    };
    // An entry unseen this run remains available behind every observed one.
    let mut rows: HashMap<_, _> = prior.into_iter().map(|(key, _)| (key, 0)).collect();
    let mut shaders = HashSet::new();
    for entry in entries {
        let p = &entry.pipeline;
        if !entry.stored.load(Ordering::Relaxed) {
            if p.shaders.is_empty() {
                continue;
            }
            match write_recipe(dir, p, &mut shaders) {
                Err(e) if e.kind() == io::ErrorKind::InvalidData => continue,
                result => payload_bytes += result?,
            }
            entry.stored.store(true, Ordering::Relaxed);
        }
        rows.insert(p.key.clone(), entry.calls.load(Ordering::Relaxed));
    }
    use std::fmt::Write;
    let mut out = String::new();
    for (key, count) in rank(rows) {
        writeln!(out, "{count} {key}").unwrap();
    }
    publish(&dir.join(ORDER_FILE), out.as_bytes())?;
    tracing::info!(payload_bytes, order_bytes = out.len(), save_us = started.elapsed().as_micros() as u64,
        "pipeline recipes saved");
    Ok(())
}

fn report_warmup() {
    use super::make::{REPLAY_LATE, REPLAY_READY, STANDIN_HITS, STANDIN_MISSES};
    let counts = [
        &RESTORED,
        &UNREADABLE,
        &REPLAY_READY,
        &REPLAY_LATE,
        &STANDIN_HITS,
        &STANDIN_MISSES,
    ]
    .map(|n| n.load(Ordering::Relaxed));
    if counts == [0; 6] {
        return;
    }
    static LAST: Mutex<[u64; 6]> = Mutex::new([0; 6]);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if *last != counts {
        *last = counts;
        let [
            restored,
            unreadable,
            ready_at_first_use,
            late_at_first_use,
            standin_hits,
            standin_misses,
        ] = counts;
        tracing::info!(
            restored,
            unreadable,
            ready_at_first_use,
            late_at_first_use,
            standin_hits,
            standin_misses,
            "pipeline warm-up and stand-in lookups"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_order_uses_frequency_and_ignores_invalid_cache_names() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c) = ("a".repeat(32), "b".repeat(32), "c".repeat(32));
        std::fs::write(
            dir.path().join(ORDER_FILE),
            format!(
                "2 {b}\n80 {c}\n2 {a}\n50 ../wrong\n1 {}\nbad {a}",
                "x".repeat(32)
            ),
        )
        .unwrap();
        assert_eq!(ranking(dir.path()).unwrap(), vec![(c, 80), (a, 2), (b, 2)]);
    }

    #[test]
    fn replay_and_title_creation_share_one_build_in_either_order() {
        for (replay_first, source) in [
            (true, b"synthetic replay first".as_slice()),
            (false, b"synthetic title first"),
        ] {
            let s = super::super::shader("compute", source);
            let make = || Arc::new(Pending::new(vec![s.clone()], None));
            let (title, replay) = (make(), make());
            let (binding, canonical) = if replay_first {
                let canonical = shared(replay.clone());
                let binding = usage(&title);
                assert!(Arc::ptr_eq(binding.shared.as_ref().unwrap(), &replay));
                (binding, canonical)
            } else {
                let binding = usage(&title);
                let canonical = shared(replay.clone());
                assert!(binding.shared.is_none());
                assert!(Arc::ptr_eq(&canonical, &title));
                (binding, canonical)
            };
            binding.calls.fetch_add(2, Ordering::Relaxed);
            assert_eq!(usage(&canonical).calls.load(Ordering::Relaxed), 2);
        }
    }

    #[test]
    fn warm_save_replays_the_last_runs_frequent_inputs_first() {
        let folder = tempfile::tempdir().unwrap();
        let dir = folder.path();
        let entry = |source: &[u8], calls| {
            Arc::new(WarmEntry {
                pipeline: Arc::new(Pending::new(
                    vec![super::super::shader("compute", source)],
                    None,
                )),
                calls: Arc::new(AtomicU64::new(calls)),
                stored: AtomicBool::new(false),
            })
        };
        let hot = entry(b"synthetic frequently used compute", 240);
        let cold = entry(b"synthetic rarely used compute", 3);
        save_entries(dir, vec![cold.clone(), hot.clone()]).unwrap();
        let rows = ranking(dir).unwrap();
        assert_eq!(
            rows,
            [
                (hot.pipeline.key.clone(), 240),
                (cold.pipeline.key.clone(), 3)
            ]
        );
        let mut verified = HashMap::new();
        for (key, _) in rows {
            assert_eq!(read_recipe(dir, &key, &mut verified).unwrap().key, key);
        }
        cold.calls.store(5, Ordering::Relaxed);
        save_entries(dir, vec![cold.clone()]).unwrap();
        assert_eq!(
            ranking(dir).unwrap(),
            [
                (cold.pipeline.key.clone(), 5),
                (hot.pipeline.key.clone(), 0)
            ]
        );
    }

    #[test]
    fn recipes_restore_shader_inputs_and_reject_incomplete_saves() {
        let dir = tempfile::tempdir().unwrap();
        // Construct outside the intern table so the reader must open and
        // verify the shader file, just as a fresh process does.
        let source = b"synthetic pipeline recipe vertex";
        let hash = blake3::hash(source);
        let shader = Arc::new(super::super::Shader {
            stage: "vertex",
            container: source.to_vec(),
            hash: hash.to_hex()[..16].to_owned(),
            translated: OnceLock::new(),
        });
        let p = Pending::new(vec![shader], Some(Box::default()));
        write_recipe(dir.path(), &p, &mut HashSet::new()).unwrap();
        let file = dir.path().join(hash.to_hex().as_str());
        std::fs::write(&file, b"damaged").unwrap();
        let mut verified = HashMap::new();
        assert!(read_recipe(dir.path(), &p.key, &mut verified).is_err());
        std::fs::write(file, source).unwrap();
        let restored = read_recipe(dir.path(), &p.key, &mut verified).unwrap();
        assert_eq!(restored.shaders[0].container, source);
        assert_eq!(p.key, restored.key);
        let file = dir.path().join(&p.key);
        let bytes = std::fs::read(&file).unwrap();
        std::fs::write(file, &bytes[..bytes.len() - 1]).unwrap();
        assert!(read_recipe(dir.path(), &p.key, &mut verified).is_err());
    }

    #[test]
    fn recording_shared_stages_writes_the_container_once() {
        let folder = tempfile::tempdir().unwrap();
        let source = vec![42; 64 * 1024];
        let shader = super::super::shader("vertex", &source);
        let make_entries = || -> Vec<_> {
            (1u32..=128).map(|index| {
                let pixel = super::super::shader("pixel", &index.to_le_bytes());
                Arc::new(WarmEntry {
                    pipeline: Arc::new(Pending::new(vec![shader.clone(), pixel], Some(Box::default()))),
                    calls: Arc::new(AtomicU64::new(1)),
                    stored: AtomicBool::new(false),
                })
            }).collect()
        };
        let entries = make_entries();
        let started = std::time::Instant::now();
        save_entries(folder.path(), entries).unwrap();
        let micros = started.elapsed().as_micros();
        let sizes: Vec<_> = folder.path().read_dir().unwrap()
            .map(|e| e.unwrap().metadata().unwrap().len()).collect();
        assert_eq!(sizes.len(), 258); // 128 descriptions, 129 containers, one ranking.
        let mut shaders = HashSet::new();
        for entry in make_entries() {
            assert_eq!(write_recipe(folder.path(), &entry.pipeline, &mut shaders).unwrap(), 0);
        }
        eprintln!("128 synthetic graphics recipes with one 64 KiB stage: {} bytes, {micros} us to save", sizes.iter().sum::<u64>());
    }

    /// A fresh process has its own driver, worker pool and cache directory.
    /// Its first, unrelated creation must suffice to build the saved pipeline.
    #[test]
    fn startup_builds_the_saved_pipeline_before_its_title_object_exists() {
        const CHILD: &str = "VICTORY_TEST_PIPELINE_RESTART_DIR";
        const SOURCE: &[u8] = b"synthetic restart compute stage";
        let shader = super::super::shader("compute", SOURCE);
        if let Some(dir) = std::env::var_os(CHILD) {
            use super::super::make;
            use std::time::{Duration, Instant};
            use windows::core::s;

            let has_device = crate::host::host().is_some();
            if has_device {
                let dxil = crate::host::compile(
                    "[numthreads(1,1,1)] void main() {}",
                    s!("main"),
                    s!("cs_5_1"),
                )
                .unwrap();
                let host = victory_shader::emit::HostShader {
                    stage: victory_shader::HwStage::Cs,
                    dxil,
                    user_data: vec![],
                    root_cbvs: vec![],
                    root_srvs: vec![],
                    root_uavs: vec![],
                    root_indexed_views: vec![],
                    samplers: vec![],
                    static_samplers: vec![],
                };
                assert!(
                    shader
                        .translated
                        .set(Some(super::super::Translated {
                            host,
                            used: vec![],
                            stores: false,
                        }))
                        .is_ok()
                );
            }
            use_pipeline_cache(dir.into());
            make::early(Arc::new(Pending::new(Vec::new(), None)));
            let title = || Arc::new(Pending::new(vec![shader.clone()], None));
            let enabled = std::env::var_os("VICTORY_PIPELINE_WARMUP").is_some_and(|v| v == "1");
            assert_eq!(replay_enabled(), enabled);
            let record = std::env::var_os("VICTORY_PIPELINE_WARMUP").is_none_or(|v| v != "0");
            assert_eq!(warm_enabled(), record);
            if !enabled {
                let title = title();
                assert!(replayed(&title).is_none());
                let _ = make::ready(&title);
                if record {
                    let calls = usage(&title).calls.load(Ordering::Relaxed);
                    assert_eq!(calls, 1);
                    save_warm(DIR.get().unwrap()).unwrap();
                    assert_eq!(ranking(&DIR.get().unwrap().join(RECIPE_DIR)).unwrap()[0].1, 1);
                }
                return;
            }
            let deadline = Instant::now() + Duration::from_secs(20);
            while RESTORED.load(Ordering::Relaxed) == 0 || make::waiting() != 0 {
                assert!(
                    Instant::now() < deadline,
                    "saved pipeline did not finish during startup"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            let title = title();
            let restored = replayed(&title).expect("built before the title asked");
            let live = make::early(title);
            assert!(Arc::ptr_eq(&restored, &live));
            if has_device {
                assert!(make::ready(&live).is_some());
            }
            return;
        }

        let folder = tempfile::tempdir().unwrap();
        let entry = Arc::new(WarmEntry {
            pipeline: Arc::new(Pending::new(vec![shader], None)),
            calls: Arc::new(AtomicU64::new(10)),
            stored: AtomicBool::new(false),
        });
        save_entries(&folder.path().join(RECIPE_DIR), vec![entry]).unwrap();
        let test = "pipeline::library::tests::startup_builds_the_saved_pipeline_before_its_title_object_exists";
        for switch in [Some("0"), Some("1"), None] {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command.args(["--exact", test, "--nocapture"]).env(CHILD, folder.path());
            match switch {
                Some(value) => command.env("VICTORY_PIPELINE_WARMUP", value),
                None => command.env_remove("VICTORY_PIPELINE_WARMUP"),
            };
            let child = command.output().unwrap();
            assert!(
                child.status.success(),
                "restart with warm-up {switch:?}: {} {}",
                String::from_utf8_lossy(&child.stdout),
                String::from_utf8_lossy(&child.stderr)
            );
            assert!(String::from_utf8_lossy(&child.stdout).contains("1 passed"));
        }
    }

    #[test]
    fn early_keys_survive_a_restart_without_a_driver_library() {
        let folder = tempfile::tempdir().unwrap();
        let dir = folder.path();
        let path = dir.join(EARLY_FILE);
        let keys = EarlyKeys::new();
        keys.note(Some(dir), "c");
        keys.note(Some(dir), "a");
        save_files(dir, None, &keys, false);
        assert!(!path.exists());

        save_files(dir, None, &keys, true);
        assert_eq!(std::fs::read(&path).unwrap(), b"a\nc");
        assert!(!dir.join(LIBRARY_FILE).exists());

        let next = EarlyKeys::new();
        assert!(next.keys(Some(dir)).contains("c"));
        next.note(Some(dir), "b");
        // A failed publication leaves the previous list intact and the new
        // key eligible for retry without another draw.
        let temporary = path.with_extension("tmp");
        std::fs::create_dir(&temporary).unwrap();
        save_files(dir, None, &next, true);
        assert_eq!(std::fs::read(&path).unwrap(), b"a\nc");
        std::fs::remove_dir(temporary).unwrap();
        save_files(dir, None, &next, true);
        assert_eq!(std::fs::read(path).unwrap(), b"a\nb\nc");
    }

    #[test]
    fn names_change_with_what_goes_into_the_pipeline() {
        let a = name("graphics", b"root", &[b"vs", b"ps"], "state");
        assert_eq!(a, name("graphics", b"root", &[b"vs", b"ps"], "state"));
        assert_ne!(a, name("graphics", b"root", &[b"vs", b"ps2"], "state"));
        assert_ne!(a, name("graphics", b"root", &[b"vs", b"ps"], "state2"));
        assert_ne!(a, name("graphics", b"root2", &[b"vs", b"ps"], "state"));
        assert_ne!(a, name("mesh", b"root", &[b"vs", b"ps"], "state"));
        // Parts do not run into each other.
        assert_ne!(a, name("graphics", b"root", &[b"vsp", b"s"], "state"));
    }

    #[test]
    fn a_rejected_blob_gives_an_empty_library() {
        let Some(device) = crate::host::host().and_then(|h| h.device.cast::<ID3D12Device1>().ok()) else {
            return; // no host GPU here
        };
        let library = open(&device, Blob::Held(b"not a pipeline library".repeat(16))).expect("an empty library");
        // SAFETY: a plain query on a live library.
        assert!(unsafe { library.library.GetSerializedSize() } > 0);
        assert!(matches!(library._blob, Blob::None));
    }

    /// A mapped file reads as the file's bytes, a file written through a
    /// mapping holds what was written, and the library's file can be
    /// replaced while it is mapped (or the new one waits as the next).
    #[test]
    fn the_library_file_is_mapped_and_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LIBRARY_FILE);
        assert!(matches!(Blob::of(dir.path()), Blob::None), "no file yet");
        std::fs::write(&path, b"").unwrap();
        assert!(matches!(Blob::of(dir.path()), Blob::None), "an empty file");
        let old: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect();
        std::fs::write(&path, &old).unwrap();
        let blob = Blob::of(dir.path());
        assert!(matches!(blob, Blob::Mapped(_)) || !mapped());
        assert_eq!(blob.bytes(), &old[..]);
        // A save while the old file is mapped.
        let tmp = path.with_extension("tmp");
        let new: Vec<u8> = (0..300_000u32).map(|i| (i * 7) as u8).collect();
        {
            let mut file = Mapped::create(&tmp, new.len()).expect("a new file, mapped");
            file.bytes_mut().copy_from_slice(&new);
        }
        assert_eq!(std::fs::read(&tmp).unwrap(), new);
        replace_library(dir.path(), &tmp).unwrap();
        assert!(!tmp.exists());
        // The mapping still reads the old library.
        assert_eq!(blob.bytes(), &old[..]);
        drop(blob);
        // The next start reads the new one, wherever the save left it.
        assert_eq!(Blob::of(dir.path()).bytes(), &new[..]);
        assert!(!dir.path().join(NEXT_FILE).exists());
    }

    /// The bytes at `at` of the pattern the test below writes.
    fn pattern(at: usize, out: &mut [u8]) {
        for (i, word) in out.chunks_exact_mut(4).enumerate() {
            let v = ((at / 4 + i) as u32).wrapping_mul(2_654_435_761).wrapping_add(1);
            word.copy_from_slice(&v.to_le_bytes());
        }
    }

    /// A file of `len` bytes (whole MiB) of [`pattern`], written past the
    /// system's cache: none of its pages is in memory afterwards. False
    /// where the folder's file system does not take such writes.
    fn write_uncached(path: &std::path::Path, len: usize) -> bool {
        use std::io::Write;
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_NO_BUFFERING | FILE_FLAG_WRITE_THROUGH
        const UNCACHED: u32 = 0x2000_0000 | 0x8000_0000;
        const CHUNK: usize = 1 << 20;
        let Ok(mut file) = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .custom_flags(UNCACHED)
            .open(path)
        else {
            return false;
        };
        // Such writes want a buffer on a sector boundary.
        let mut store = vec![0u8; CHUNK + 4096];
        let start = store.as_ptr().align_offset(4096);
        for at in (0..len).step_by(CHUNK) {
            let chunk = &mut store[start..start + CHUNK];
            pattern(at, chunk);
            if file.write_all(chunk).is_err() {
                return false;
            }
        }
        true
    }

    /// The device loss of runs 72, 73 and 82: the library's file is
    /// replaced while it is mapped, and pages of the mapping that were not
    /// in memory then are read afterwards (in the runs: pages the trim let
    /// go and Windows gave to something else). With the file's handle kept
    /// open they still read as the old file. (With it closed, as before,
    /// all but the first few read as zeros on the PC this was found on.)
    #[test]
    fn a_replaced_library_file_still_reads_from_the_disk() {
        const LEN: usize = 8 << 20;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LIBRARY_FILE);
        if !write_uncached(&path, LEN) {
            return; // no uncached writes here: nothing to show
        }
        let mapped = Mapped::open_holding(&path, true).expect("the file, mapped");
        // A save: the new library, renamed over the mapped one.
        let tmp = path.with_extension("tmp");
        {
            let mut file = Mapped::create(&tmp, LEN).expect("a new file, mapped");
            file.bytes_mut().fill(0xEE);
        }
        replace_library(dir.path(), &tmp).unwrap();
        // Every page of the old mapping, read for the first time now.
        let mut expected = vec![0u8; 4096];
        for (page, bytes) in mapped.bytes().chunks(4096).enumerate() {
            pattern(page * 4096, &mut expected);
            assert!(bytes == &expected[..], "page {page} of the replaced library no longer reads as the old file");
        }
    }
}
