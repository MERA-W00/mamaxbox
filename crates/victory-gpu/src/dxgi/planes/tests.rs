use super::*;

pub(crate) struct Guest(pub This);

impl Drop for Guest {
    fn drop(&mut self) {
        let release = crate::object::slot::<extern "system" fn(This) -> u32>(self.0, 0x10);
        release(self.0);
    }
}

pub(crate) fn texture(format: DXGI_FORMAT) -> Guest {
    sized_texture(format, 2)
}

fn sized_texture(format: DXGI_FORMAT, width: u64) -> Guest {
    let heap = D3D12_HEAP_PROPERTIES {
        Type: D3D12_HEAP_TYPE_DEFAULT,
        ..Default::default()
    };
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        Width: width,
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET | D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
        ..Default::default()
    };
    let mut resource = std::ptr::null();
    assert_eq!(
        crate::resource::create_committed(
            std::ptr::null(),
            std::ptr::from_ref(&heap).cast(),
            0,
            std::ptr::from_ref(&desc).cast(),
            0,
            std::ptr::null(),
            &crate::resource::PAGEABLE_IIDS[0],
            &mut resource
        ),
        0
    );
    Guest(resource)
}

pub(crate) fn read_pixels(
    host: &Host,
    list: &HostList,
    src: &ID3D12Resource,
    format: DXGI_FORMAT,
) -> [u32; 2] {
    let buffer = host.read_back_buffer(256).unwrap();
    // SAFETY: the 256-byte row pitch covers both texels. Synchronous
    // execution completes the GPU copy before the mapping is read.
    unsafe {
        let dst = D3D12_TEXTURE_COPY_LOCATION {
            pResource: std::mem::transmute_copy(&buffer),
            Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                    Offset: 0,
                    Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                        Format: format,
                        Width: 2,
                        Height: 1,
                        Depth: 1,
                        RowPitch: 256,
                    },
                },
            },
        };
        let from = D3D12_TEXTURE_COPY_LOCATION {
            pResource: std::mem::transmute_copy(src),
            Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                SubresourceIndex: 0,
            },
        };
        list.list.CopyTextureRegion(&dst, 0, 0, 0, &from, None);
        list.close();
        host.execute(&[list]);
        let mut bytes = std::ptr::null_mut();
        buffer.Map(0, None, Some(&mut bytes)).unwrap();
        let pixels = bytes.cast::<[u32; 2]>().read_unaligned();
        buffer.Unmap(0, None);
        pixels
    }
}

fn fill(host: &Host, list: &HostList, image: &Arc<HostTexture>, colour: [f32; 4]) {
    list.settle(image, D3D12_RESOURCE_STATE_RENDER_TARGET.0 as u32);
    let rtv = host.rtv(Some(&image.texture), None);
    unsafe { list.list.ClearRenderTargetView(rtv, &colour, None) };
}

#[test]
fn half_alpha_overlay_is_added_once_to_the_scene() {
    let Some(host) = crate::host::host() else {
        return;
    };
    let format = DXGI_FORMAT_R8G8B8A8_UNORM;
    let scene = texture(format);
    let overlay = texture(format);
    let output = texture(format);
    let layers = [scene.0, overlay.0].map(|id| Layer {
        id: id as usize,
        copy: crate::resource::host_copy(id).unwrap(),
        crop: None,
    });
    let list = host.new_list().unwrap();
    fill(host, &list, &layers[0].copy, [0., 0., 1., 1.]);
    fill(host, &list, &layers[1].copy, [0.5, 0., 0., 0.5]);
    let back = crate::resource::host_copy(output.0).unwrap();
    list.settle(&back, D3D12_RESOURCE_STATE_PRESENT.0 as u32);
    assert!(record(host, &list, &back.texture, &layers, format));
    for pixel in read_pixels(host, &list, &back.texture, format) {
        let [red, green, blue, alpha] = pixel.to_le_bytes();
        // UNORM conversion can round a half step either way.
        assert!(red.abs_diff(128) <= 1 && blue.abs_diff(128) <= 1);
        assert_eq!((green, alpha), (0, 255));
    }
}

#[test]
fn fallback_copies_the_primary_and_clears_the_uncovered_pixel() {
    let Some(host) = crate::host::host() else {
        return;
    };
    let format = DXGI_FORMAT_R8G8B8A8_UNORM;
    let source = sized_texture(format, 1);
    let output = texture(format);
    let src = crate::resource::host_copy(source.0).unwrap();
    let back = crate::resource::host_copy(output.0).unwrap();
    let list = host.new_list().unwrap();
    fill(host, &list, &src, [1., 0., 0., 1.]);
    fill(host, &list, &back, [0., 1., 0., 1.]);
    list.settle(&src, D3D12_RESOURCE_STATE_COPY_SOURCE.0 as u32);
    list.settle(&back, D3D12_RESOURCE_STATE_COPY_DEST.0 as u32);
    copy_primary(host, &list.list, &src.texture, &back.texture);
    list.settle(&back, D3D12_RESOURCE_STATE_COPY_SOURCE.0 as u32);
    assert_eq!(
        read_pixels(host, &list, &back.texture, format),
        [0xff0000ff, 0xff000000]
    );
}

#[test]
fn two_plane_records_keep_their_resource_and_crop() {
    let ids = [std::ptr::dangling(), 2usize as This];
    let crop = [2, 1, 6, 3];
    let planes = std::array::from_fn::<_, 2, _>(|i| Parameters {
        token: 0,
        count: 1,
        reserved: 0,
        resources: &ids[i],
        crop: if i == 1 { &crop } else { std::ptr::null() },
        display: [0; 5],
    });
    assert_eq!(plane_resource(&planes[0]), Some((ids[0], None)));
    assert_eq!(plane_resource(&planes[1]), Some((ids[1], Some(crop))));
    assert_eq!(uv(Some(crop), 8, 4), [0.25, 0.25, 0.5, 0.5]);
    assert_eq!(uv(Some(crop), 4, 4), [0., 0., 1., 1.]);
}
