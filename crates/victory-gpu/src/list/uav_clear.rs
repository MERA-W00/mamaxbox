//! Approach after XCL 1a1a07f: both console handles name records in title
//! memory. D3D12 needs a CPU descriptor and a matching GPU descriptor in
//! the currently bound heap. A queued recording owns the latter until retire.

use super::{Rect, This};
use crate::descriptor::{View, host as views};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use windows::Win32::Graphics::Direct3D12::*;

pub(super) const fn table(newer: bool) -> [*const (); 160] {
    let mut slots = super::list_table(newer);
    slots[53] = clear::<false> as *const ();
    slots[54] = clear::<true> as *const ();
    slots
}

extern "system" fn clear<const FLOAT: bool>(
    this: This,
    gpu: u64,
    cpu: u64,
    resource: This,
    values: *const [u32; 4],
    count: u32,
    rects: *const Rect,
) {
    static DONE: AtomicU64 = AtomicU64::new(0);
    static SKIPPED: AtomicU64 = AtomicU64::new(0);
    let mut recorded = false;
    super::with_state(this, |list, state| {
        let (Some(host), Some(values)) = (crate::host::host(), unsafe { values.as_ref() }) else {
            return;
        };
        if resource.is_null() || count != 0 && rects.is_null() {
            return;
        }
        let mut words = [0; 12];
        if !crate::host::read_into(cpu as *const u8, &mut words) {
            return;
        }
        let Some(view) = crate::descriptor::view(words.as_ptr() as u64) else {
            return;
        };
        match &view {
            View::Unordered(r, _, _) if *r == resource as usize => {}
            View::Placed(true, ..)
                if views::buffer_extent(&view)
                    == crate::resource::buffer_bytes(resource).map(|(p, n)| (p as u64, n))
                    && views::buffer_extent(&view).is_some() => {}
            _ => return,
        }
        let desc = D3D12_DESCRIPTOR_HEAP_DESC {
            Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
            NumDescriptors: 1,
            ..Default::default()
        };
        // SAFETY: one CPU-only slot, consumed when recording the clear.
        let Ok(staging) = (unsafe {
            host.device
                .CreateDescriptorHeap::<ID3D12DescriptorHeap>(&desc)
        }) else {
            return;
        };
        let cpu = unsafe { staging.GetCPUDescriptorHandleForHeapStart() };
        let Some(Some(backing)) = views::make(&view, cpu) else {
            return;
        };
        let buffer = views::buffer_copy(&view);
        if buffer.as_ref().is_some_and(|(s, _, _)| s.buffer != backing) {
            return;
        }
        let Some((copy, gpu)) = state.clear_slot(list, gpu, &view) else {
            return;
        };
        if let Some((shadow, address, len)) = buffer {
            shadow.refresh(list, address, len);
            shadow.gpu_writes(list, address, len);
        } else if !list.twin_use(resource, true, true) {
            super::ensure_state(
                list,
                resource,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS.0 as u32,
            );
        }
        list.keep(&backing);
        // SAFETY: both descriptors describe backing. The recording owns the
        // bound heap and its allocated slot. A zero count permits a null RECT.
        unsafe {
            host.device.CopyDescriptorsSimple(1, copy, cpu, desc.Type);
            let rects = if count == 0 {
                &[]
            } else {
                std::slice::from_raw_parts(rects.cast(), count as usize)
            };
            if FLOAT {
                list.list.ClearUnorderedAccessViewFloat(
                    gpu,
                    cpu,
                    &backing,
                    &values.map(f32::from_bits),
                    rects,
                );
            } else {
                list.list
                    .ClearUnorderedAccessViewUint(gpu, cpu, &backing, values, rects);
            }
        }
        recorded = true;
    });
    let n = if recorded { &DONE } else { &SKIPPED }.fetch_add(1, Relaxed) + 1;
    if n.is_power_of_two() {
        tracing::info!(
            recorded = DONE.load(Relaxed),
            skipped = SKIPPED.load(Relaxed),
            "console UAV clears"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_clear_values_reach_the_selected_texels() {
        use crate::dxgi::planes::tests::{Guest, read_pixels, texture};
        use crate::resource::PAGEABLE_IIDS;
        use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R32_FLOAT, DXGI_FORMAT_R32_UINT};
        let Some(host) = crate::host::host() else {
            eprintln!("no D3D12 device");
            return;
        };
        let null = std::ptr::null();
        let mut a = null;
        assert_eq!(
            super::super::create_allocator(null, 0, &PAGEABLE_IIDS[0], &mut a),
            0
        );
        let allocator = Guest(a);
        let mut l = null;
        assert_eq!(
            super::super::create_list(
                null,
                0,
                0,
                allocator.0,
                null,
                &super::super::LIST_IIDS[0],
                &mut l
            ),
            0
        );
        let list = Guest(l);
        let make_heap = |visible| {
            // Include room for the newer creation layout's optional placement.
            let desc = [0u32, 8, u32::from(visible), 0, 0, 0, 0, 0];
            let mut h = null;
            assert_eq!(
                crate::descriptor::create_heap(
                    null,
                    desc.as_ptr().cast(),
                    &PAGEABLE_IIDS[0],
                    &mut h
                ),
                0
            );
            Guest(h)
        };
        let visible = make_heap(true);
        let staging = make_heap(false);
        super::super::with_state(list.0, |_, state| state.set_heaps(&[visible.0]));
        for (i, format, bits) in [
            (0, DXGI_FORMAT_R32_UINT, 0xdeadbeef),
            (1, DXGI_FORMAT_R32_FLOAT, 0.75f32.to_bits()),
        ] {
            let image = texture(format);
            let slot = |heap: &Guest| crate::descriptor::heap(heap.0).handle(i).unwrap();
            let (gpu, cpu) = (slot(&visible), slot(&staging));
            for handle in [gpu, cpu] {
                crate::descriptor::create_uav(null, image.0, null, std::ptr::null(), handle);
            }
            let values = [bits; 4];
            let rect = Rect {
                left: 1,
                top: 0,
                right: 2,
                bottom: 1,
            };
            if i == 0 {
                clear::<false>(list.0, gpu, cpu, image.0, &[0; 4], 0, std::ptr::null());
                clear::<false>(list.0, gpu, cpu, image.0, &values, 1, &rect);
            } else {
                clear::<true>(list.0, gpu, cpu, image.0, &[0; 4], 0, std::ptr::null());
                clear::<true>(list.0, gpu, cpu, image.0, &values, 1, &rect);
            }
            let host_list = super::super::host_list(list.0).unwrap();
            super::super::ensure_state(
                host_list,
                image.0,
                D3D12_RESOURCE_STATE_COPY_SOURCE.0 as u32,
            );
            let src = crate::resource::host_resource(image.0).unwrap();
            assert_eq!(read_pixels(host, host_list, src, format), [0, bits]);
            assert_eq!(super::super::reset(list.0, allocator.0, null), 0);
            super::super::with_state(list.0, |_, state| state.set_heaps(&[visible.0]));
        }
    }

    #[test]
    fn integer_and_float_clear_have_the_same_slots_in_both_layouts() {
        for newer in [false, true] {
            let old = super::super::list_table(newer);
            let new = table(newer);
            assert_eq!(new[53], clear::<false> as *const ());
            assert_eq!(new[54], clear::<true> as *const ());
            assert_eq!(new.iter().zip(old).filter(|(a, b)| **a != *b).count(), 2);
        }
    }
}
