//! Command queues. Lists, waits, signals and queued presents keep their
//! place while our workers prepare submissions and land their GPU writes.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(windows)]
use std::sync::{Arc, Mutex};
#[cfg(windows)]
use crate::frame_wait::{self, Call, Stage, lock};

use crate::object::{self, E_INVALIDARG, HRESULT, Iid, Object, QUEUE, S_OK, Table, This, guid};
use crate::resource::PAGEABLE_IIDS;

const QUEUE_IIDS: [Iid; 4] = [
    guid(
        0x0ec870a6,
        0x5d7e,
        0x4c22,
        [0x8c, 0xfc, 0x5b, 0xaa, 0xe0, 0x76, 0x16, 0xed],
    ),
    PAGEABLE_IIDS[0],
    PAGEABLE_IIDS[1],
    PAGEABLE_IIDS[2],
];

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct QueueDesc {
    kind: u32,
    priority: i32,
    flags: u32,
    nodes: u32,
}

/// The title reads the list type right after the header.
#[repr(C)]
struct Queue {
    object: Object,
    kind: u32,
    desc: QueueDesc,
    /// What the queue was given after a wait for a fence that had not been
    /// reached, in order; it runs when the fence gets there.
    #[cfg(windows)]
    held: Arc<QueueLine>,
}
const _: () = assert!(std::mem::offset_of!(Queue, kind) == 0x10);

/// One thing a waiting queue holds back.
#[cfg(windows)]
enum Held {
    /// The queue waits for the fence to reach the value.
    Wait(crate::fence::FenceRef, u64),
    /// A list of the next submission waits for a memory value.
    Value(crate::list::ValueWait),
    /// A submission: the lists' host sides, taken from the title's lists
    /// (which it may reset meanwhile), and the order they run in.
    Run(Vec<crate::list::Detached>, Vec<usize>),
    /// A signal of the queue's, due after what it holds before it.
    Signal(crate::fence::FenceRef, u64),
    /// With [`behind`]: the submission at the head of the line is being
    /// prepared and sent by our thread, with the line unlocked, so the
    /// title can add to its end meanwhile; whatever is added stays behind
    /// this (the line is not empty), signals too.
    Running,
    /// Owns the presentation source and its place among the frames in flight.
    Present(Box<dyn FnOnce() + Send>),
}

#[cfg(windows)]
#[derive(Default)]
struct QueueLine {
    commands: Mutex<Vec<Held>>,
    retired: AtomicBool,
}

/// L'adresse de la queue sert au diagnostic. Le fil de soumission conserve
/// sa propre référence aux commandes pendant toute la préparation.
#[cfg(windows)]
static WAITING: Mutex<Vec<(usize, Arc<QueueLine>)>> = Mutex::new(Vec::new());

#[cfg(windows)]
fn register(this: This) {
    let line = &queue(this).held;
    let mut waiting = lock(&WAITING);
    if !waiting.iter().any(|(_, other)| Arc::ptr_eq(other, line)) {
        waiting.push((this as usize, line.clone()));
    }
}

#[cfg(windows)]
fn unregister(line: &Arc<QueueLine>) {
    lock(&WAITING).retain(|(_, other)| !Arc::ptr_eq(other, line));
}

#[cfg(windows)]
impl Drop for Queue {
    fn drop(&mut self) {
        self.held.retired.store(true, Ordering::Release);
        unregister(&self.held);
    }
}

/// Whether queues wait for fences. `VICTORY_QUEUE_WAIT=0`: they do not,
/// and every submission runs when it is made (as before).
#[cfg(windows)]
fn waits() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_QUEUE_WAIT").is_none_or(|v| v != "0"))
}

/// Whether a submission is held list by list: the lists before the first
/// one whose wait for a memory value is not met run at once, and each list
/// that waits is held with the lists after it, behind its own waits.
/// Holding a submission whole made the title's queues wait for each other:
/// a graphics submission was held for a value a compute list writes, and
/// that compute list for a value an earlier list of the same graphics
/// submission writes. Nothing moved until the present forced it, and the
/// title reached its present only after waiting about 2.5 s, every frame
/// (runs 27-30: 2.4-2.9 s from the last submission to the present call,
/// 13-32 ms before waits were honoured; the frame's main graphics
/// submissions, 88 lists, ran forced, the compute lists before them).
/// On by default; `VICTORY_SPLIT_WAITS=0` holds whole submissions, as
/// before.
#[cfg(windows)]
fn split() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_SPLIT_WAITS").is_none_or(|v| v != "0"))
}

/// Whether a list is cut into host lists at its waits for memory values
/// (`crate::list::cut`), and a queue runs a submission part by part: it
/// stands at each wait until the value is in memory, with everything
/// recorded before the wait run and its bottom-of-pipe writes done, as on
/// the console. Held list by list ([`split`]) a wait still held its whole
/// list: the title's lists are chains of jobs, each behind a wait for a
/// value another queue's list writes in the middle of its own chain
/// (frame 858 of run 33: a compute list of 433 commands with 13 waits;
/// graphics lists of the form "341 dispatches, wait, draws"), so two lists
/// each waited for a write behind the other's wait and only the present
/// moved them, in the wrong order.
/// On by default where waits are honoured list by list;
/// `VICTORY_CUT_WAITS=0` holds whole lists, as before.
#[cfg(windows)]
pub(crate) fn cuts() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    waits() && split() && *ON.get_or_init(|| std::env::var_os("VICTORY_CUT_WAITS").is_none_or(|v| v != "0"))
}

/// Whether a present runs what queues still hold (on by default: a wait
/// is not kept across frames). `VICTORY_WAIT_FORCE=0` keeps it held: for
/// finding out whether the title's waits reach across its presents; a
/// wait nothing ever meets then stops the title for good.
#[cfg(windows)]
fn forces() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_WAIT_FORCE").is_none_or(|v| v != "0"))
}

/// A submission to a queue whose lists are cut at their waits ([`cuts`]).
/// The lists before the first one that waits run at once if the queue
/// holds nothing; the rest goes behind what the queue holds, part by part
/// (`crate::list::detach_parts`): the waits of a part, then its host
/// lists. [`release_held`] runs the queue's line as far as the values are
/// there, now and whenever a submission has run.
#[cfg(windows)]
fn submit_cut(this: This, lists: &[This]) {
    let Some(host) = crate::host::host() else {
        return;
    };
    let first = if !behind() && lock(&queue(this).held.commands).is_empty() {
        lists
            .iter()
            .position(|&l| crate::list::cut_list(l))
            .unwrap_or(lists.len())
    } else {
        0
    };
    #[cfg(feature = "diagnostics")]
    crate::capture::op(this as usize, || {
        let named: Vec<String> = lists
            .iter()
            .map(|&l| match crate::list::host_list(l) {
                Some(h) => format!("{:#x}", std::ptr::from_ref(h) as usize),
                None => format!("title {l:?}"),
            })
            .collect();
        format!(
            "SUBMIT queue kind {:?} held {} lists {named:?} (the first {first} run at once, the rest part by part)",
            Some(queue(this).kind),
            first < lists.len()
        )
    });
    if first > 0 && crate::host::late() {
        // Sent to the card and not waited for: their host sides are taken
        // from the title's lists (which it may reset before they have run)
        // and come back, with their tables' slots, when they are settled.
        let (taken, order) = crate::list::detach(&lists[..first]);
        host.execute_late(taken, order);
    } else if first > 0 {
        let now: Vec<_> = lists[..first]
            .iter()
            .filter_map(|&l| crate::list::host_list(l))
            .collect();
        host.execute(&now);
        // They have run: their descriptor tables' slots are free again, as
        // on the path without cuts ([`execute`]). This path replaced that
        // one without the call, so the lists that run at once kept their
        // slots until the title's reset and the ring ran out as before
        // (run 45: 3,143 of 15,942 tables refused in one frame).
        crate::list::tables_done(&lists[..first]);
    }
    if first < lists.len() {
        let parts = crate::list::detach_parts(&lists[first..]);
        static SEEN: AtomicU64 = AtomicU64::new(0);
        if SEEN.fetch_add(1, Ordering::Relaxed) < 8 {
            tracing::info!(
                kind = queue(this).kind,
                lists = lists.len(),
                run_at_once = first,
                parts = parts.len(),
                waits = parts.iter().map(|p| p.0.len()).sum::<usize>(),
                "a submission is given to its queue part by part (its lists wait for memory values)"
            );
        }
        {
            let mut held = lock(&queue(this).held.commands);
            for (waits, taken, order) in parts {
                if crate::watch::ahead::enabled() && waits.is_empty()
                    && matches!(held.as_slice(), [Held::Running])
                    && let Some(&first) = order.first()
                {
                    taken[first].host().prepare_guarded_pages();
                }
                held.extend(waits.into_iter().map(Held::Value));
                held.push(Held::Run(taken, order));
            }
        }
        register(this);
    }
    // What ran may be what another queue waits for, and what was just
    // given to the queue may be free to run already.
    release_held(false);
}

