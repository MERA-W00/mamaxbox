//! Approach after XCL 517d983: build with the device settings and cache paths
//! used during play. The input here is a recorded pipeline, including its
//! graphics state. A shader translation alone cannot warm the driver library.

use super::*;
use crate::pipeline::host::HostPipeline;
use std::cell::Cell;
use std::fs::{File, OpenOptions};
use std::os::windows::fs::MetadataExt;
use std::sync::mpsc;
use windows::Win32::Graphics::Dxgi::*;

const RECEIPT: &str = "precompiled-v1.txt";

fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_PIPELINE_PRECOMPILE").is_none_or(|v| v != "0"))
}

pub(super) fn tracking() -> bool {
    warm_enabled() && enabled()
}

/// Only one process may replace a library and its completion list. Windows
/// releases the lock even if a process is interrupted during publication.
pub(super) fn writer(dir: &Path) -> bool {
    static WRITER: OnceLock<Option<File>> = OnceLock::new();
    WRITER
        .get_or_init(|| {
            std::fs::create_dir_all(dir).ok()?;
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(dir.join("precompile.lock"))
                .ok()?;
            file.try_lock().ok()?;
            Some(file)
        })
        .is_some()
}

thread_local! { static IN_LIBRARY: Cell<bool> = const { Cell::new(false) }; }

pub(super) fn hit() {
    IN_LIBRARY.set(true);
}

fn completed() -> &'static Mutex<HashSet<String>> {
    static KEYS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    KEYS.get_or_init(Mutex::default)
}

/// Some mesh draws also need the pipeline without its launch stage. Play
/// makes that second object lazily. The offline pass can make both before
/// marking the title recipe complete.
pub(in crate::pipeline) fn build(p: &Pending, offline: bool) -> Option<HostPipeline> {
    IN_LIBRARY.set(false);
    let host = HostPipeline::new(&p.shaders, p.graphics.as_deref())?;
    if IN_LIBRARY.get() && tracking() {
        if let Some(launch) = &host.launch {
            IN_LIBRARY.set(false);
            if offline {
                launch.plain()?;
            }
        }
        if IN_LIBRARY.get() {
            completed().lock().unwrap().insert(p.key.clone());
        }
    }
    Some(host)
}

/// Device initialization also supplies the wave width and heap limit used
/// in the translation key. Checking never opens the large pipeline library.
fn identity() -> io::Result<&'static str> {
    static ID: OnceLock<String> = OnceLock::new();
    if let Some(id) = ID.get() {
        return Ok(id);
    }
    let host = crate::host::host().ok_or_else(|| io::Error::other("no D3D12 device"))?;
    crate::descriptor::host::probe_view_limit();
    // SAFETY: the device is live and DXGI fills plain adapter information.
    let (desc, version) = unsafe {
        let factory: IDXGIFactory4 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))?;
        let adapter: IDXGIAdapter1 = factory.EnumAdapterByLuid(host.device.GetAdapterLuid())?;
        (
            adapter.GetDesc1()?,
            adapter.CheckInterfaceSupport(&IDXGIDevice::IID)?,
        )
    };
    let mut hash = blake3::Hasher::new();
    hash.update_reader(File::open(std::env::current_exe()?)?)?;
    hash.update(&[std::env::var_os("VICTORY_SHADER_CACHE_KEY").is_some_and(|v| v == "exe") as u8]);
    hash.update(&victory_shader::emit::translation_fingerprint());
    hash.update(&[crate::launch::on() as u8]);
    hash.update(&[std::env::var_os("VICTORY_ROOT_SRV_DIRECT").is_some() as u8]);
    for value in [
        desc.VendorId,
        desc.DeviceId,
        desc.SubSysId,
        desc.Revision,
        desc.AdapterLuid.HighPart as u32,
        desc.AdapterLuid.LowPart,
    ] {
        hash.update(&value.to_le_bytes());
    }
    hash.update(&version.to_le_bytes());
    Ok(ID.get_or_init(|| hash.finalize().to_hex().to_string()))
}

