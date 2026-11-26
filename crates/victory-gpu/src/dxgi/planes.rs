//! Approach after XCL 2ca749a: PresentX passes an array of 72-byte plane
//! records. Each plane has its own source rectangle. Overlay pixels carry
//! premultiplied alpha, so their RGB contribution must be added once.

use crate::host::{Host, HostList, compile};
use crate::object::This;
use crate::resource::HostTexture;
use std::sync::{Arc, LazyLock, Mutex};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::{
    Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, Direct3D12::*, Dxgi::Common::*,
};
use windows::core::s;

#[repr(C)]
struct Parameters {
    token: u64,
    count: u32,
    reserved: u32,
    resources: *const This,
    crop: *const [i32; 4],
    display: [u64; 5],
}

const _: () = assert!(size_of::<Parameters>() == 0x48);

pub(crate) struct Layer {
    pub id: usize,
    pub copy: Arc<HostTexture>,
    crop: Option<[i32; 4]>,
}

fn plane_resource(p: &Parameters) -> Option<(This, Option<[i32; 4]>)> {
    if p.count == 0 || p.resources.is_null() {
        return None;
    }
    // SAFETY: the resource array and optional crop are valid for this call.
    let resource = unsafe { *p.resources };
    (!resource.is_null()).then(|| (resource, unsafe { p.crop.as_ref() }.copied()))
}

pub(crate) fn snapshot(count: u32, p: *const crate::queue::Plane) -> Option<Vec<Layer>> {
    static ON: LazyLock<bool> =
        LazyLock::new(|| std::env::var_os("VICTORY_PRESENT_PLANES").is_some_and(|v| v == "1"));
    if !*ON || count < 2 || p.is_null() {
        return None;
    }
    // SAFETY: the title supplied count records. No title pointers used by
    // the worker survive this call except id, which is only logged.
    let planes = unsafe { std::slice::from_raw_parts(p.cast::<Parameters>(), count as usize) };
    planes
        .iter()
        .map(|p| {
            let (id, crop) = plane_resource(p)?;
            Some(Layer {
                id: id as usize,
                copy: crate::resource::host_copy(id)?,
                crop,
            })
        })
        .collect()
}

fn uv(crop: Option<[i32; 4]>, w: u32, h: u32) -> [f32; 4] {
    if let Some([x, y, r, b]) = crop
        && x >= 0
        && y >= 0
        && r > x
        && b > y
        && r as u32 <= w
        && b as u32 <= h
    {
        [
            x as f32 / w as f32,
            y as f32 / h as f32,
            (r - x) as f32 / w as f32,
            (b - y) as f32 / h as f32,
        ]
    } else {
        [0., 0., 1., 1.]
    }
}

const SOURCE: &str = r#"
#define RS "RootConstants(num32BitConstants=4,b0),DescriptorTable(SRV(t0)),StaticSampler(s0,filter=FILTER_MIN_MAG_MIP_LINEAR,addressU=TEXTURE_ADDRESS_CLAMP,addressV=TEXTURE_ADDRESS_CLAMP,addressW=TEXTURE_ADDRESS_CLAMP)"
cbuffer Bounds : register(b0) { float4 crop; }
Texture2D<float4> plane : register(t0);
SamplerState linearClamp : register(s0);
struct Vertex { float4 pos : SV_Position; float2 st : TEXCOORD0; };
[RootSignature(RS)] Vertex vs(uint i : SV_VertexID) {
    float2 st = float2(i == 1 ? 2 : 0, i == 2 ? 2 : 0);
    Vertex o;
    o.pos = float4(st.x * 2 - 1, 1 - st.y * 2, 0, 1);
    o.st = crop.xy + st * crop.zw;
    return o;
}
[RootSignature(RS)] float4 ps(Vertex v) : SV_Target {
    return plane.SampleLevel(linearClamp, v.st, 0);
}
"#;

type Pipeline = (ID3D12RootSignature, ID3D12PipelineState);