/// A submission to a queue that honours waits, list by list ([`split`]).
/// shortcut: a wait still holds its whole list, not only the commands
/// after it; and lists named twice among those held stay in one piece.
#[cfg(windows)]
fn submit_split(this: This, lists: &[This]) {
    let Some(host) = crate::host::host() else {
        return;
    };
    let by = crate::list::waits_by_list(lists);
    // Behind what the queue already holds, nothing runs now.
    let cut = if lock(&queue(this).held.commands).is_empty() {
        (0..lists.len())
            .find(|&i| by[i].iter().any(|w| !w.met()))
            .unwrap_or(lists.len())
    } else {
        0
    };
    #[cfg(feature = "diagnostics")]
    crate::capture::op(this as usize, || {
        let named: Vec<String> = lists
            .iter()
            .map(|&l| match crate::list::host_list(l) {
                Some(h) => format!("{:#x}", std::ptr::from_ref(h) as usize),
                None => format!("title {l:?}"),
            })
            .collect();
        format!(
            "SUBMIT queue kind {:?} held {} lists {named:?} (the first {cut} run at once)",
            Some(queue(this).kind),
            cut < lists.len()
        )
    });
    if cut > 0 {
        let now: Vec<_> = lists[..cut]
            .iter()
            .filter_map(|&l| crate::list::host_list(l))
            .collect();
        host.execute(&now);
    }
    if cut < lists.len() {
        static SEEN: AtomicU64 = AtomicU64::new(0);
        if SEEN.fetch_add(1, Ordering::Relaxed) < 8 {
            tracing::info!(
                kind = queue(this).kind,
                lists = lists.len(),
                run_at_once = cut,
                waits = ?by[cut],
                "lists of a submission are held for a memory value"
            );
        }
        let rest = &lists[cut..];
        // A list's host side can be taken once.
        let twice = rest.iter().enumerate().any(|(i, l)| rest[..i].contains(l));
        let mut pieces: Vec<Held> = Vec::new();
        let mut from = cut;
        while from < lists.len() {
            // Up to the next list that waits for anything (met or not: it
            // is looked at again when its turn comes).
            let to = if twice {
                lists.len()
            } else {
                (from + 1..lists.len())
                    .find(|&i| !by[i].is_empty())
                    .unwrap_or(lists.len())
            };
            let waits: Vec<crate::list::ValueWait> = by[from..to].concat();
            pieces.extend(waits.into_iter().map(Held::Value));
            let (taken, order) = crate::list::detach(&lists[from..to]);
            pieces.push(Held::Run(taken, order));
            from = to;
        }
        lock(&queue(this).held.commands).extend(pieces);
        register(this);
    }
    // What ran may be what another queue waits for, and what was just held
    // may be free to run already.
    release_held(false);
}

/// Adds `what()` to what the queue holds, if it holds anything (it waits
/// for a fence): whether it was added.
#[cfg(windows)]
fn hold(this: This, what: impl FnOnce() -> Held) -> bool {
    if this.is_null() {
        return false;
    }
    let mut held = lock(&queue(this).held.commands);
    if held.is_empty() {
        return false;
    }
    held.push(what());
    true
}

/// Runs what waiting queues hold as far as their fences have been reached
/// (`all`: all of it), each queue's in the order it was given. Called
/// where a fence may have moved: after a signal and after a submission (a
/// list's bottom-of-pipe write may be into a fence), never from inside
/// one.
#[cfg(windows)]
pub(crate) fn release_held(all: bool) {
    if !all && behind() {
        // A thread of ours runs what is free to run ([`behind`]).
        wake();
        return;
    }
    release_held_now(all);
}

/// Whether a submission is only put in its queue's line by the thread
/// that submits it, and prepared and sent by a thread of ours (on by
/// default; `VICTORY_ASYNC_SUBMIT=0`: on the title's thread, as before;
/// needs flights and lists cut at their waits, both defaults). In the
/// prologue on the RTX 5080 the title's render thread stood in the
/// submission for most of every frame, as in run 213 below.
///
/// Why: the profile of run 213 (292 ms a frame) has the title's render
/// thread inside `queue::execute` for 202 ms of every frame: 155 ms in
/// `Host::submit` (preparing: the copies of title memory, the write
/// watch, descriptors, textures; and waiting for flights), while its five
/// recording threads stand idle 78 % of the time and the card 43 %. The
/// title's thread does nothing with the result of that work: it only has
/// to be done before the lists run, in order.
///
/// With the switch a submission's lists are taken from the title's lists
/// (as they already are when the queue holds anything) and put behind
/// the queue's line; one thread of ours ([`worker`]) runs the line in
/// order. A signal of the queue goes behind them, as it does behind
/// anything held. Everything that must see earlier submissions done runs
/// the line itself first: a present, and every submission of ours that
/// is waited for ([`before_sync`]).
///
/// What it assumes: title memory a submission reads is not rewritten by
/// the title between its submit call and the moment our thread prepares
/// it, some milliseconds later. On the console the card reads that
/// memory when it runs the list, later still, so a title that rewrote it
/// earlier would be wrong there too.
#[cfg(windows)]
pub(crate) fn behind() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var_os("VICTORY_ASYNC_SUBMIT").is_none_or(|v| v != "0") && crate::host::late() && cuts();
        on && worker_started(std::thread::Builder::new().name("victory-submit".into()).spawn(worker))
    })
}

#[cfg(windows)]
fn worker_started(result: std::io::Result<std::thread::JoinHandle<()>>) -> bool {
    match result {
        Ok(_) => {
            tracing::info!("submissions are prepared and sent by the submission worker (VICTORY_ASYNC_SUBMIT=1)");
            true
        }
        Err(error) => {
            tracing::warn!(%error, "could not start the submission worker; submissions remain synchronous");
            false
        }
    }
}

/// Whether there is something for [`worker`] to look at.
#[cfg(windows)]
static WORK: (std::sync::Mutex<bool>, std::sync::Condvar) = (std::sync::Mutex::new(false), std::sync::Condvar::new());

#[cfg(windows)]
fn wake() {
    *lock(&WORK.0) = true;
    WORK.1.notify_one();
}

/// The thread of [`behind`]: runs what queues hold as far as it is free
/// to run, each time something was added or a fence moved.
#[cfg(windows)]
fn worker() {
    crate::fast_cores::select("victory-submit");
    frame_wait::start_worker(false);
    let mut blocked = false;
    loop {
        {
            let mut work = lock(&WORK.0);
            while !*work {
                let stage = if blocked { Stage::Dependency } else { Stage::Idle };
                if blocked && async_present() {
                    // A title store to a mapped value has no wake call. At
                    // the frame limit there may be no further submissions to
                    // wake us either. Recheck the head without forcing it.
                    let (next, timeout) = frame_wait::during(stage, || {
                        WORK.1.wait_timeout(work, std::time::Duration::from_millis(4))
                            .unwrap_or_else(|e| e.into_inner())
                    });
                    work = next;
                    if timeout.timed_out() {
                        break;
                    }
                } else {
                    work = frame_wait::during(stage, || WORK.1.wait(work).unwrap_or_else(|e| e.into_inner()));
                }
            }
            *work = false;
        }
        blocked = release_held_now(false);
    }
}

