use super::*;

fn state(at: usize) -> u32 {
    let mut info = MEMORY_BASIC_INFORMATION::default();
    assert_ne!(
        unsafe { VirtualQuery(at as _, &mut info, size_of_val(&info)) },
        0
    );
    info.State
}

fn reserve(pages: usize) -> usize {
    let at = XMemVirtualAlloc(
        std::ptr::null_mut(),
        pages * PAGE,
        MEM_RESERVE | PAGES_64K,
        0,
        PAGE_READWRITE,
    ) as usize;
    assert_ne!(at, 0);
    at
}

/// Exercise the public queue mapper in a fresh process so the switch and
/// per-second counters are independent of the other HLE tests.
#[test]
fn streamed_aliases_keep_their_pool_bytes() {
    if std::env::var_os("VICTORY_LAZY_REMAP").is_none_or(|v| v != "1") {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "modules::xmem::lazy::tests::streamed_aliases_keep_their_pool_bytes",
                "--nocapture",
            ])
            .env("VICTORY_LAZY_REMAP", "1")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    assert!(enabled());
    let (pool, guest) = (reserve(4), reserve(4));
    let mut pages = [0; 4];
    let mut count = pages.len();
    assert_ne!(
        XMemAllocatePhysicalPages(0, &mut count, pages.as_mut_ptr()),
        0
    );
    assert_eq!(
        XMemMapPhysicalPages(pool as _, count, pages.as_ptr()) as usize,
        pool
    );
    let alias = |at: usize, from: usize, bytes: usize| {
        super::super::map_alias(at as u64, from as u64, bytes as u64)
    };
    let texels: Vec<[u8; 1024]> = (0..4u8)
        .map(|p| std::array::from_fn(|i| (i as u8).wrapping_mul(37).wrapping_add(51 * p)))
        .collect();
    for (i, bytes) in texels.iter().enumerate() {
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), (pool + i * PAGE) as _, bytes.len())
        };
    }
    // Every request changes the page. Verify each upload, including the
    // first one, and count actual section operations on this thread.
    let mut upload = [0; 1024];
    for i in 0..4096 {
        let target = guest + (i % 3) * PAGE;
        let source = i % 4;
        KERNEL.set(Some(0));
        assert!(alias(target, pool + source * PAGE, PAGE));
        assert_eq!(KERNEL.replace(None), Some(0));
        assert!(victory_gpu::remap::read_into(target as _, &mut upload));
        assert_eq!(upload, texels[source]);
    }
    for i in 0..3 {
        assert_eq!(state(guest + i * PAGE), MEM_RESERVE);
    }

    // An actual tiled image with different values in every channel.
    use victory_formats::texture::{Desc, Dimension, Layout, Swizzle};
    let layout = Layout::new(&Desc {
        dimension: Dimension::Tex2d,
        width: 16,
        height: 16,
        depth: 1,
        mips: 1,
        element_bytes: 4,
        block: 1,
        samples: 1,
        swizzle: Swizzle::SW_4KB_S,
    })
    .unwrap();
    let mut tiled = vec![0; layout.size as usize];
    layout.tile(&mut tiled, 0, 0, &texels[2], 64, 1024).unwrap();
    assert!(write_physical(&pages[2..3], PAGE / 2, &tiled));
    assert!(alias(guest, pool + 2 * PAGE, PAGE));
    assert!(victory_gpu::remap::read_into(
        (guest + PAGE / 2) as _,
        &mut tiled
    ));
    layout.detile(&tiled, 0, 0, &mut upload, 64, 1024).unwrap();
    assert_eq!(upload, texels[2]);
    assert_eq!(state(guest), MEM_RESERVE);

    assert!(alias(guest, pool, 3 * PAGE));
    let middle = guest + PAGE;
    // Simultaneous faults on a nonzero slice of a reserved section.
    let gate = std::sync::Barrier::new(8);
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                gate.wait();
                assert_eq!(unsafe { (middle as *const u8).read_volatile() }, 51);
            });
        }
    });
    assert_eq!(state(guest), MEM_RESERVE);
    assert_eq!(state(middle), MEM_COMMIT);
    assert_eq!(state(middle + PAGE), MEM_RESERVE);
    // The CPU's VM APIs cannot free a live section view. Querying it
    // returns the same mapped allocation a CPU load would expose.
    assert_eq!(
        super::super::virtual_memory::VirtualFree(guest as _, 0, MEM_RELEASE),
        0
    );
    let mut mbi = MEMORY_BASIC_INFORMATION::default();
    let process = unsafe { GetCurrentProcess() };
    assert_ne!(
        super::super::virtual_memory::query(process, guest as _, &mut mbi, size_of_val(&mbi)),
        0
    );
    assert_eq!((mbi.State, mbi.RegionSize), (MEM_COMMIT, PAGE));

    // Copying the alias makes another section reference. Neither remapping
    // its source nor releasing the physical number changes that reference.
    let second = guest + 3 * PAGE;
    assert!(alias(second, middle, PAGE));
    assert!(alias(middle, pool + 3 * PAGE, PAGE));
    assert_eq!(state(middle), MEM_RESERVE);
    assert_eq!(unsafe { (second as *const u8).read_volatile() }, 51);
    crate::modules::dstorage::tests::write_alias(middle + 13, &[5, 9, 4, 1]);
    assert_eq!(
        unsafe { ((pool + 3 * PAGE + 13) as *const [u8; 4]).read() },
        [5, 9, 4, 1]
    );

    // A hole between two populated neighbours, followed by a replacement
    // in a different order. Check the bytes across both page boundaries.
    assert!(alias(middle, 0, PAGE));
    assert_eq!(state(middle), MEM_RESERVE);
    assert!(!victory_gpu::remap::read_into(middle as _, &mut upload));
    assert!(victory_gpu::remap::read_into(guest as _, &mut upload));
    assert_eq!(upload, texels[0]);
    assert!(victory_gpu::remap::read_into(
        (middle + PAGE) as _,
        &mut upload
    ));
    assert_eq!(upload, texels[2]);
    assert!(alias(middle, pool + PAGE, PAGE));
    let mut seam = [0; 5];
    for (at, expected) in [(middle - 1, &texels[1]), (middle + PAGE - 1, &texels[2])] {
        assert!(victory_gpu::remap::read_into(at as _, &mut seam));
        assert_eq!(seam[0], 0);
        assert_eq!(seam[1..], expected[..4]);
    }
    // A storage write by page number is visible through both virtual
    // aliases, and its reverse lookup includes the software-only view.
    assert!(write_physical(&pages[1..2], 37, &[15, 31]));
    assert!(shown_at(&pages[1..2], 37, 2).contains(&(middle + 37, 2)));
    for at in [middle, second] {
        let mut seen = [0; 2];
        assert!(victory_gpu::remap::read_into((at + 37) as _, &mut seen));
        assert_eq!(seen, [15, 31]);
    }
    // A CPU reader racing a two-page replacement can fault between
    // generations. Its page must always name one complete pool slice.
    assert!(alias(guest, pool, 2 * PAGE));
    let gate = std::sync::Barrier::new(2);
    let done = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            assert_eq!(unsafe { (guest as *const u8).read_volatile() }, 0);
            gate.wait();
            while !done.load(Ordering::Acquire) {
                assert!(matches!(
                    unsafe { (guest as *const u8).read_volatile() },
                    0 | 51
                ));
            }
        });
        gate.wait();
        for i in 0..512 {
            assert!(alias(guest, pool + (i % 2) * PAGE, 2 * PAGE));
        }
        done.store(true, Ordering::Release);
    });
    assert_eq!(
        XMemMapPhysicalPages((pool + 2 * PAGE) as _, 1, std::ptr::null()) as usize,
        pool + 2 * PAGE
    );
    assert!(!alias(guest, pool, 3 * PAGE));
    assert!(victory_gpu::remap::read_into(middle as _, &mut upload));
    assert_eq!(upload[37], 15);
    assert!(!victory_gpu::remap::read_into(
        (guest + 2 * PAGE) as _,
        &mut upload
    ));
    assert!(!alias(guest, guest, PAGE));
    assert_eq!(
        XMemMapPhysicalPages(pool as _, count, std::ptr::null()) as usize,
        pool
    );
    assert_ne!(XMemFreePhysicalPages(count, pages.as_ptr()), 0);
    assert_eq!(unsafe { ((middle + 37) as *const u8).read_volatile() }, 15);
    assert!(alias(guest, 0, count * PAGE));
    for i in 0..count {
        assert_eq!(state(guest + i * PAGE), MEM_RESERVE);
    }
    // The CPU can coalesce and release pages that were never touched.
    for at in [pool, guest] {
        super::super::virtual_memory::VirtualFree(at as _, count * PAGE, MEM_RELEASE | 1);
        assert_ne!(
            super::super::virtual_memory::VirtualFree(at as _, 0, MEM_RELEASE),
            0
        );
    }
}
