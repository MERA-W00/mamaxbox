//! Raw addresses keep their existing directory and descriptor numbers.
//! Extra immutable slots name whole physical sections. A read-only list
//! can use a different section and offset on each 64 KiB page. Publication
//! happens under the submission lock after earlier directory readers finish.

use super::*;
use crate::inplace::pool::{self, PoolBuffer};
use std::collections::BTreeSet;

pub(super) const SLOTS: u32 = 64;

#[derive(Default)]
pub(super) struct Sections {
    buffers: Vec<Arc<PoolBuffer>>,
    heaps: HashMap<usize, usize>,
    scanned: Option<(u64, usize)>,
    candidates: Vec<(usize, usize, u32)>,
    mapped_before: BTreeSet<usize>,
    holes: Vec<usize>,
    saved: Vec<(usize, [u32; 2])>,
}

impl Sections {
    pub(super) fn reserve(&mut self, heap: &HostHeap, key: usize) -> Option<u32> {
        if let Some(base) = heap.dynamic(COPIES + 1 + SLOTS) {
            self.heaps.insert(key, 0);
            return Some(base);
        }
        heap.dynamic(COPIES + 1)
    }

    /// A descriptor installed for a section is immutable. A replay can use
    /// only the slots installed in every view heap that its list bound.
    pub(super) fn bind(&mut self, heap: &HostHeap, key: usize, base: u32) -> usize {
        let Some(made) = self.heaps.get_mut(&key) else {
            return 0;
        };
        for (n, section) in self.buffers.iter().enumerate().skip(*made) {
            heap.raw_uav_at(
                base + COPIES + 1 + n as u32,
                &section.buffer,
                0,
                section.bytes,
            );
        }
        *made = self.buffers.len();
        *made
    }

    pub(super) fn scan(
        &mut self,
        device: &windows::Win32::Graphics::Direct3D12::ID3D12Device,
        copies: &[Arc<Shadow>],
        recording: bool,
    ) {
        let stamp = (pool::version(), copies.len());
        if self.scanned == Some(stamp) {
            return;
        }
        self.candidates.clear();
        self.holes.clear();
        let mut seen = BTreeSet::new();
        for shadow in copies {
            let (at, len) = shadow.span();
            let end = (at + len).min((u64::from(MEMORY_PAGES) - 1) * PAGE);
            for page in at.div_ceil(PAGE)..end / PAGE {
                if !seen.insert(page) {
                    continue;
                }
                let open_new = recording && self.buffers.len() < SLOTS as usize;
                let at = (page * PAGE) as usize;
                let Some((buffer, offset)) = pool::lookup(device, at, PAGE as usize, open_new)
                else {
                    if matches!(
                        crate::inplace::cover(at, PAGE as usize),
                        crate::inplace::Cover::Run(..)
                    ) {
                        self.mapped_before.insert(page as usize);
                    } else if self.mapped_before.contains(&(page as usize))
                        && !crate::texture::committed(at as *const u8, PAGE as usize)
                    {
                        self.holes.push(page as usize);
                    }
                    continue;
                };
                self.mapped_before.insert(page as usize);
                let slot = match self.buffers.iter().position(|b| Arc::ptr_eq(b, &buffer)) {
                    Some(n) => n,
                    None if open_new => {
                        self.buffers.push(buffer);
                        self.buffers.len() - 1
                    }
                    None => continue,
                };
                self.candidates.push((page as usize, slot, offset as u32));
            }
        }
        // A remap after recording can point into a slab that this heap
        // cannot name yet. The next recording may install its descriptor.
        self.scanned = recording.then_some(stamp);
    }

    fn publish(&mut self, directory: *mut u32, bound: usize) -> Reads {
        let mut direct = BTreeSet::new();
        // SAFETY: the submitting thread has settled earlier directory
        // readers. The directory mutex excludes concurrent CPU updates.
        unsafe {
            for (page, previous) in self.saved.drain(..) {
                let entry = directory.add(2 * page).cast::<[u32; 2]>();
                let slot = entry.read()[0];
                if slot == 0 || slot > COPIES {
                    entry.write(previous);
                }
            }
            for &page in &self.holes {
                let entry = directory.add(2 * page).cast::<[u32; 2]>();
                let previous = entry.read();
                if previous[0] != 0 {
                    self.saved.push((page, previous));
                    entry.write([0, 0]);
                }
            }
            for &(page, slot, offset) in &self.candidates {
                if slot >= bound {
                    continue;
                }
                let entry = directory.add(2 * page).cast::<[u32; 2]>();
                let previous = entry.read();
                if previous[0] == 0 {
                    continue;
                }
                self.saved.push((page, previous));
                entry.write([COPIES + 1 + slot as u32, offset]);
                direct.insert(page);
            }
        }
        Reads(direct)
    }
}

#[derive(Default)]
pub(crate) struct Reads(BTreeSet<usize>);

impl Reads {
    /// Keep fallback fragments separate across direct pages. Joining their
    /// endpoints would reintroduce watch and copy work in the middle.
    pub(crate) fn fallback(&self, at: usize, bytes: usize, mut read: impl FnMut(usize, usize)) {
        let end = at + bytes;
        let mut first = at;
        if bytes != 0 {
            for &page in self
                .0
                .range(at / PAGE as usize..end.div_ceil(PAGE as usize))
            {
                let base = page * PAGE as usize;
                if first < base {
                    read(first, base - first);
                }
                first = end.min(base + PAGE as usize);
            }
        }
        if first < end {
            read(first, end - first);
        }
    }
}

pub(crate) fn prepare(bound: usize) -> Reads {
    // A remap can remove a whole view before it installs its replacement.
    // Wait outside the directory lock so holes and offsets are from one
    // completed mapping. An opened section keeps that selection alive.
    let _mapping = crate::watch::title_memory_reading();
    let mut guard = MEMORY.lock().unwrap_or_else(|e| e.into_inner());
    let Some(m) = guard.as_mut() else {
        return Reads::default();
    };
    m.sections.scan(&host().unwrap().device, &m.copies, false);
    let reads = m.sections.publish(m.directory.1, bound);
    crate::inplace::pool::note_directory(reads.0.len());
    reads
}

#[cfg(test)]
mod tests;