#[cfg(windows)]
thread_local! {
    /// This thread is inside [`release_held_now`].
    static RELEASING_HERE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Before a submission of ours that is waited for, with [`behind`]: what
/// the queues hold and is free to run runs first, on this thread, so the
/// submission keeps its place behind them.
#[cfg(windows)]
pub(crate) fn before_sync() {
    if behind() && !RELEASING_HERE.get() {
        release_held_now(false);
    }
}

#[cfg(windows)]
fn release_held_now(all: bool) -> bool {
    // One caller at a time, so a queue's submissions keep their order.
    static RELEASING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    if lock(&WAITING).is_empty() {
        return false;
    }
    let _one = {
        let _waiting = frame_wait::present_releaser();
        lock(&RELEASING)
    };
    let was_here = RELEASING_HERE.replace(true);
    struct Leave(bool);
    impl Drop for Leave {
        fn drop(&mut self) {
            RELEASING_HERE.set(self.0);
        }
    }
    let _leave = Leave(was_here);
    loop {
        let queues = lock(&WAITING).clone();
        let mut moved = false;
        let mut blocked = false;
        for (q, line) in queues {
            let mut held = lock(&line.commands);
            while !line.retired.load(Ordering::Acquire) {
                match held.first() {
                    None | Some(Held::Running) => break,
                    Some(Held::Wait(fence, value))
                        if !all && crate::fence::completed(fence.as_this()) < *value =>
                    {
                        blocked = true;
                        break;
                    }
                    Some(&Held::Value(wait)) if !all => {
                        let met = if behind() {
                            // late_met can wait for GPU writes to reach title memory.
                            // RELEASING owns this head; producers may append while the
                            // wait is checked but cannot bypass it. The Arc identifies
                            // this queue even if the title releases it and reuses its address.
                            drop(held);
                            let met = value_met(&wait);
                            held = lock(&line.commands);
                            met && !line.retired.load(Ordering::Acquire)
                        } else {
                            value_met(&wait)
                        };
                        if !met {
                            blocked = true;
                            break;
                        }
                    }
                    Some(_) => {}
                }
                match held.remove(0) {
                    Held::Wait(..) | Held::Running => {}
                    Held::Value(wait) => {
                        // Passed at a present without being met: what the
                        // queue stood at.
                        if all && !value_met(&wait) {
                            static SEEN: AtomicU64 = AtomicU64::new(0);
                            if SEEN.fetch_add(1, Ordering::Relaxed) < 16 {
                                tracing::warn!(
                                    queue = format_args!("{q:#x}"),
                                    ?wait,
                                    "a wait for a memory value was not met at a present: the queue runs on"
                                );
                            }
                        }
                    }
                    Held::Present(show) => {
                        // RELEASING still owns this head. Leave the line open for
                        // the next frame's lists and signals while DXGI presents.
                        held.insert(0, Held::Running);
                        drop(held);
                        show();
                        held = lock(&line.commands);
                        held.remove(0);
                    }
                    Held::Run(lists, order) => {
                        #[cfg(feature = "diagnostics")]
                        crate::capture::op(q, || {
                            format!(
                                "RUN held submission of {} lists (forced {all})",
                                order.len()
                            )
                        });
                        // Diagnostics: the part behind this one stands at
                        // waits that are met already, so the two could have
                        // been one host submission (one wait for the card,
                        // each range shaders write compared once).
                        #[cfg(feature = "diagnostics")]
                        {
                            let waits = held.iter().take_while(|h| matches!(h, Held::Value(_))).count();
                            if waits > 0
                                && matches!(held.get(waits), Some(Held::Run(..)))
                                && held[..waits].iter().all(|h| matches!(h, Held::Value(w) if w.met()))
                            {
                                crate::copystat::add(crate::copystat::C::PartsTogether, 1);
                            }
                        }
                        if crate::host::late()
                            && let Some(host) = crate::host::host()
                        {
                            // Not waited for: the host keeps the lists
                            // until they are settled.
                            if behind() && !all {
                                if crate::watch::ahead::enabled()
                                    && let Some(Held::Run(next, order)) = held.first()
                                    && let Some(&first) = order.first()
                                {
                                    next[first].host().prepare_guarded_pages();
                                }
                                // The line is open while this part is
                                // prepared and sent: only this thread takes
                                // from its head (one releaser at a time).
                                held.insert(0, Held::Running);
                                drop(held);
                                host.execute_late(lists, order);
                                held = lock(&line.commands);
                                if matches!(held.first(), Some(Held::Running)) {
                                    held.remove(0);
                                }
                            } else {
                                host.execute_late(lists, order);
                            }
                        } else {
                            let ran = {
                                let runs: Vec<&crate::host::HostList> =
                                    order.iter().map(|&i| lists[i].host()).collect();
                                match crate::host::host() {
                                    Some(host) => {
                                        host.execute(&runs);
                                        true
                                    }
                                    None => false,
                                }
                            };
                            // They have run: their host lists are used again.
                            if ran {
                                crate::list::recycle(lists, &order);
                            }
                        }
                        if all {
                            #[cfg(feature = "diagnostics")]
                            crate::copystat::add(crate::copystat::C::Forced, 1);
                            static ONCE: AtomicBool = AtomicBool::new(false);
                            if !ONCE.swap(true, Ordering::Relaxed) {
                                tracing::warn!(
                                    "a queue still held a submission at a present: its fence was not reached"
                                );
                            }
                        }
                    }
                    // Behind what was sent to the card and not waited
                    // for, if anything was.
                    Held::Signal(fence, value) => {
                        if !crate::host::late_signal(fence.as_this(), value) {
                            crate::fence::set(fence.as_this(), value);
                        }
                    }
                }
                moved = true;
            }
            if held.is_empty() {
                unregister(&line);
            }
        }
        if !moved {
            return blocked;
        }
    }
}

/// Whether a queue may pass `wait`: memory holds the value, or, with
/// submissions sent to the card without a wait (`crate::host::late`), it
/// will once those in the air have run.
#[cfg(windows)]
fn value_met(wait: &crate::list::ValueWait) -> bool {
    wait.met() || (crate::host::late() && crate::host::late_met(wait))
}

fn queue<'a>(this: This) -> &'a Queue {
    // SAFETY: called from queue methods only.
    unsafe { &*this.cast::<Queue>() }
}

pub(crate) extern "system" fn create(
    _: This,
    desc: *const QueueDesc,
    iid: *const Iid,
    out: *mut This,
) -> HRESULT {
    // SAFETY: the title passes a description (null is tolerated).
    let Some(&desc) = (unsafe { desc.as_ref() }) else {
        return E_INVALIDARG;
    };
    tracing::debug!(?desc, "command queue");
    let queue = Box::new(Queue {
        object: if crate::device::newer() {
            Object::new(&NEWER_FUNCTIONS)
        } else {
            Object::new(&FUNCTIONS)
        },
        kind: desc.kind,
        desc,
        #[cfg(windows)]
        held: Default::default(),
    });
    object::hand_out(QUEUE, queue, iid, &QUEUE_IIDS, out)
}

/// The console's queue description: an engine or pipe and a queue on it
/// in place of PC's priority and node mask.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct QueueDescX {
    kind: u32,
    flags: u32,
    engine_or_pipe: u32,
    queue: u32,
}

/// Device method `CreateCommandQueueX`. The host picks engines itself, so
/// the queue is created as on PC.
pub(crate) extern "system" fn create_x(
    this: This,
    desc: *const QueueDescX,
    iid: *const Iid,
    out: *mut This,
) -> HRESULT {
    // SAFETY: the title passes a description (null is tolerated).
    let Some(&x) = (unsafe { desc.as_ref() }) else {
        return E_INVALIDARG;
    };
    tracing::debug!(desc = ?x, "console command queue");
    let desc = QueueDesc {
        kind: x.kind,
        priority: 0,
        flags: x.flags,
        nodes: 1,
    };
    create(this, &desc, iid, out)
}

extern "system" fn query(this: This, iid: *const Iid, out: *mut This) -> HRESULT {
    object::answer(
        QUEUE,
        iid,
        object::is(iid, &QUEUE_IIDS).then_some(this),
        out,
    )
}
extern "system" fn release(this: This) -> u32 {
    object::release_boxed::<Queue>(this)
}