fn header(dir: &Path, identity: &str) -> io::Result<String> {
    let next = dir.join(NEXT_FILE);
    let file = if next.exists() {
        next
    } else {
        dir.join(LIBRARY_FILE)
    };
    let lib = file.metadata()?;
    let shaders = dir.with_file_name("shaders").metadata()?;
    Ok(format!(
        "{identity} {} {} {}",
        lib.file_size(),
        lib.last_write_time(),
        shaders.creation_time()
    ))
}

fn read_keys(dir: &Path, header: &str) -> HashSet<String> {
    let Ok(bytes) = read_bounded(&dir.join(RECEIPT), 1 << 22) else {
        return HashSet::new();
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return HashSet::new();
    };
    let mut lines = text.lines();
    if lines.next() != Some(header) {
        return HashSet::new();
    }
    lines
        .filter(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_owned)
        .collect()
}

fn known(dir: &Path) -> io::Result<HashSet<String>> {
    if !dir.join(RECEIPT).is_file() {
        return Ok(HashSet::new());
    }
    Ok(header(dir, identity()?)
        .ok()
        .map_or_else(HashSet::new, |h| read_keys(dir, &h)))
}

pub(super) fn restore(dir: &Path) {
    if let Ok(keys) = known(dir) {
        completed().lock().unwrap().extend(keys);
    }
}

pub(super) fn snapshot() -> Option<HashSet<String>> {
    tracking()
        .then(|| completed().lock().unwrap().clone())
        .filter(|keys| !keys.is_empty())
}

pub(super) fn publish_keys(dir: &Path, keys: &HashSet<String>) -> io::Result<()> {
    let mut text = header(dir, identity()?)?;
    for key in keys {
        text.push('\n');
        text.push_str(key);
    }
    publish(&dir.join(RECEIPT), text.as_bytes())
}

fn rows(root: &Path) -> io::Result<Vec<(String, u64)>> {
    if !enabled() || std::env::var_os("VICTORY_PIPELINE_WARMUP").is_some_and(|v| v == "0") {
        return Ok(Vec::new());
    }
    ranking(&root.join("pipelines").join(RECIPE_DIR))
}

/// Number of saved recipes without a matching published host pipeline.
pub fn outstanding(root: &Path) -> io::Result<usize> {
    let rows = rows(root)?;
    if rows.is_empty() {
        return Ok(0);
    }
    let keys = known(&root.join("pipelines"))?;
    Ok(rows.iter().filter(|(key, _)| !keys.contains(key)).count())
}