fn pipeline(host: &Host, format: DXGI_FORMAT) -> Option<Pipeline> {
    static CACHE: Mutex<Vec<(i32, Pipeline)>> = Mutex::new(Vec::new());
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, p)) = cache.iter().find(|(f, _)| *f == format.0) {
        return Some(p.clone());
    }
    let vs = compile(SOURCE, s!("vs"), s!("vs_5_1")).ok()?;
    let ps = compile(SOURCE, s!("ps"), s!("ps_5_1")).ok()?;
    let bytecode = |v: &[u8]| D3D12_SHADER_BYTECODE {
        pShaderBytecode: v.as_ptr().cast(),
        BytecodeLength: v.len(),
    };
    // SAFETY: the description borrows the root and both shader blobs only for creation.
    unsafe {
        let root: ID3D12RootSignature = host.device.CreateRootSignature(0, &ps).ok()?;
        let mut desc = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
            pRootSignature: std::mem::transmute_copy(&root),
            VS: bytecode(&vs),
            PS: bytecode(&ps),
            NumRenderTargets: 1,
            SampleMask: u32::MAX,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
            RasterizerState: D3D12_RASTERIZER_DESC {
                FillMode: D3D12_FILL_MODE_SOLID,
                CullMode: D3D12_CULL_MODE_NONE,
                DepthClipEnable: true.into(),
                ..Default::default()
            },
            ..Default::default()
        };
        desc.RTVFormats[0] = format;
        desc.BlendState.RenderTarget[0] = D3D12_RENDER_TARGET_BLEND_DESC {
            BlendEnable: true.into(),
            SrcBlend: D3D12_BLEND_ONE,
            DestBlend: D3D12_BLEND_INV_SRC_ALPHA,
            BlendOp: D3D12_BLEND_OP_ADD,
            SrcBlendAlpha: D3D12_BLEND_ONE,
            DestBlendAlpha: D3D12_BLEND_INV_SRC_ALPHA,
            BlendOpAlpha: D3D12_BLEND_OP_ADD,
            LogicOp: D3D12_LOGIC_OP_NOOP,
            RenderTargetWriteMask: 15,
            ..Default::default()
        };
        let pso = host.device.CreateGraphicsPipelineState(&desc).ok()?;
        let p = (root, pso);
        cache.push((format.0, p.clone()));
        Some(p)
    }
}

/// Approach after XCL 9251d80: the largest plane sets the output size.
/// A plane-zero fallback copies just its first subresource. Clear any
/// uncovered part of the back buffer so an old frame cannot show through.
pub(crate) fn copy_primary(
    host: &Host,
    list: &ID3D12GraphicsCommandList,
    primary: &ID3D12Resource,
    back: &ID3D12Resource,
) {
    // SAFETY: the caller put the primary in COPY_SOURCE and back in COPY_DEST.
    unsafe {
        let src = primary.GetDesc();
        let dst = back.GetDesc();
        if src.Width < dst.Width || src.Height < dst.Height {
            let rtv = host.rtv(Some(back), None);
            list.ResourceBarrier(&[crate::host::transition(
                back,
                D3D12_RESOURCE_STATE_COPY_DEST,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
            )]);
            list.ClearRenderTargetView(rtv, &[0., 0., 0., 1.], None);
            list.ResourceBarrier(&[crate::host::transition(
                back,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
                D3D12_RESOURCE_STATE_COPY_DEST,
            )]);
        }
        let first = |r: &ID3D12Resource| D3D12_TEXTURE_COPY_LOCATION {
            pResource: std::mem::transmute_copy(r),
            Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                SubresourceIndex: 0,
            },
        };
        let region = D3D12_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: src.Width.min(dst.Width) as u32,
            bottom: src.Height.min(dst.Height),
            back: 1,
        };
        list.CopyTextureRegion(&first(back), 0, 0, 0, &first(primary), Some(&region));
    }
}

