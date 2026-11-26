use super::*;

#[test]
#[ignore = "requires diagnostics, VICTORY_D3D12_WARP=1 and VICTORY_UMA_POOL=1"]
fn warp_raw_shader_replays_across_a_pool_remap() {
    use windows::Win32::Graphics::Direct3D12::*;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
        System::{Memory as mem, Threading::GetCurrentProcess},
    };
    let host = host().unwrap();
    assert!(crate::inplace::uma_pool());
    assert_eq!(unsafe { host.device.GetAdapterLuid() }, unsafe {
        crate::host::warp_adapter()
            .unwrap()
            .GetDesc()
            .unwrap()
            .AdapterLuid
    });
    let shader = r#"
        ByteAddressBuffer directory : register(t0);
        RWByteAddressBuffer inputs[1088] : register(u0);
        RWByteAddressBuffer output : register(u0, space1);
        cbuffer Args : register(b0) { uint page; };
        [RootSignature("SRV(t0),DescriptorTable(UAV(u0,numDescriptors=1088)),UAV(u0,space=1),RootConstants(num32BitConstants=1,b0)")]
        [numthreads(1,1,1)] void main() {
            uint2 e = directory.Load2(8 * page);
            uint value = e.x ? inputs[e.x].Load(e.y + 256) : 0;
            output.Store3(0, uint3(value,e.x,e.y));
        }"#;
    let code = crate::host::compile(
        shader,
        windows::core::s!("main"),
        windows::core::s!("cs_5_1"),
    )
    .unwrap();
    unsafe {
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
        let process = GetCurrentProcess();
        let size = PAGE as usize;
        let base = (8..16usize)
            .map(|tag| {
                mem::VirtualAlloc2(
                    process,
                    (tag << 32) as *const _,
                    size,
                    mem::MEM_RESERVE | mem::MEM_RESERVE_PLACEHOLDER,
                    mem::PAGE_NOACCESS,
                    std::ptr::null_mut(),
                    0,
                )
            })
            .find(|base| !base.is_null())
            .expect("a placeholder in the shader directory");
        let section = mem::CreateFileMappingW(
            INVALID_HANDLE_VALUE,
            std::ptr::null(),
            mem::PAGE_READWRITE | mem::SEC_COMMIT,
            0,
            2 * size as u32,
            std::ptr::null(),
        );
        assert!(!section.is_null());
        crate::title_memory_section(section as usize, 2 * PAGE, true);
        let show = |offset, word: u32| {
            let view = mem::MapViewOfFile3(
                section,
                process,
                base,
                offset,
                size,
                mem::MEM_REPLACE_PLACEHOLDER,
                mem::PAGE_READWRITE,
                std::ptr::null_mut(),
                0,
            );
            assert_eq!(view.Value, base);
            view.Value.byte_add(256).cast::<u32>().write(word);
            crate::title_memory_remapped(base as usize, size);
            crate::title_memory_view(base as usize, size, section as usize, offset);
            view
        };
        let hide = |view| {
            assert_ne!(
                mem::UnmapViewOfFile2(process, view, mem::MEM_PRESERVE_PLACEHOLDER),
                0
            );
            crate::title_memory_remapped(base as usize, size);
        };
        let first = show(0, 314);
        let addr = base as u64;
        crate::shadow::register_buffer(27, addr, PAGE, true);
        let list = host.new_list().unwrap();
        let heap = HostHeap::new(false, 32).unwrap();
        let ready = super::super::ready(&list, &heap, &[addr], &[], false).unwrap();
        let (out, data) = host.shared_buffer(256).unwrap();
        let step = host
            .device
            .GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV);
        let mut table = heap.heap.GetGPUDescriptorHandleForHeapStart();
        table.ptr += u64::from(ready.base) * u64::from(step);
        list.list.SetDescriptorHeaps(&[Some(heap.heap.clone())]);
        list.list.SetComputeRootSignature(&root);
        list.list.SetPipelineState(&pso);
        list.list
            .SetComputeRootShaderResourceView(0, ready.directory);
        list.list.SetComputeRootDescriptorTable(1, table);
        list.list
            .SetComputeRootUnorderedAccessView(2, out.GetGPUVirtualAddress());
        list.list
            .SetComputeRoot32BitConstant(3, (addr / PAGE) as u32, 0);
        list.list.Dispatch(1, 1, 1);
        list.close();
        let execute = || {
            host.execute(&[&list]);
            data.cast::<[u32; 3]>().read()
        };
        let result = execute();
        let copied = std::env::var("VICTORY_UMA_POOL_OPEN_MB").as_deref() == Ok("0");
        assert_eq!(result, [314, if copied { 1 } else { COPIES + 1 }, 0]);
        hide(first);
        let second = show(PAGE, 159);
        assert_eq!(
            execute(),
            [159, result[1], if copied { 0 } else { PAGE as u32 }]
        );
        hide(second);
        assert_eq!(execute(), [0, 0, 0]);
        crate::shadow::unregister(27, addr);
        let _charge = crate::title_memory_section_closed(section as usize);
        assert_ne!(CloseHandle(section), 0);
        assert_ne!(mem::VirtualFree(base, 0, mem::MEM_RELEASE), 0);
    }
}

#[test]
fn remapped_and_missing_slots_restore_the_original_page() {
    let mut directory = [0, 0, 2, 16, 3, 32, 4, 48];
    let mut sections = Sections::default();
    sections.candidates = vec![(2, 0, 65536)];
    let reads = sections.publish(directory.as_mut_ptr(), 1);
    assert_eq!(directory[4..6], [COPIES + 1, 65536]);
    let mut fragments = Vec::new();
    reads.fallback(65540, 3 * 65536 - 9, |at, n| fragments.push((at, n)));
    assert_eq!(fragments, [(65540, 65532), (196608, 65531)]);
    sections.candidates = vec![(2, 1, 0)];
    sections.publish(directory.as_mut_ptr(), 2);
    assert_eq!(directory[4..6], [COPIES + 2, 0]);
    sections.publish(directory.as_mut_ptr(), 1);
    assert_eq!(directory[4..6], [3, 32]);
    sections.candidates.clear();
    sections.holes.push(2);
    sections.publish(directory.as_mut_ptr(), 2);
    assert_eq!(directory[4..6], [0, 0]);
    sections.holes.clear();
    sections.publish(directory.as_mut_ptr(), 2);
    assert_eq!(directory[4..6], [3, 32]);
}