/// Runs the lists' host lists and waits for them (decision 0007). A queue
/// that waits for a fence keeps them until the fence is reached.
extern "system" fn execute(this: This, count: u32, lists: *const This) {
    let _call = crate::title_hold::begin();
    if lists.is_null() {
        return;
    }
    #[cfg(windows)]
    if let Some(host) = crate::host::host() {
        // SAFETY: the title passes `count` lists.
        let lists = unsafe { std::slice::from_raw_parts(lists, count as usize) };
        if cuts() && !this.is_null() {
            submit_cut(this, lists);
            return;
        }
        if waits() && split() && !this.is_null() {
            submit_split(this, lists);
            return;
        }
        // A list that waits for a memory value not there yet: the queue
        // holds the submission, as it does behind a fence.
        if waits() && !this.is_null() {
            let unmet = crate::list::unmet_waits(lists);
            if !unmet.is_empty() {
                static SEEN: AtomicU64 = AtomicU64::new(0);
                if SEEN.fetch_add(1, Ordering::Relaxed) < 8 {
                    tracing::info!(kind = queue(this).kind, ?unmet, "a submission is held for a memory value");
                }
                lock(&queue(this).held.commands).extend(unmet.into_iter().map(Held::Value));
                register(this);
            }
        }
        let kept = hold(this, || {
            let (taken, order) = crate::list::detach(lists);
            Held::Run(taken, order)
        });
        // Diagnostics: the order submissions are made in, next to the
        // commands of the frame's `ops` file (which are in recording order).
        #[cfg(feature = "diagnostics")]
        crate::capture::op(this as usize, || {
            let kind = (!this.is_null()).then(|| queue(this).kind);
            // The lists as the frame's other lines name them (their host sides).
            let named: Vec<String> = lists
                .iter()
                .map(|&l| match crate::list::host_list(l) {
                    Some(h) => format!("{:#x}", std::ptr::from_ref(h) as usize),
                    None => format!("title {l:?}"),
                })
                .collect();
            format!("SUBMIT queue kind {kind:?} held {kept} lists {named:?}")
        });
        if kept {
            return;
        }
        let host_lists: Vec<_> = lists
            .iter()
            .filter_map(|&l| crate::list::host_list(l))
            .collect();
        host.execute(&host_lists);
        crate::list::tables_done(lists);
        // What the lists wrote at the bottom of the pipe may be a fence
        // another queue waits for.
        release_held(false);
        return;
    }
    let _ = this;
    static ONCE: AtomicBool = AtomicBool::new(false);
    if !ONCE.swap(true, Ordering::Relaxed) {
        tracing::warn!(count, "command lists submitted; no host GPU runs them");
    }
}

/// The start of a present plane: frame token, then the plane's resources.
#[repr(C)]
pub(crate) struct Plane {
    token: u64,
    count: u32,
    resources: *const This,
}
/// Submission tagged with the frame it belongs to `(count, lists, frame
/// token, flags)`; frames are paced elsewhere, so it runs as a plain one.
extern "system" fn execute_x(this: This, count: u32, lists: *const This, _token: u64, _flags: u32) {
    execute(this, count, lists);
}
/// Signal with sync settings for the top and bottom of the pipe; our
/// submissions are complete when they return, so it signals at once.
extern "system" fn signal2_x(
    this: This,
    fence: This,
    value: u64,
    _bottom: *const u8,
    _top: *const u8,
) -> HRESULT {
    signal(this, fence, value)
}
/// Markers and events only matter to profilers.
extern "system" fn marker(_: This) {}
extern "system" fn signal(this: This, fence: This, value: u64) -> HRESULT {
    if fence.is_null() {
        return E_INVALIDARG;
    }
    // A queue that waits signals after what it holds.
    #[cfg(windows)]
    if hold(this, || Held::Signal(crate::fence::FenceRef::retain(fence), value)) {
        return S_OK;
    }
    // Behind the submissions sent to the card and not waited for: the
    // fence is set when they are settled.
    #[cfg(windows)]
    if crate::host::late_signal(fence, value) {
        return S_OK;
    }
    let _ = this;
    crate::fence::signal(fence, value)
}
/// From here on the queue runs nothing it is given until `fence` reaches
/// `value`: how the console orders work between queues (the title's
/// compute queue waits for the depth the graphics queue draws). Our
/// submissions run when they are made, so a wait for a value already
/// reached is nothing; otherwise what the queue is given next is held
/// ([`Held`]) until a signal or a list's bottom-of-pipe write gets the
/// fence there.
extern "system" fn wait(this: This, fence: This, value: u64) -> HRESULT {
    #[cfg(windows)]
    let _call = frame_wait::time(Call::QueueFence);
    if fence.is_null() {
        return E_INVALIDARG;
    }
    #[cfg(windows)]
    if !this.is_null() {
        let now = crate::fence::completed(fence);
        // Whether the title uses this at all (the first calls).
        {
            static CALLS: AtomicU64 = AtomicU64::new(0);
            if CALLS.fetch_add(1, Ordering::Relaxed) < 8 {
                tracing::info!(kind = queue(this).kind, now, value, "a queue is told to wait for a fence");
            }
        }
        #[cfg(feature = "diagnostics")]
        crate::capture::op(this as usize, || format!("WAIT queue kind {} for fence {fence:?} value {value} (now {now})", queue(this).kind));
        let behind = {
            let mut held = lock(&queue(this).held.commands);
            let behind = !held.is_empty() || now < value;
            if behind && waits() {
                held.push(Held::Wait(crate::fence::FenceRef::retain(fence), value));
            }
            behind
        };
        if behind {
            static SEEN: AtomicU64 = AtomicU64::new(0);
            if SEEN.fetch_add(1, Ordering::Relaxed) < 8 {
                tracing::info!(
                    kind = queue(this).kind,
                    now,
                    value,
                    held = waits(),
                    "a queue waits for a fence not reached yet"
                );
            }
        }
        if behind && waits() {
            register(this);
            // The fence may have got there meanwhile.
            release_held(false);
        }
    }
    let _ = (this, value);
    S_OK
}
/// Ticks per second of GPU timestamps (see [`Written::GpuTimestamp`]).
extern "system" fn timestamp_frequency(_: This, out: *mut u64) -> HRESULT {
    if out.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: checked non-null.
    unsafe { out.write(TIMESTAMP_FREQUENCY) };
    S_OK
}
const TIMESTAMP_FREQUENCY: u64 = 100_000_000;

/// What a list's bottom-of-pipe write stores when the list has run.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Written {
    Value(u64),
    /// The GPU clock: [`TIMESTAMP_FREQUENCY`] ticks per second since the
    /// process started (our GPU work is done on the CPU's timeline).
    GpuTimestamp,
    /// The CPU's performance counter.
    SystemTimestamp,
}

impl Written {
    /// Stores the low `bytes` (4 or 8) of the value at `at`.
    pub(crate) fn write(self, at: *mut u8, bytes: usize) {
        let value = match self {
            Written::Value(v) => v,
            Written::GpuTimestamp => {
                static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
                let elapsed = START.get_or_init(std::time::Instant::now).elapsed();
                (elapsed.as_nanos() / (1_000_000_000 / u128::from(TIMESTAMP_FREQUENCY))) as u64
            }
            Written::SystemTimestamp => system_timestamp(),
        };
        // SAFETY: the title names writable GPU memory (unified with ours).
        unsafe {
            if bytes == 4 {
                at.cast::<u32>().write_unaligned(value as u32);
            } else {
                at.cast::<u64>().write_unaligned(value);
            }
        }
    }
}

fn system_timestamp() -> u64 {
    #[cfg(windows)]
    {
        let mut count = 0;
        // SAFETY: a valid out pointer.
        unsafe { windows_sys::Win32::System::Performance::QueryPerformanceCounter(&mut count) };
        count as u64
    }
    #[cfg(not(windows))]
    0
}

/// Frames presented so far.
static FRAMES: AtomicU64 = AtomicU64::new(0);

#[cfg(any(windows, feature = "diagnostics"))]
pub(crate) fn presented() -> u64 {
    FRAMES.load(Ordering::Relaxed)
}