/// Returns before recording any commands when a layer cannot be sampled.
/// The caller then uses its normal plane-zero copy.
pub(crate) fn record(
    host: &Host,
    list: &HostList,
    back: &ID3D12Resource,
    layers: &[Layer],
    format: DXGI_FORMAT,
) -> bool {
    let Some((root, pipeline)) = pipeline(host, format) else {
        return false;
    };
    let mut inputs = Vec::with_capacity(layers.len());
    for layer in layers {
        let desc = unsafe { layer.copy.texture.GetDesc() };
        if desc.Dimension != D3D12_RESOURCE_DIMENSION_TEXTURE2D
            || desc.SampleDesc.Count != 1
            || desc.DepthOrArraySize != 1
            || desc
                .Flags
                .contains(D3D12_RESOURCE_FLAG_DENY_SHADER_RESOURCE)
        {
            return false;
        }
        // Scan-out planes use normalized colour or float. Never create a
        // view that changes the type of a fully typed resource.
        let srv = match desc.Format.0 {
            27 | 28 => DXGI_FORMAT_R8G8B8A8_UNORM,
            87 | 90 => DXGI_FORMAT_B8G8R8A8_UNORM,
            23 | 24 => DXGI_FORMAT_R10G10B10A2_UNORM,
            9 | 10 => DXGI_FORMAT_R16G16B16A16_FLOAT,
            29 | 91 => desc.Format,
            _ => return false,
        };
        inputs.push((srv, uv(layer.crop, desc.Width as u32, desc.Height)));
    }
    let description = D3D12_DESCRIPTOR_HEAP_DESC {
        Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
        NumDescriptors: layers.len() as u32,
        Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
        NodeMask: 0,
    };
    // SAFETY: a separate heap belongs to this recording. settle retains the
    // sources and resolves their states under the submission lock.
    unsafe {
        let Ok(heap) = host
            .device
            .CreateDescriptorHeap::<ID3D12DescriptorHeap>(&description)
        else {
            return false;
        };
        let step = host
            .device
            .GetDescriptorHandleIncrementSize(description.Type);
        let mut cpu = heap.GetCPUDescriptorHandleForHeapStart();
        let mut gpu = heap.GetGPUDescriptorHandleForHeapStart();
        let target = host.rtv(Some(back), None);
        let size = back.GetDesc();
        let l = &list.list;
        l.ResourceBarrier(&[crate::host::transition(
            back,
            D3D12_RESOURCE_STATE_PRESENT,
            D3D12_RESOURCE_STATE_RENDER_TARGET,
        )]);
        l.ClearRenderTargetView(target, &[0.; 4], None);
        l.OMSetRenderTargets(1, Some(&target), false, None);
        l.SetDescriptorHeaps(&[Some(heap.clone())]);
        list.keep(&heap);
        l.SetGraphicsRootSignature(&root);
        l.SetPipelineState(&pipeline);
        l.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        l.RSSetViewports(&[D3D12_VIEWPORT {
            Width: size.Width as f32,
            Height: size.Height as f32,
            MaxDepth: 1.,
            ..Default::default()
        }]);
        l.RSSetScissorRects(&[RECT {
            right: size.Width as i32,
            bottom: size.Height as i32,
            ..Default::default()
        }]);
        for (layer, (format, crop)) in layers.iter().zip(inputs) {
            list.settle(
                &layer.copy,
                D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE.0 as u32,
            );
            let view = D3D12_SHADER_RESOURCE_VIEW_DESC {
                Format: format,
                ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
                Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
                Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                    Texture2D: D3D12_TEX2D_SRV {
                        MipLevels: 1,
                        ..Default::default()
                    },
                },
            };
            host.device
                .CreateShaderResourceView(&layer.copy.texture, Some(&view), cpu);
            l.SetGraphicsRoot32BitConstants(0, 4, crop.as_ptr().cast(), 0);
            l.SetGraphicsRootDescriptorTable(1, gpu);
            l.DrawInstanced(3, 1, 0, 0);
            cpu.ptr += step as usize;
            gpu.ptr += u64::from(step);
        }
        // The existing capture and black-frame probe need a copy source.
        l.ResourceBarrier(&[crate::host::transition(
            back,
            D3D12_RESOURCE_STATE_RENDER_TARGET,
            D3D12_RESOURCE_STATE_COPY_SOURCE,
        )]);
    }
    true
}

#[cfg(test)]
pub(crate) mod tests;
