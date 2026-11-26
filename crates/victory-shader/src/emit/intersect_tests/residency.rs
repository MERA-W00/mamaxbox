//! Exercise the title-memory directory, heap copies and fault UAV together.
//! The wide node straddles a page boundary. Its two halves can be mapped
//! independently, including across a carry out of the low address dword.

use super::*;
use crate::emit::lower::intersect::RESIDENT_BOXES;
use crate::emit::{MEMORY_FAULTS, SLOT_FAULTS, SLOT_MEMORY, USER_MEMORY_BASE};

const PAGE: u64 = 1 << 16;
const BASE: u64 = (9 << 32) - PAGE;

#[test]
fn box_children_require_the_pages_containing_their_pointer_and_bounds() {
    let _one = crate::emit::valu_tests::ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut devices = 0;
    for warp in [false, true] {
        let Ok(host) = Host::new(warp) else { continue };
        for wide in [false, true] {
            for enabled in [false, true] {
                check_pages(&host, wide, enabled);
            }
        }
        devices += 1;
    }
    assert!(devices > 0, "requires a compute device");
}

fn check_pages(host: &Host, wide: bool, enabled: bool) {
    let descriptor = if wide {
        WHOLE
    } else {
        Descriptor::Numbers((BASE >> 8) as u32, (BASE >> 40) as u32 | bvh::BOX_SORT)
    };
    let previous = RESIDENT_BOXES.replace(enabled);
    let built = build(wide, descriptor);
    RESIDENT_BOXES.set(previous);
    let (shader, _) = built.unwrap();
    let (root, pso) = host.compute_pipeline(&shader).unwrap();
    assert_eq!(shader.root_uavs, [SLOT_FAULTS]);
    assert_eq!(shader.root_srvs, [SLOT_MEMORY]);
    assert!(shader.root_cbvs.is_empty());
    let mut constants = vec![0; shader.user_data_dwords() as usize];
    let base = shader
        .user_data
        .iter()
        .find(|u| u.root == RootType::Other(USER_MEMORY_BASE))
        .unwrap();
    constants[base.index as usize] = 2;

    let ray = Ray::new([0.0; 3], [1.0, 0.0, 0.0], 10.0);
    let ids = [0, 16, 32, 48];
    let box32 = nodes::box32(
        ids,
        [
            [3.0, -1.0, -1.0, 4.0, 1.0, 1.0],
            [1.0, -1.0, -1.0, 2.0, 1.0, 1.0],
            [5.0, -1.0, -1.0, 6.0, 1.0, 1.0],
            [0.0, 3.0, 3.0, 1.0, 4.0, 4.0],
        ],
    );
    // The compact node has four genuine point boxes at the origin. One
    // points at node zero, just like the bytes read from a missing page.
    let mut head = vec![0; (PAGE / 4) as usize];
    head[..4].copy_from_slice(&ids);
    let triangle = nodes::triangles(&[[2.0, -1.0, -1.0], [2.0, 1.0, -1.0], [2.0, 0.0, 1.0]], 9);
    head[32..48].copy_from_slice(&triangle[..16]); // child 16, byte 128
    head[(PAGE as usize - 64) / 4..].copy_from_slice(&box32[..16]);
    let mut tail = vec![0x7fc0_0000; 32];
    tail[16..28].copy_from_slice(&box32[16..]);

    let first_page = (BASE / PAGE) as usize;
    let mut directory = vec![0; 2 * (first_page + 9)];
    let mut records = vec![0; (THREADS * RECORD / 4) as usize];
    let mut expected = [BEFORE; THREADS as usize];
    let mut missing = Vec::new();
    let none = bvh::NONE;
    for (pair, (has_head, has_tail)) in [(true, true), (true, false), (false, true), (false, false)]
        .into_iter()
        .enumerate()
    {
        let page = first_page + 2 * pair;
        if has_head {
            directory[2 * page] = 1;
        } else {
            missing.push(page);
        }
        if has_tail {
            directory[2 * (page + 1)] = 2;
            directory[2 * (page + 1) + 1] = 64;
        } else {
            missing.push(page + 1);
        }
        let page_base = if wide {
            page as u64 * PAGE
        } else {
            (2 * pair) as u64 * PAGE
        };
        let lane = 2 * pair;
        ray_record(&mut records, lane, ((page_base + PAGE - 64) >> 3) | 5, &ray);
        ray_record(&mut records, lane + 1, (page_base >> 3) | 4, &ray);
        expected[lane] = match (has_head, has_tail, enabled) {
            (true, true, _) => [16, 0, 32, none],
            (true, false, true) => [16, 0, none, none],
            (true, false, false) => [32, 48, 16, 0],
            (false, _, true) => [none; 4],
            (false, true, false) => [0, 0, 0, none],
            (false, false, false) => [0; 4],
        };
        expected[lane + 1] = if has_head {
            ids
        } else if enabled {
            [none; 4]
        } else {
            [0; 4]
        };
    }
    // The masked lane points at a different missing page. It must neither
    // report a fault there nor overwrite its old result registers.
    let masked = if wide { BASE + 8 * PAGE } else { 8 * PAGE };
    ray_record(&mut records, 8, (masked >> 3) | 4, &ray);
    records[8 * RECORD as usize / 4 + 12] = 0;
    let dispatch = |records: &[u32]| {
        host.dispatch(
            &root,
            &pso,
            &Dispatch {
                // Slot 253 sorts before 254 in the test host's root signature.
                roots: vec![
                    (0, Root::Constants(constants.clone())),
                    (1, Root::Uav(4)),
                    (
                        2,
                        Root::View {
                            srv: true,
                            data: directory.clone(),
                        },
                    ),
                ],
                views: &[
                    View::Buffer {
                        data: records,
                        stride: Stride::Raw,
                    },
                    View::RawUav(4 * THREADS),
                    View::RawUav(1), // reads zero, as does the production null view
                    View::RawUavData(&head),
                    View::RawUavData(&tail),
                    View::RawUav(MEMORY_FAULTS),
                ],
                samplers: &[],
                groups: [1, 1, 1],
            },
        )
        .unwrap()
    };
    let out = dispatch(&records);
    assert_eq!(
        out[0],
        expected.as_flattened(),
        "wide={wide} resident boxes={enabled}"
    );
    let mut faults = vec![0; MEMORY_FAULTS as usize];
    for page in missing {
        faults[1 + page % (MEMORY_FAULTS as usize - 1)] = page as u32;
    }
    // Slot zero is the sink for resident and inactive lanes.
    assert_eq!(&out[4][1..], &faults[1..]);

    // Follow the first child returned for the split root whose tail is
    // missing. Both node tests run on the device. Its real triangle lies
    // two units along the ray, a quarter and a half along its two edges.
    let child = u64::from(out[0][2 * 4]);
    let pointer = if wide { (BASE >> 3) + child } else { child };
    records.fill(0);
    ray_record(&mut records, 0, pointer, &ray);
    let leaf = dispatch(&records);
    if enabled {
        let [t, det, i, j] = std::array::from_fn(|k| f32::from_bits(leaf[0][k]));
        assert_eq!([t / det, i / det, j / det], [2.0, 0.25, 0.5]);
    } else {
        // The zero bounds ranked child 32 first. Its zero vertices miss.
        assert_eq!(leaf[0][..4], bvh::MISS.map(f32::to_bits));
    }
}