/// Replays even zero-count recipes, which came from earlier runs. Workers
/// claim the next ranked recipe only after finishing their current build.
/// Cancelling drains those builds and publishes their successful results.
pub fn precompile(
    root: &Path,
    stop: &AtomicBool,
    mut progress: impl FnMut(usize, usize, usize) -> io::Result<()>,
) -> io::Result<()> {
    let mut rows = rows(root)?;
    if rows.is_empty() {
        return progress(0, 0, 0);
    }
    let dir = root.join("pipelines");
    use_pipeline_cache(dir.clone());
    if !writer(&dir) {
        return Err(io::Error::other(
            "pipeline cache already in use or unwritable",
        ));
    }
    crate::shader_cache::use_shader_cache(root.join("shaders"));
    identity()?;
    let lib = library().ok_or_else(|| io::Error::other("no host pipeline library"))?;
    {
        let built = completed().lock().unwrap();
        rows.retain(|(key, _)| !built.contains(key));
    }
    let total = rows.len();
    progress(0, total, 0)?;
    if total == 0 {
        return Ok(());
    }
    let recipes = dir.join(RECIPE_DIR);
    let reader = Mutex::new((rows.into_iter(), HashMap::new()));
    let (tx, rx) = mpsc::channel();
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(total);
    let mut done = 0;
    let mut failed = 0;
    let mut output = Ok(());
    std::thread::scope(|scope| {
        for _ in 0..threads {
            let tx = tx.clone();
            let reader = &reader;
            let recipes = &recipes;
            scope.spawn(move || {
                loop {
                    let recipe = {
                        let mut reader = reader.lock().unwrap();
                        if stop.load(Ordering::Acquire) {
                            break;
                        }
                        let Some((key, _)) = reader.0.next() else {
                            break;
                        };
                        read_recipe(recipes, &key, &mut reader.1)
                    };
                    let ok = recipe.is_ok_and(|p| build(&p, true).is_some() && IN_LIBRARY.get());
                    if tx.send(ok).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);
        for ok in rx {
            done += 1;
            failed += usize::from(!ok);
            if done < total && output.is_ok() {
                output = progress(done, total, failed);
                if output.is_err() {
                    stop.store(true, Ordering::Release);
                }
            }
        }
    });
    save_files(&dir, Some(lib), &USED, false);
    if lib.added.load(Ordering::Relaxed) != 0 {
        return Err(io::Error::other("pipeline library save failed"));
    }
    if let Some(keys) = snapshot() {
        publish_keys(&dir, &keys)?;
    }
    output?;
    progress(done, total, failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One lane executes only s_endpgm. Build every envelope here so the
    /// translation, disk cache and driver all receive a synthetic input.
    fn compute_container() -> Vec<u8> {
        let mut payload = vec![0; 0x200];
        payload[..8].copy_from_slice(&6u64.to_le_bytes());
        for offset in [8, 16, 24, 32, 40, 48, 72] {
            payload[offset..offset + 8].copy_from_slice(&0xdead_beef_dead_beefu64.to_le_bytes());
        }
        let record = 0x68;
        payload[0x60..0x68].copy_from_slice(&(record as u64).to_le_bytes());
        payload[record..record + 8].copy_from_slice(&0x200u64.to_le_bytes());
        payload[record + 8..record + 16].copy_from_slice(&4u64.to_le_bytes());
        payload[record + 0x14] = 32;
        payload[record + 0x50] = 4;
        let threads = record + 0x54 + 0x28;
        for offset in [threads, threads + 4, threads + 8] {
            payload[offset] = 1;
        }
        payload.extend(0xbf81_0000u32.to_le_bytes());

        let mut plain = b"FSSM".to_vec();
        plain.extend(0x101u32.to_le_bytes());
        plain.extend((payload.len() as u64).to_le_bytes());
        plain.extend(payload);
        let mut buffer = vec![0; zlib_rs::compress_bound(plain.len())];
        let (packed, status) =
            zlib_rs::compress_slice(&mut buffer, &plain, zlib_rs::DeflateConfig::new(6));
        assert_eq!(status, zlib_rs::ReturnCode::Ok);
        let mut blob = b"XBCH".to_vec();
        blob.extend(3u32.to_le_bytes());
        blob.extend((packed.len() as u64).to_le_bytes());
        blob.extend((plain.len() as u64).to_le_bytes());
        blob.extend_from_slice(packed);
        let mut part = vec![0; 24];
        part.extend(b"XDAT");
        for v in [1u32, 36, 13, 48, blob.len() as u32] {
            part.extend(v.to_le_bytes());
        }
        part.extend(blob);
        let mut out = b"DXBC".to_vec();
        out.resize(20, 0);
        for v in [1u32, 44 + part.len() as u32, 1, 36] {
            out.extend(v.to_le_bytes());
        }
        out.extend(b"DXIL");
        out.extend((part.len() as u32).to_le_bytes());
        out.extend(part);
        out
    }

    #[test]
    fn offline_replay_translates_saves_resumes_and_retries_only_failures() {
        const CHILD: &str = "VICTORY_TEST_OFFLINE_REPLAY";
        if let Some(root) = std::env::var_os(CHILD) {
            let root = PathBuf::from(root);
            if crate::host::host().is_none() {
                return;
            }
            assert_eq!(outstanding(&root).unwrap(), 3);
            let stop = AtomicBool::new(true);
            let mut last = (0, 0, 0);
            let mut report = |d, t, f| {
                last = (d, t, f);
                Ok(())
            };
            precompile(&root, &stop, &mut report).unwrap();
            assert_eq!(last, (0, 3, 0));
            stop.store(false, Ordering::Release);
            precompile(&root, &stop, |d, t, f| {
                last = (d, t, f);
                Ok(())
            })
            .unwrap();
            assert_eq!(last, (3, 3, 1));
            assert_eq!(crate::shader_cache::counts().1, 2);
            assert_eq!(outstanding(&root).unwrap(), 1);
            assert!(
                root.join("pipelines")
                    .join(LIBRARY_FILE)
                    .metadata()
                    .unwrap()
                    .len()
                    > 0
            );
            precompile(&root, &stop, |d, t, f| {
                last = (d, t, f);
                Ok(())
            })
            .unwrap();
            assert_eq!(last, (1, 1, 1));
            // Keep a competing mapping from giving up the old name. The next
            // generation must survive another save and still be the one read.
            use std::os::windows::fs::OpenOptionsExt;
            let side = tempfile::tempdir().unwrap();
            let dir = side.path();
            std::fs::write(dir.join(LIBRARY_FILE), b"old generation").unwrap();
            std::fs::write(dir.join(NEXT_FILE), b"first successor").unwrap();
            let tmp = dir.join("synthetic.tmp");
            std::fs::write(&tmp, b"second successor").unwrap();
            replace_library(dir, &tmp).unwrap();
            let _hold = OpenOptions::new()
                .read(true)
                .share_mode(3)
                .open(dir.join(LIBRARY_FILE))
                .unwrap();
            assert_eq!(Blob::of(dir).bytes(), b"second successor");

            return;
        }
        let folder = tempfile::tempdir().unwrap();
        let dir = folder.path().join("pipelines").join(RECIPE_DIR);
        // Different source hashes can give the same host pipeline description.
        let first = compute_container();
        let mut other = first.clone();
        other[4] = 1;
        let entries = [first, other, b"synthetic invalid stage".to_vec()]
            .into_iter()
            .map(|bytes| {
                Arc::new(WarmEntry {
                    pipeline: Arc::new(Pending::new(
                        vec![crate::pipeline::shader("compute", &bytes)],
                        None,
                    )),
                    calls: Arc::new(AtomicU64::new(3)),
                    stored: AtomicBool::new(false),
                })
            })
            .collect();
        save_entries(&dir, entries).unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact","pipeline::library::precompile::tests::offline_replay_translates_saves_resumes_and_retries_only_failures","--nocapture"])
            .env(CHILD,folder.path()).env("VICTORY_PIPELINE_WARMUP","1").env("VICTORY_PIPELINE_PRECOMPILE","1")
            .output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(String::from_utf8_lossy(&result.stdout).contains("1 passed"));
    }

    #[test]
    fn receipt_matches_only_the_published_library_and_device() {
        let folder = tempfile::tempdir().unwrap();
        let dir = folder.path().join("pipelines");
        std::fs::create_dir(&dir).unwrap();
        std::fs::create_dir(folder.path().join("shaders")).unwrap();
        std::fs::write(dir.join(LIBRARY_FILE), b"synthetic library").unwrap();
        let key = "a".repeat(32);
        let old = header(&dir, "build adapter driver").unwrap();
        publish(&dir.join(RECEIPT), format!("{old}\n{key}").as_bytes()).unwrap();
        assert!(read_keys(&dir, &old).contains(&key));
        assert!(read_keys(&dir, &header(&dir, "changed driver").unwrap()).is_empty());
        std::fs::write(dir.join(NEXT_FILE), b"new synthetic library").unwrap();
        assert!(read_keys(&dir, &header(&dir, "build adapter driver").unwrap()).is_empty());
    }

    #[test]
    fn empty_cache_check_and_build_create_nothing() {
        let folder = tempfile::tempdir().unwrap();
        assert_eq!(outstanding(folder.path()).unwrap(), 0);
        let mut lines = Vec::new();
        precompile(folder.path(), &AtomicBool::new(false), |d, t, f| {
            lines.push((d, t, f));
            Ok(())
        })
        .unwrap();
        assert_eq!(lines, [(0, 0, 0)]);
        assert_eq!(folder.path().read_dir().unwrap().count(), 0);
    }
}
