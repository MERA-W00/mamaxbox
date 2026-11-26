//! Coordinate reads of known texture ranges with changes to whole Windows views.
//! A writer announces before waiting so new overlapping readers cannot starve it.

use std::ops::Range;
use std::sync::{Condvar, Mutex};

pub(super) const ALL: Range<usize> = 0..usize::MAX;
pub(super) static GATE: Gate = Gate::new();

struct State {
    readers: Vec<Range<usize>>,
    writers: Vec<Range<usize>>,
}

pub(super) struct Gate {
    state: Mutex<State>,
    changed: Condvar,
}

fn conflicts(active: &[Range<usize>], area: &Range<usize>) -> bool {
    active.iter().any(|other| area.start < other.end && other.start < area.end)
}

pub(super) struct Permit<'a> {
    gate: &'a Gate,
    area: Range<usize>,
    writer: bool,
}

impl Gate {
    const fn new() -> Self {
        Self {
            state: Mutex::new(State { readers: Vec::new(), writers: Vec::new() }),
            changed: Condvar::new(),
        }
    }

    pub(super) fn read(&self, area: Range<usize>, wait: bool) -> Option<Permit<'_>> {
        // The bookkeeping mutex never spans a mapping, copy, or user
        // callback. A disjoint reader need not wait for a writer to finish.
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !wait && conflicts(&state.writers, &area) {
            return None;
        }
        if wait {
            state = self.changed.wait_while(state, |s| conflicts(&s.writers, &area)).unwrap_or_else(|e| e.into_inner());
        }
        state.readers.push(area.clone());
        Some(Permit { gate: self, area, writer: false })
    }

    fn announce(&self, area: Range<usize>) -> Permit<'_> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).writers.push(area.clone());
        Permit { gate: self, area, writer: true }
    }

    pub(super) fn write(&self, area: Range<usize>) -> Permit<'_> {
        let permit = self.announce(area);
        permit.await_readers();
        permit
    }
}

impl Permit<'_> {
    pub(super) fn narrow(&mut self, area: Range<usize>) {
        assert!(!self.writer && self.area.start <= area.start && area.end <= self.area.end);
        let mut state = self.gate.state.lock().unwrap_or_else(|e| e.into_inner());
        let old = state.readers.iter_mut().find(|r| **r == self.area).expect("registered reader");
        *old = area.clone();
        self.area = area;
        self.gate.changed.notify_all();
    }

    fn await_readers(&self) {
        let state = self.gate.state.lock().unwrap_or_else(|e| e.into_inner());
        drop(self.gate.changed.wait_while(state, |s| conflicts(&s.readers, &self.area)).unwrap_or_else(|e| e.into_inner()));
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let mut state = self.gate.state.lock().unwrap_or_else(|e| e.into_inner());
        let active = if self.writer { &mut state.writers } else { &mut state.readers };
        let at = active.iter().position(|r| *r == self.area).expect("registered mapping scope");
        active.swap_remove(at);
        self.gate.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_views_and_unknown_writers_exclude_readers_while_disjoint_textures_advance() {
        let gate = Gate::new();
        let previous = gate.read(0x10000..0x20000, true).unwrap();
        // A writer replacing the middle page of a three-page Windows view
        // announces the full view. Keep a neighbouring texture reading.
        let writer = gate.announce(0x10000..0x40000);
        std::thread::scope(|threads| {
            let change = threads.spawn(move || {
                writer.await_readers();
                writer
            });
            assert!(gate.read(0x30000..0x40000, false).is_none());
            assert!(gate.read(ALL, false).is_none());
            let disjoint = gate.read(0x40000..0x50000, false).expect("an adjacent, disjoint texture must advance");
            assert!(!change.is_finished());
            drop(previous);
            let writer = change.join().unwrap();
            assert!(gate.read(0x20000..0x30000, false).is_none());
            // A second, unknown writer blocks even the disjoint texture.
            let unknown = gate.announce(ALL);
            assert!(gate.read(0x40000..0x50000, false).is_none());
            drop(writer);
            assert!(gate.read(0x10000..0x20000, false).is_none());
            drop(disjoint);
            unknown.await_readers();
            drop(unknown);
            assert!(gate.read(ALL, true).is_some());
        });
    }
}