/// Mark a guest presentation and preserve the original power-of-two checkpoints.
fn mark_present() -> (u64, Option<(u64, u64)>) {
    crate::title_hold::presented();
    let advance = || {
        let frame = FRAMES.fetch_add(1, Ordering::Relaxed) + 1;
        (
            frame,
            frame.is_power_of_two().then(crate::list::take_counts),
        )
    };
    #[cfg(windows)]
    if kelvaroq_render::pacing::workload_enabled() {
        // Only PresentX drains list counters. Serialize snapshots and carry forward
        // the drained totals, leaving command-recording threads unchanged.
        static DRAINED: std::sync::Mutex<(u64, u64)> = std::sync::Mutex::new((0, 0));
        let mut drained = DRAINED.lock().unwrap_or_else(|e| e.into_inner());
        let (frame, checkpoint) = advance();
        let pending = checkpoint.unwrap_or_else(crate::list::counts);
        let totals = (drained.0 + pending.0, drained.1 + pending.1);
        if checkpoint.is_some() {
            *drained = totals;
        }
        kelvaroq_render::pacing::workload_queue(
            frame,
            totals.0,
            crate::draw::drawn(),
            totals.1,
            crate::draw::dispatched(),
        );
        return (frame, checkpoint);
    }
    advance()
}

#[cfg(windows)]
fn drain_for_present() {
    let _drain = frame_wait::time(Call::Drain);
    // Submissions sent to the card and not waited for are settled first,
    // and what that frees runs in its own order, before anything is forced.
    if crate::host::late() && !crate::host::late_free_present() {
        // (With `behind`: what the queues hold runs here first, and what
        // it sent is settled with the rest.)
        if behind() {
            release_held_now(false);
        }
        crate::host::settle_for_present();
        release_held_now(false);
        crate::host::settle_for_present();
    } else if crate::host::late() {
        // The present is sent ahead too: what waits for a value a flight
        // will write runs behind that flight, and nothing is settled here.
        release_held_now(false);
    }
    // shortcut: what a queue still holds at a present runs now, whatever
    // its fence: a wait is not kept across frames.
    release_held(forces());
}

/// The title may record the next frame while our worker prepares this one
/// (on by default; `VICTORY_ASYNC_PRESENT=0`: the present waits as before).
/// Only the texture and the closed lists are retained here. The ordinary
/// submit-time refresh still observes title writes, and a title that reuses
/// their storage must wait for its fence, as it does with async submit.
///
/// Measured in the heavy prologue (RTX 5080, 300 s each, same save): the
/// title's time in the present call fell to 0.1 ms a frame; median heavy
/// frame 115.5 -> 108.5 ms, p95 568 -> 262 ms, 1,022 -> 1,648 heavy frames.
/// The submitting thread is then the frame's critical path.
#[cfg(windows)]
pub(crate) fn async_present() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        let enabled = std::env::var_os("VICTORY_ASYNC_PRESENT").is_none_or(|v| v != "0")
            && behind() && crate::host::late_free_present();
        if enabled {
            tracing::info!(frames_ahead = frames_ahead(), "presents run behind the lists on their queue");
        }
        enabled
    })
}

/// Two queued or running presents by default. A finite maximum also
/// bounds the number of retained back-buffer copies for a mistyped limit.
#[cfg(windows)]
fn frames_ahead() -> usize {
    static LIMIT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *LIMIT.get_or_init(|| {
        std::env::var("VICTORY_FRAMES_AHEAD").ok()
            .and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(2).clamp(1, 8)
    })
}

/// A slot belongs to both the queued present and its GPU flight. The last
/// owner returns it, including on errors. A change of presenting queue
/// drains older frames so display order holds across queues as well.
#[cfg(windows)]
#[derive(Default)]
struct FrameLimit {
    active: Mutex<(usize, usize)>, // presenting queue, number of slots
    changed: std::sync::Condvar,
}

#[cfg(windows)]
pub(crate) struct FrameSlot<'a>(&'a FrameLimit);

#[cfg(windows)]
impl FrameLimit {
    fn enter(&self, queue: usize, limit: usize) -> FrameSlot<'_> {
        let mut active = lock(&self.active);
        let available = |&(owner, count): &(usize, usize)| count == 0 || (owner == queue && count < limit);
        if !available(&active) {
            let _pacing = frame_wait::time(Call::Pacing);
            active = self.changed.wait_while(active, |state| !available(state)).unwrap_or_else(|e| e.into_inner());
        }
        *active = (queue, active.1 + 1);
        FrameSlot(self)
    }
}

#[cfg(windows)]
impl Drop for FrameSlot<'_> {
    fn drop(&mut self) {
        lock(&self.0.active).1 -= 1;
        self.0.changed.notify_all();
    }
}

#[cfg(windows)]
fn enqueue_present(this: This, show: impl FnOnce() + Send + 'static) {
    // Keep the queue alive even if the title releases its last reference
    // before the worker reaches this frame. After show, Running still
    // keeps later commands behind it until the releaser retakes the lock.
    object::add_ref(this);
    let address = this as usize;
    lock(&queue(this).held.commands).push(Held::Present(Box::new(move || {
        show();
        release(address as This);
    })));
    register(this);
    wake();
}

#[cfg(windows)]
fn present_copy(planes: u32, parameters: *const Plane) -> Option<(usize, Arc<crate::resource::HostTexture>)> {
    if planes == 0 {
        return None;
    }
    // SAFETY: the title supplies the plane and its resources for this call.
    let plane = unsafe { parameters.as_ref() }?;
    if plane.count == 0 || plane.resources.is_null() {
        return None;
    }
    let resource = unsafe { *plane.resources };
    Some((resource as usize, crate::resource::host_copy(resource)?))
}

fn note_present() -> u64 {
    let (frame, checkpoint) = mark_present();
    if let Some((draws, dispatches)) = checkpoint {
        #[cfg(windows)]
        let (drawn, dispatched) = {
            static DRAWN: AtomicU64 = AtomicU64::new(0);
            static DISPATCHED: AtomicU64 = AtomicU64::new(0);
            let since = |last: &AtomicU64, now: u64| now - last.swap(now, Ordering::Relaxed);
            (
                since(&DRAWN, crate::draw::drawn()),
                since(&DISPATCHED, crate::draw::dispatched()),
            )
        };
        #[cfg(not(windows))]
        let (drawn, dispatched) = (0, 0);
        tracing::info!(
            frame,
            draws,
            drawn,
            dispatches,
            dispatched,
            "frame presented (since the last line: draws and dispatches, and those run on the host)"
        );
        #[cfg(windows)]
        if frame >= 256 {
            let (hits, misses) = crate::shader_cache::counts();
            tracing::info!(hits, misses, "shaders from the cache and translated so far");
        }
    }
    #[cfg(all(windows, feature = "diagnostics"))]
    {
        crate::capture::frame_drew(frame, crate::draw::drawn());
        if crate::capture::listed(frame) {
            crate::capture::dump_targets(frame);
        }
    }
    frame
}

/// A capture still waits on the title thread. Take its measurements and
/// copy its targets at the queue head, after the earlier lists have run
/// through preparation. Even a synchronous capture cannot force a wait.
#[cfg(windows)]
fn present_in_order(
    this: This,
    source: Option<(usize, Arc<crate::resource::HostTexture>)>,
    layers: Option<Vec<crate::dxgi::planes::Layer>>,
    limit: &'static FrameLimit,
) {
    #[cfg(feature = "diagnostics")]
    let capture = crate::capture::wanted(presented() + 1) || crate::capture::listed(presented() + 1);
    #[cfg(not(feature = "diagnostics"))]
    let capture = false;
    let synchronous = capture || source.is_none();
    let slot = Arc::new(limit.enter(this as usize, if synchronous { 1 } else { frames_ahead() }));
    let frame = (!synchronous).then(note_present);
    let (done, wait) = synchronous.then(std::sync::mpsc::channel).unzip();
    enqueue_present(this, move || {
        let frame = frame.unwrap_or_else(note_present);
        if let Some((_id, source)) = source && let Some(host) = crate::host::host() {
            #[cfg(feature = "diagnostics")]
            let state = source.state.load(Ordering::Relaxed);
            let image = match &layers {
                Some(planes) => crate::host::PresentSource::Planes(planes, Some(&slot)),
                None => crate::host::PresentSource::Queued(&source, &slot),
            };
            let _shown = host.present(image, frame);
            #[cfg(feature = "diagnostics")]
            blinks(frame, _id as This, state, _shown);
        }
        if let Some(done) = done {
            let _ = done.send(());
        }
    });
    if let Some(wait) = wait {
        let _drain = frame_wait::time(Call::Drain);
        wait.recv().expect("the queued present completes");
    } else {
        frame_wait::queued();
    }
}

