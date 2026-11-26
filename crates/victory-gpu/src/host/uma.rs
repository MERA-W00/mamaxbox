use super::*;

pub(super) fn prepare_directory(
    host: &Host,
    submitted: &mut u64,
    runs: &[&HostList],
    bound: usize,
) -> crate::memory::pool::Reads {
    #[cfg(feature = "diagnostics")]
    let begin = std::time::Instant::now();
    host.settle(submitted, WHY_READ);
    #[cfg(feature = "diagnostics")]
    let settled = std::time::Instant::now();
    let writes = runs.iter().any(|list| {
        list.deferred()
            .copies
            .iter()
            .any(|step| matches!(step, Step::GpuWrite(..) | Step::Run(_) | Step::Copy(..)))
    });
    let reads = crate::memory::pool::prepare(if writes { 0 } else { bound });
    #[cfg(feature = "diagnostics")]
    crate::inplace::pool::note_preparation(settled - begin, settled.elapsed());
    reads
}

/// A detached recording owns its dynamic constant slots until its fence.
/// Resolve a pool offset here because the title can remap the address
/// after recording. A borrowed recording keeps its replayable upload slot.
pub(super) fn bind_constants(
    device: &ID3D12Device,
    owner: &crate::list::Detached,
    runs: &[&HostList],
    fills: &mut std::collections::HashSet<usize>,
) {
    if !owner.whole_recording
        || flying()
        || !runs.first().is_some_and(|&r| std::ptr::eq(r, owner.host()))
        || runs
            .iter()
            .filter(|&&r| std::ptr::eq(r, owner.host()))
            .count()
            != 1
    {
        return;
    }
    let Some(_mapping) = crate::watch::title_memory_reading_now() else {
        return;
    };
    // A write through another view of this section can have a different
    // virtual address. Keep the shadow for submissions with GPU writers.
    if runs.iter().any(|l| {
        l.deferred()
            .copies
            .iter()
            .any(|s| matches!(s, Step::GpuWrite(..) | Step::Run(_) | Step::Copy(..)))
    }) {
        return;
    }
    let mut recording = owner.host().deferred();
    if recording.submitted {
        return;
    }
    let mut keep = Vec::new();
    let mut private = std::collections::BTreeMap::new();
    for step in &recording.copies {
        let &Step::Fill(dst, src, bytes, Some(slot)) = step else {
            continue;
        };
        if !(1..=65536).contains(&bytes) || src % 256 != 0 || fills.contains(&dst) {
            continue;
        }
        let size = bytes.next_multiple_of(256);
        let section = crate::inplace::pool::constants()
            .then(|| crate::inplace::pool::resolve(device, src, size))
            .flatten();
        let location = if let Some((section, offset)) = section {
            let location = section.gpu + offset;
            keep.push(section);
            crate::inplace::pool::note_constant(bytes);
            location
        } else if crate::inplace::private_constants() {
            let start = src & !65535;
            let Some(end) = src
                .checked_add(size)
                .and_then(|e| e.checked_next_multiple_of(65536))
            else {
                continue;
            };
            let entry = private.entry((start, end)).or_insert_with(|| {
                let placed =
                    crate::inplace::place_for(start as u64, (end - start) as u64, true, true)?;
                Some(std::sync::Arc::new(crate::shadow::Shadow::placed(
                    placed,
                    start as u64,
                    (end - start) as u64,
                    true,
                )))
            });
            let Some(shadow) = entry else { continue };
            crate::inplace::skipped(1, bytes as u64);
            shadow.gpu(src as u64)
        } else {
            continue;
        };
        let desc = D3D12_CONSTANT_BUFFER_VIEW_DESC {
            BufferLocation: location,
            SizeInBytes: size as u32,
        };
        // SAFETY: the section covers the rounded CBV and the recording
        // retains it. Only this detached list can use the fresh slot.
        unsafe { device.CreateConstantBufferView(Some(&desc), slot) };
        fills.insert(dst);
    }
    recording.pool_constants = keep;
    recording.private_constants = private.into_values().flatten().collect();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires diagnostics, WARP and the UMA pool"]
    fn warp_detached_constant_keeps_the_selected_physical_offset() {
        detached_constant(false);
    }

    #[test]
    #[ignore = "requires diagnostics, WARP and private constant views"]
    fn warp_detached_constant_reads_a_private_page() {
        detached_constant(true);
    }

    fn detached_constant(private: bool) {
        use windows_sys::Win32::{
            Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
            System::Memory as mem,
        };
        let host = host().unwrap();
        assert!(if private {
            crate::inplace::private_constants()
        } else {
            crate::inplace::pool::constants()
        });
        unsafe {
            assert_eq!(
                host.device.GetAdapterLuid(),
                warp_adapter().unwrap().GetDesc().unwrap().AdapterLuid
            );
            let page = 65536;
            let section = mem::CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                std::ptr::null(),
                mem::PAGE_READWRITE | mem::SEC_COMMIT,
                0,
                2 * page,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            // Keep a separate writable view to distinguish the two physical pages.
            let data = mem::MapViewOfFile(section, mem::FILE_MAP_WRITE, 0, 0, 0);
            assert!(!data.Value.is_null());
            data.Value.cast::<u32>().write(17);
            data.Value.byte_add(page as usize).cast::<u32>().write(31);
            let guest = mem::MapViewOfFile(section, mem::FILE_MAP_READ, 0, page, page as usize);
            assert!(!guest.Value.is_null());
            crate::title_memory_section(section as usize, u64::from(page) * 2, true);
            crate::title_memory_view(
                guest.Value as usize,
                page as usize,
                section as usize,
                u64::from(page),
            );
            let list = host.new_list().unwrap();
            let (scratch, fallback) = list.scratch(256).unwrap();
            let heap: ID3D12DescriptorHeap = host
                .device
                .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                    Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                    NumDescriptors: 1,
                    Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                    NodeMask: 0,
                })
                .unwrap();
            let slot = heap.GetCPUDescriptorHandleForHeapStart();
            host.device.CreateConstantBufferView(
                Some(&D3D12_CONSTANT_BUFFER_VIEW_DESC {
                    BufferLocation: scratch,
                    SizeInBytes: 256,
                }),
                slot,
            );
            let private_base = if private {
                let base = mem::VirtualAlloc(
                    std::ptr::null(),
                    page as usize,
                    mem::MEM_COMMIT | mem::MEM_RESERVE,
                    mem::PAGE_READWRITE,
                );
                assert!(!base.is_null());
                base.cast::<u32>().write(71);
                base
            } else {
                std::ptr::null_mut()
            };
            let src = if private { private_base } else { guest.Value };
            list.defer_constant_fill(fallback, src.cast(), 256, slot);
            let code = compile(
                r#"
                cbuffer C : register(b0) { uint word; };
                RWByteAddressBuffer result : register(u0);
                [RootSignature("DescriptorTable(CBV(b0)),UAV(u0)")]
                [numthreads(1,1,1)] void main() { result.Store(0,word); }
            "#,
                windows::core::s!("main"),
                windows::core::s!("cs_5_1"),
            )
            .unwrap();
            let root: ID3D12RootSignature = host.device.CreateRootSignature(0, &code).unwrap();
            let pso: ID3D12PipelineState = host
                .device
                .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                    pRootSignature: std::mem::transmute_copy(&root),
                    CS: D3D12_SHADER_BYTECODE {
                        pShaderBytecode: code.as_ptr().cast(),
                        BytecodeLength: code.len(),
                    },
                    ..Default::default()
                })
                .unwrap();
            let (out, result) = host.shared_buffer(256).unwrap();
            list.list.SetDescriptorHeaps(&[Some(heap.clone())]);
            list.list.SetComputeRootSignature(&root);
            list.list.SetPipelineState(&pso);
            list.list
                .SetComputeRootDescriptorTable(0, heap.GetGPUDescriptorHandleForHeapStart());
            list.list
                .SetComputeRootUnorderedAccessView(1, out.GetGPUVirtualAddress());
            list.list.Dispatch(1, 1, 1);
            list.close();
            let owner = crate::list::Detached::for_test(list);
            let mut selected = Default::default();
            bind_constants(&host.device, &owner, &[owner.host()], &mut selected);
            assert!(selected.contains(&(fallback as usize)));
            assert_eq!(
                owner.host().deferred().pool_constants.len(),
                usize::from(!private)
            );
            assert_eq!(
                owner.host().deferred().private_constants.len(),
                usize::from(private)
            );
            // Poison the upload and remove its fill. The selected descriptor
            // must now read the second physical page on its own.
            fallback.cast::<u32>().write(12345);
            owner.host().deferred().copies.clear();
            crate::title_memory_remapped(guest.Value as usize, page as usize);
            assert_ne!(mem::UnmapViewOfFile(guest), 0);
            host.execute(&[owner.host()]);
            assert_eq!(result.cast::<u32>().read(), if private { 71 } else { 31 });
            drop(owner);
            if private {
                assert_ne!(mem::VirtualFree(private_base, 0, mem::MEM_RELEASE), 0);
            }
            let _charge = crate::title_memory_section_closed(section as usize);
            assert_ne!(mem::UnmapViewOfFile(data), 0);
            assert_ne!(CloseHandle(section), 0);
        }
    }

    /// Execute the same recording twice. Each copy must see the last CPU
    /// store, even though the recording contains no deferred refresh.
    #[test]
    #[ignore = "requires diagnostics, VICTORY_D3D12_WARP=1 and VICTORY_UMA=1"]
    fn warp_private_read_replays_without_a_refresh() {
        use windows_sys::Win32::System::Memory as mem;
        let host = host().expect("software adapter");
        assert!(
            host.adapter
                .as_ref()
                .and_then(|a| unsafe { a.GetDesc1() }.ok())
                .is_some_and(|d| d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0)
        );
        let page = 65536;
        // SAFETY: the test owns this reservation until the list and heap are dropped.
        let base = unsafe {
            mem::VirtualAlloc(
                std::ptr::null(),
                page,
                mem::MEM_COMMIT | mem::MEM_RESERVE,
                mem::PAGE_READWRITE,
            )
        };
        assert!(!base.is_null());
        let at = base as u64 + 256;
        crate::shadow::register_buffer(17, at, 512, true);
        let source = crate::shadow::find(at, 512).unwrap();
        assert!(source.direct_read());
        // A later raw store can name the entire untyped page. Only the
        // declared small read-only buffer gets a permanent private binding.
        assert!(
            !crate::shadow::find(base as u64, page as u64)
                .unwrap()
                .direct_read()
        );
        let weak = std::sync::Arc::downgrade(&source);
        let list = host.new_list().unwrap();
        source.refresh(&list, at, 16);
        source.refresh(&list, at + 32, 16);
        assert_eq!(list.deferred().direct.len(), 1);
        assert!(list.deferred().copies.is_empty());
        let readers = [&list];
        let fills = Default::default();
        assert_eq!(
            blocked_by(&[(at as usize, at as usize + 4)], &readers, &fills, false),
            Some(WHY_READ)
        );
        assert_eq!(
            blocked_by(
                &[(at as usize + 48, at as usize + 80)],
                &readers,
                &fills,
                false
            ),
            None
        );
        let (dst, data) = host.shared_buffer(16).unwrap();
        // SAFETY: the list holds the source and dst outlives both executions.
        unsafe {
            list.list
                .CopyBufferRegion(&dst, 0, &source.buffer, source.offset(at), 16)
        };
        list.close();
        drop(source);
        crate::shadow::unregister(17, at);
        assert!(weak.upgrade().is_some());
        for value in [73, 109] {
            unsafe { std::ptr::write_bytes(at as *mut u8, value, 16) };
            host.execute(&[&list]);
            // The synchronous queue entry point has waited for this copy.
            assert_eq!(unsafe { data.cast::<[u8; 16]>().read() }, [value; 16]);
        }
        drop(list);
        assert!(weak.upgrade().is_none());
        assert_ne!(unsafe { mem::VirtualFree(base, 0, mem::MEM_RELEASE) }, 0);
    }
}