/// Shows the first plane's first resource in our window.
///
/// shortcut: one plane, no scaling or placement, the source expected in
/// the common state.
extern "system" fn present(
    this: This,
    planes: u32,
    plane_parameters: *const Plane,
    _parameters: *const u8,
) -> HRESULT {
    #[cfg(windows)]
    let _call = frame_wait::time(Call::Present);
    #[cfg(all(windows, feature = "diagnostics"))]
    crate::copystat::present_begins();
    #[cfg(all(windows, feature = "diagnostics"))]
    crate::profile::frame();
    // Serialize title presents only in the opt-in mode. This reserves the
    // frame number and FIFO position together. Neither worker takes CALLS.
    #[cfg(windows)]
    static CALLS: Mutex<()> = Mutex::new(());
    #[cfg(windows)]
    static SLOTS: std::sync::LazyLock<FrameLimit> = std::sync::LazyLock::new(FrameLimit::default);
    #[cfg(windows)]
    let _serial = async_present().then(|| {
        let _turn = frame_wait::time(Call::PresentTurn);
        lock(&CALLS)
    });
    #[cfg(windows)]
    let layers = crate::dxgi::planes::snapshot(planes, plane_parameters);
    #[cfg(windows)]
    if _serial.is_some() && !this.is_null() {
        let source = layers
            .as_ref()
            .map(|p| (p[0].id, p[0].copy.clone()))
            .or_else(|| present_copy(planes, plane_parameters));
        present_in_order(this, source, layers, &SLOTS);
        return S_OK;
    }
    #[cfg(windows)]
    let _older = _serial.as_ref().map(|_| SLOTS.enter(0, 1));
    #[cfg(windows)]
    drain_for_present();
    let frame = note_present();
    #[cfg(windows)]
    if let Some(planes) = layers
        && let Some(host) = crate::host::host()
    {
        let _show = frame_wait::time(Call::Show);
        let _ = host.present(crate::host::PresentSource::Planes(&planes, None), frame);
        return S_OK;
    }
    #[cfg(windows)]
    if planes > 0
        // SAFETY: the title passes `planes` plane descriptions.
        && let Some(plane) = unsafe { plane_parameters.as_ref() }
        && plane.count > 0
        && !plane.resources.is_null()
        // SAFETY: the plane lists `count` resources.
        && let resource = unsafe { *plane.resources }
        && let Some(texture) = crate::resource::host_resource(resource)
        && let Some(host) = crate::host::host()
    {
        const COPY_SOURCE: u32 = 0x800;
        let state = crate::resource::host_transition(resource, COPY_SOURCE)
            .map_or(COPY_SOURCE, |(_, before)| before);
        let shown = {
            let _show = frame_wait::time(Call::Show);
            host.present(crate::host::PresentSource::Immediate(texture, state), frame)
        };
        crate::resource::host_transition(resource, state);
        #[cfg(feature = "diagnostics")]
        blinks(frame, resource, state, shown);
        #[cfg(not(feature = "diagnostics"))]
        let _ = shown;
    }
    #[cfg(not(windows))]
    let _ = (this, planes, plane_parameters, frame);
    S_OK
}
/// Diagnostics: logs a presented frame that is black after one that was
/// not, with what was presented and how many draws ran since.
#[cfg(all(windows, feature = "diagnostics"))]
fn blinks(frame: u64, resource: This, state: u32, shown: Option<bool>) {
    static LAST: std::sync::Mutex<(bool, u64, u64)> = std::sync::Mutex::new((false, 0, 0));
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let drawn = crate::draw::drawn();
    let black = shown.unwrap_or(false);
    let draws = crate::draw::take_frame();
    if black && !last.0 {
        last.2 += 1;
        tracing::warn!(
            frame,
            resource = ?resource,
            state = format_args!("{state:#x}"),
            draws = drawn - last.1,
            copied = shown.is_some(),
            black_frames = last.2,
            levels = ?crate::capture::recent_levels(),
            "a black frame after a frame that was not"
        );
        for d in draws {
            tracing::warn!("black frame {frame}: {d}");
        }
        AFTER.store(frame + 6, Ordering::Relaxed);
    }
    // Six frames on: the levels around it (a fade or a single drop).
    static AFTER: AtomicU64 = AtomicU64::new(0);
    if AFTER.load(Ordering::Relaxed) == frame {
        tracing::warn!(frame, levels = ?crate::capture::recent_levels(), "levels after the black frame");
    }
    *last = (black, drawn, last.2);
}

/// A run of destination pages: how they are mapped (0 pool pages from
/// `start` on, 1 pool page `start` each, 2 unmapped, 3 left as they are)
/// and how many.
#[repr(C)]
pub(crate) struct PageRange {
    kind: u32,
    start: u32,
    count: u32,
}

/// The ranges mapped from a destination address on.
#[repr(C)]
pub(crate) struct PageBatch {
    destination: u64,
    count: u32,
    ranges: *const PageRange,
}

/// Bytes per mapped page.
const PAGE: u64 = 64 << 10;

/// `CopyPageMappingsBatchX(count, batches, pool, pool pages, flags)`: maps
/// pages of a pool (memory the title mapped physical pages into) at the
/// batches' addresses. The title streams textures into pool pages
/// and maps them into its tiled heap this way (title+0xf2c470, ranges of
/// one constant page or of unmapped pages). Done at once: every earlier
/// submission has run.
extern "system" fn copy_page_mappings(
    _: This,
    count: u32,
    batches: *const PageBatch,
    pool: u64,
    pool_pages: u32,
    _flags: u32,
) -> HRESULT {
    let Some(map) = crate::resource::page_mapper() else {
        return E_INVALIDARG;
    };
    if batches.is_null() && count > 0 {
        return E_INVALIDARG;
    }
    let batches = (count > 0).then(|| {
        // SAFETY: the title passes `count` batches.
        unsafe { std::slice::from_raw_parts(batches, count as usize) }
    });
    let mut ok = true;
    for b in batches.into_iter().flatten() {
        if b.ranges.is_null() {
            continue;
        }
        // SAFETY: each batch holds `count` ranges.
        let ranges = unsafe { std::slice::from_raw_parts(b.ranges, b.count as usize) };
        let mut at = b.destination;
        for r in ranges {
            let len = u64::from(r.count) * PAGE;
            let inside = |pages: u32| r.start.checked_add(pages).is_some_and(|e| e <= pool_pages);
            let source = pool + u64::from(r.start) * PAGE;
            ok &= match r.kind {
                0 if inside(r.count) => map(at, source, len),
                1 if inside(1) => (0..u64::from(r.count)).all(|k| map(at + k * PAGE, source, PAGE)),
                2 => map(at, 0, len),
                3 => true,
                _ => false,
            };
            at += len;
        }
    }
    if !ok {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| tracing::warn!(pool = format_args!("{pool:#x}"), "a page mapping failed"));
    }
    S_OK
}

extern "system" fn desc(this: This, out: *mut QueueDesc) {
    if !out.is_null() {
        // SAFETY: checked non-null; the title passes room for a description.
        unsafe { out.write(queue(this).desc) };
    }
}

/// The queue methods by their slot in older titles' queues (the 2020-era
/// layout): IUnknown 3, object 4, device child 1, queue 26 (12 as on PC,
/// then console ones).
const METHODS: [(usize, *const ()); 15] = [
    (0, query as *const ()),
    (2, release as *const ()),
    (7, object::parent_device as *const ()), // 0x38
    (10, execute as *const ()),              // 0x50
    (11, marker as *const ()),               // 0x58 set marker
    (12, marker as *const ()),               // 0x60 begin event
    (13, marker as *const ()),               // 0x68 end event
    (14, signal as *const ()),               // 0x70
    (15, wait as *const ()),                 // 0x78
    (16, timestamp_frequency as *const ()),  // 0x80
    (26, desc as *const ()),                 // 0xd0
    (29, copy_page_mappings as *const ()),   // 0xe8 page mapping batch
    (30, present as *const ()),              // 0xf0
    (33, execute_x as *const ()),            // 0x108
    (32, signal2_x as *const ()),            // 0x100
];

/// Newer titles' queues have two more methods before Signal, which moved
/// from 0x70 to 0x80: every slot from 0x70 on is two later (taken here as
/// starting exactly at 0x70, the methods before it kept in place).
/// Inferred from the title's calls: Signal 0x80 `(fence,
/// value)`, timestamp frequency 0x90 (title+0x24841a6), suspend and resume
/// 0xb8/0xc0, PresentX 0x100, the frame-token submission 0x118 `(count,
/// lists, token, flags)` (title+0x24821dd).
const fn newer_slot(slot: usize) -> usize {
    if slot >= 14 { slot + 2 } else { slot }
}

const fn queue_table(newer: bool) -> [*const (); 36] {
    let mut f = object::d3d12_object(object::missing_table::<QUEUE, 36>());
    let mut i = 0;
    while i < METHODS.len() {
        let (slot, method) = METHODS[i];
        f[if newer { newer_slot(slot) } else { slot }] = method;
        i += 1;
    }
    f
}

static FUNCTIONS: Table<36> = Table(queue_table(false));
static NEWER_FUNCTIONS: Table<36> = Table(queue_table(true));

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::slot;

    #[cfg(windows)]
    #[test]
    fn failed_worker_creation_keeps_synchronous_submission() {
        assert!(!worker_started(Err(std::io::Error::other("no worker"))));
    }

    #[cfg(windows)]
    #[test]
    fn a_frame_slot_stays_taken_until_its_gpu_owner_leaves() {
        use std::sync::mpsc::{channel, RecvTimeoutError};
        use std::time::Duration;
        let limit = FrameLimit::default();
        let first = Arc::new(limit.enter(1, 2));
        let on_card = first.clone();
        let second = limit.enter(1, 2);
        drop(first);
        std::thread::scope(|scope| {
            let (trying, tried) = channel();
            let (entered, entry) = channel();
            let limit = &limit;
            scope.spawn(move || {
                trying.send(()).unwrap();
                let _third = limit.enter(1, 2);
                entered.send(()).unwrap();
            });
            tried.recv().unwrap();
            assert_eq!(entry.recv_timeout(Duration::from_millis(20)), Err(RecvTimeoutError::Timeout));
            drop(on_card);
            entry.recv_timeout(Duration::from_secs(2)).unwrap();
        });
        assert_eq!(*lock(&limit.active), (1, 1));
        drop(second);
        assert_eq!(*lock(&limit.active), (1, 0));
    }

    #[cfg(windows)]
    #[test]
    fn a_queued_present_retains_its_queue_and_follows_earlier_signals() {
        let null = std::ptr::null();
        let desc = QueueDesc { kind: 0, priority: 0, flags: 0, nodes: 1 };
        let (mut q, mut gate, mut preceding) = (null, null, null);
        assert_eq!(create(null, &desc, &QUEUE_IIDS[0], &mut q), S_OK);
        for out in [&mut gate, &mut preceding] {
            assert_eq!(crate::fence::create(null, 0, 0, &crate::fence::FENCE_IIDS[0], out), S_OK);
        }
        wait(q, gate, 1);
        signal(q, preceding, 3);
        let observed = crate::fence::FenceRef::retain(preceding);
        let (display, shown) = std::sync::mpsc::channel();
        enqueue_present(q, move || {
            crate::host::settle_for_present();
            display.send(crate::fence::completed(observed.as_this())).unwrap();
        });
        assert_eq!(release(q), 1);
        release_held_now(false);
        assert!(matches!(shown.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));
        crate::fence::signal(gate, 1);
        release_held_now(false);
        assert_eq!(shown.recv_timeout(std::time::Duration::from_secs(2)).unwrap(), 3);
        for f in [gate, preceding] {
            slot::<extern "system" fn(This) -> u32>(f, 0x10)(f);
        }
    }

    /// VICTORY_ASYNC_SUBMIT=1 VICTORY_D3D12_WARP=1 VICTORY_LATE_WAIT=3.
    /// An unknown bottom-of-pipe value requires a real GPU fence/CPU readback.
    /// Enqueue another recording while the worker is inside that wait.
    #[cfg(windows)]
    #[test]
    fn warp_value_wait_allows_enqueue_reset_and_queue_release() {
        use std::time::{Duration, Instant};
        use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0, WAIT_TIMEOUT};
        use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
        if std::env::var_os("VICTORY_D3D12_WARP").is_none_or(|v| v != "1")
            || std::env::var_os("VICTORY_ASYNC_SUBMIT").is_none_or(|v| v != "1")
            || !crate::host::late()
            || !cuts()
        {
            return;
        }
        assert!(behind(), "enable flights and cut waits");
        let host = crate::host::host().unwrap();
        let null = std::ptr::null();
        let desc = QueueDesc {
            kind: 0,
            priority: 0,
            flags: 0,
            nodes: 1,
        };
        let make_fence = || {
            let mut f = null;
            assert_eq!(
                crate::fence::create(null, 0, 0, &crate::fence::FENCE_IIDS[0], &mut f),
                S_OK
            );
            f
        };
        let drop_ref = |o| slot::<extern "system" fn(This) -> u32>(o, 0x10)(o);
        let event_at = |f, v| {
            let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
            assert!(!event.is_null());
            let notify =
                slot::<extern "system" fn(This, u64, *mut std::ffi::c_void) -> HRESULT>(f, 0x48);
            assert_eq!(notify(f, v, event), S_OK);
            event
        };
        for retire in [false, true] {
            let (mut q, mut alloc, mut list) = (null, null, null);
            assert_eq!(create(null, &desc, &QUEUE_IIDS[0], &mut q), S_OK);
            let old_line = Arc::downgrade(&queue(q).held);
            assert_eq!(
                crate::list::create_allocator(null, 0, &PAGEABLE_IIDS[1], &mut alloc),
                S_OK
            );
            assert_eq!(
                crate::list::create_list(null, 0, 0, alloc, null, &PAGEABLE_IIDS[1], &mut list),
                S_OK
            );
            let close = slot::<extern "system" fn(This) -> HRESULT>(list, 0x48);
            let reset = slot::<extern "system" fn(This, This, This) -> HRESULT>(list, 0x50);
            let wait64 = slot::<extern "system" fn(This, u64, u32, u64)>(
                list,
                if crate::device::newer() { 0x250 } else { 0x240 },
            );
            let (blocked, finished, landed) = (make_fence(), make_fence(), make_fence());
            let (finished_event, landed_event) = (event_at(finished, 9), event_at(landed, 7));
            let mut words = Box::new([0u64; 3]);
            let producer = host.new_list().unwrap();
            producer.defer_write((&raw mut words[0]).cast(), Written::SystemTimestamp, 8);
            producer.close();
            let (gate, inside_value_settle) = crate::host::gate_value_wait_test();
            let (allow_gpu, watchdog) = std::sync::mpsc::channel();
            let opener = std::thread::spawn(move || {
                let on_time = watchdog.recv_timeout(Duration::from_secs(5)).is_ok();
                unsafe { gate.Signal(1) }.unwrap();
                on_time
            });
            static LIMIT: std::sync::LazyLock<FrameLimit> = std::sync::LazyLock::new(FrameLimit::default);
            let frame_slot = Arc::new(LIMIT.enter(q as usize, 1));
            let in_flight = Arc::downgrade(&frame_slot);
            // The direct queue is behind gate. The same submission used
            // for a present copy must retain the slot after enqueue returns.
            host.execute_late(vec![crate::list::Detached::for_test(producer)], vec![0]);
            let copy = host.new_list().unwrap();
            copy.close();
            host.execute_own(&copy, Some(frame_slot));
            assert!(in_flight.upgrade().is_some());
            assert!(crate::host::late_signal(landed, 7));
            assert_eq!(drop_ref(landed), 1, "pending host signal owns the fence");
            wait64(list, (&raw const words[0]) as u64, 6, 0);
            crate::list::host_list(list).unwrap().defer_write(
                (&raw mut words[1]).cast(),
                Written::Value(11),
                8,
            );
            assert_eq!(close(list), S_OK);
            execute(q, 1, &list);
            let until = Instant::now() + Duration::from_secs(3);
            while !inside_value_settle() && Instant::now() < until {
                std::thread::yield_now();
            }
            let worker_waited = inside_value_settle();
            // A synchronous consumer must serialize with the worker. The title
            // can enqueue a fence wait and a second recording on the same queue.
            let (enter, concurrent) = std::sync::mpsc::channel();
            let other_consumer = std::thread::spawn(move || {
                enter.send(()).unwrap();
                before_sync();
            });
            concurrent.recv().unwrap();
            assert_eq!(wait(q, blocked, 1), S_OK);
            assert_eq!(reset(list, alloc, null), S_OK);
            crate::list::host_list(list).unwrap().defer_write(
                (&raw mut words[2]).cast(),
                Written::Value(22),
                8,
            );
            assert_eq!(close(list), S_OK);
            execute(q, 1, &list);
            assert_eq!(signal(q, finished, 9), S_OK);
            let (display, shown) = std::sync::mpsc::channel();
            if !retire {
                let address = (&raw const words[2]) as usize;
                enqueue_present(q, move || {
                    crate::host::settle_for_present();
                    // SAFETY: words outlives this command and the flight's
                    // write has landed under the settle above.
                    display.send(unsafe { *(address as *const u64) }).unwrap();
                });
            }
            let cpu_waiter = (!retire && async_present()).then(|| {
                let fence = crate::fence::FenceRef::retain(finished);
                let address = (&raw const words[2]) as usize;
                std::thread::spawn(move || {
                    let block = slot::<extern "system" fn(This, u64, *mut std::ffi::c_void) -> HRESULT>(
                        fence.as_this(), 0x48,
                    );
                    assert_eq!(block(fence.as_this(), 9, std::ptr::null_mut()), S_OK);
                    // SAFETY: the blocking wait observes the landed flight.
                    unsafe { *(address as *const u64) }
                })
            });
            let refs = drop_ref(finished);
            assert_eq!(drop_ref(list), 0);
            assert_eq!(drop_ref(alloc), 0);
            if retire {
                assert_eq!(release(q), 0);
            }
            let _ = allow_gpu.send(());
            let on_time = opener.join().unwrap();
            other_consumer.join().unwrap();
            before_sync();
            crate::host::settle_for_present();
            assert!(
                worker_waited,
                "the worker must have reached the GPU value wait"
            );
            assert!(
                on_time,
                "the producer could not enqueue before the GPU gate opened"
            );
            assert_eq!(refs, 1 + u32::from(cpu_waiter.is_some()), "the pending signal and CPU waiter own the fence");
            assert_eq!(
                unsafe { WaitForSingleObject(landed_event, 0) },
                WAIT_OBJECT_0
            );
            assert_ne!(words[0], 0);
            assert!(in_flight.upgrade().is_none(), "the landed copy returned its frame slot");
            assert_eq!(words[1], if retire { 0 } else { 11 });
            assert_eq!(words[2], 0);
            assert_eq!(
                unsafe { WaitForSingleObject(finished_event, 0) },
                WAIT_TIMEOUT
            );
            if retire {
                let until = Instant::now() + Duration::from_secs(2);
                while old_line.strong_count() != 0 && Instant::now() < until {
                    std::thread::yield_now();
                }
                assert_eq!(old_line.strong_count(), 0);
            } else {
                assert!(matches!(shown.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));
                assert_eq!(crate::fence::signal(blocked, 1), S_OK);
                before_sync();
                crate::host::settle_for_present();
                assert_eq!(
                    unsafe { WaitForSingleObject(finished_event, 2000) },
                    WAIT_OBJECT_0
                );
                assert_eq!(shown.recv_timeout(Duration::from_secs(2)).unwrap(), 22);
                if let Some(waiter) = cpu_waiter {
                    assert_eq!(waiter.join().unwrap(), 22);
                }
                assert_eq!(words[2], 22);
                assert_eq!(release(q), 0);
            }
            assert_eq!(
                drop_ref(blocked),
                0,
                "the consumed or cancelled wait dropped its reference"
            );
            unsafe {
                CloseHandle(finished_event);
                CloseHandle(landed_event);
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn worker_keeps_its_command_storage_after_queue_release() {
        let null = std::ptr::null();
        let desc = QueueDesc { kind: 0, priority: 0, flags: 0, nodes: 1 };
        let mut q = null;
        assert_eq!(create(null, &desc, &QUEUE_IIDS[0], &mut q), S_OK);
        lock(&queue(q).held.commands).extend([Held::Running, Held::Run(vec![], vec![])]);
        register(q);
        register(q);
        let worker = {
            let waiting = lock(&WAITING);
            let mut matches = waiting.iter().filter(|(_, line)| Arc::ptr_eq(line, &queue(q).held));
            let line = matches.next().expect("enregistrée").1.clone();
            assert!(matches.next().is_none());
            line
        };
        assert!(!worker.retired.load(Ordering::Acquire));
        assert_eq!(release(q), 0);
        assert!(worker.retired.load(Ordering::Acquire));
        assert!(!lock(&WAITING).iter().any(|(_, line)| Arc::ptr_eq(line, &worker)));
        assert!(matches!(&lock(&worker.commands)[..], [Held::Running, Held::Run(..)]));
    }

    /// Where a queue's signal is: newer layouts have it two slots later.
    /// Another test may switch layouts while this one runs, so the queue's
    /// own table decides.
    fn signal_slot(q: This) -> usize {
        // SAFETY: one of our queues; its first field is the table.
        let table = unsafe { *q.cast::<*const *const ()>() };
        if table == NEWER_FUNCTIONS.0.as_ptr() {
            0x80
        } else {
            0x70
        }
    }

    #[test]
    fn queue_signals_complete_at_once() {
        let null = std::ptr::null();
        let desc = QueueDesc {
            kind: 0,
            priority: 0,
            flags: 0,
            nodes: 0,
        };
        let mut q = null;
        assert_eq!(create(null, &desc, &QUEUE_IIDS[0], &mut q), S_OK);
        // SAFETY: the title reads the type at +0x10.
        assert_eq!(unsafe { q.cast::<u8>().add(0x10).cast::<u32>().read() }, 0);
        let mut fence = null;
        let fence_iid = &crate::fence::FENCE_IIDS[0];
        assert_eq!(
            crate::fence::create(null, 0, 0, fence_iid, &mut fence),
            S_OK
        );
        let signal = slot::<extern "system" fn(This, This, u64) -> HRESULT>(q, signal_slot(q));
        assert_eq!(signal(q, fence, 7), S_OK);
        assert_eq!(
            slot::<extern "system" fn(This) -> u64>(fence, 0x40)(fence),
            7
        );
        assert_eq!(slot::<extern "system" fn(This) -> u32>(q, 0x10)(q), 0);
    }

    /// The title's compute queue: `{COMPUTE, 0, pipe 1, queue 0}`.
    #[test]
    fn console_queues_are_created_as_on_pc() {
        let null = std::ptr::null();
        let desc = QueueDescX {
            kind: 2,
            flags: 0,
            engine_or_pipe: 1,
            queue: 0,
        };
        let mut q = null;
        assert_eq!(create_x(null, &desc, &QUEUE_IIDS[0], &mut q), S_OK);
        assert_eq!(queue(q).desc.kind, 2);
        let none = std::ptr::null();
        assert_eq!(create_x(null, none, &QUEUE_IIDS[0], &mut q), E_INVALIDARG);
    }

    #[test]
    fn newer_queues_have_their_methods_two_slots_later() {
        let (old, new) = (queue_table(false), queue_table(true));
        assert_eq!(old[10], new[10]); // submission stays at 0x50
        assert_eq!(old[14], new[16]); // signal 0x70 → 0x80
        assert_eq!(old[30], new[32]); // present 0xf0 → 0x100
        assert_eq!(new[35], execute_x as *const ()); // 0x118
    }

    #[test]
    fn timestamps_and_values_are_written() {
        let mut at = [0u64; 2];
        Written::Value(0x1_0000_0002).write(at.as_mut_ptr().cast(), 4);
        assert_eq!(at[0], 2);
        Written::GpuTimestamp.write(at[1..].as_mut_ptr().cast(), 8);
        let first = at[1];
        std::thread::sleep(std::time::Duration::from_millis(2));
        Written::GpuTimestamp.write(at[1..].as_mut_ptr().cast(), 8);
        assert!(at[1] > first);
    }
}
