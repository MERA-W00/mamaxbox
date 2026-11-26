//! Pass-through to the host's own implementation of shared APIs.

use victory_pe::ImportSymbol;

/// Which DLLs may be satisfied by the host's own implementation.
///
/// Console titles link mostly against Windows API sets that behave the same
/// on PC. Passing those through lets bring-up focus on what really differs.
/// Anything whose semantics differ on console (memory, files, threads) is
/// overridden in `modules`, which always wins over the host.
#[derive(Debug, Clone)]
pub struct HostPolicy {
    /// Lower-case patterns; a trailing `*` matches a prefix.
    pub allow: Vec<String>,
}

impl Default for HostPolicy {
    fn default() -> Self {
        let allow = [
            "api-ms-win-core-*",
            "api-ms-win-eventing-*",
            "api-ms-win-security-*",
            "api-ms-win-crt-*",
            "kernel32.dll",
            "kernelbase.dll",
            "ntdll.dll",
            "ucrtbase.dll",
            "vcruntime140*",
            "msvcp140*",
            // Off-machine traffic is refused in `modules::net`.
            "ws2_32.dll",
            "bcrypt.dll",
            "crypt32.dll",
            "ncrypt.dll",
            "winhttp.dll",
            "sspicli.dll",
            "cabinet.dll",
            "avrt.dll",
            // Window, message and shell APIs: the host's desktop window
            // stands in for the console's single core window.
            "ext-ms-win-rtcore-ntuser-*",
            "api-ms-win-shcore-*",
            "msvcp_win.dll",
            "ole32.dll",
            "advapi32.dll",
            "iphlpapi.dll",
            // The title's crash handler writes minidumps with it (loaded by name).
            "dbghelp.dll",
            // Same APIs as the console's (XAudio2 ordinal 1 is XAudio2Create
            // on both); Media Foundation reads local media files.
            "xaudio2_9.dll",
            "mfplat.dll",
            "mfreadwrite.dll",
        ];
        HostPolicy {
            allow: allow.iter().map(|s| s.to_string()).collect(),
        }
    }
}

impl HostPolicy {
    /// No pass-through at all: shows the full surface a title needs.
    pub fn none() -> Self {
        HostPolicy { allow: Vec::new() }
    }

    pub fn allows(&self, dll: &str) -> bool {
        let dll = dll.to_ascii_lowercase();
        self.allow
            .iter()
            .any(|pattern| match pattern.strip_suffix('*') {
                Some(prefix) => dll.starts_with(prefix),
                None => dll == *pattern,
            })
    }

    pub(crate) fn resolve(&self, dll: &str, symbol: &ImportSymbol) -> Option<usize> {
        if !self.allows(dll) {
            return None;
        }
        imp::resolve(dll, symbol)
    }
}

/// The host's own `name` from the System32 DLL `dll`: what a title import
/// of it is bound to when we do not override it.
pub(crate) fn function(dll: &str, name: &str) -> Option<usize> {
    let name = ImportSymbol::Name {
        hint: 0,
        name: name.into(),
    };
    imp::resolve(dll, &name)
}

#[cfg(windows)]
mod imp {
    use std::collections::HashMap;
    use std::ffi::CString;
    use std::sync::{LazyLock, Mutex};

    use victory_pe::ImportSymbol;
    use windows_sys::Win32::System::LibraryLoader::{
        GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExA,
    };

    /// Module handles by lower-case DLL name (stored as usize: Send + Sync).
    static MODULES: LazyLock<Mutex<HashMap<String, usize>>> = LazyLock::new(Default::default);

    pub(super) fn resolve(dll: &str, symbol: &ImportSymbol) -> Option<usize> {
        let module = module(dll)?;
        let proc = match symbol {
            ImportSymbol::Name { name, .. } => {
                let name = CString::new(name.as_str()).ok()?;
                // SAFETY: valid module handle, NUL-terminated name.
                unsafe { GetProcAddress(module as _, name.as_ptr().cast()) }
            }
            // SAFETY: MAKEINTRESOURCE-style ordinal lookup on a valid module.
            ImportSymbol::Ordinal(o) => unsafe { GetProcAddress(module as _, *o as usize as _) },
        };
        proc.map(|f| f as *const () as usize)
    }

    fn module(dll: &str) -> Option<usize> {
        static UNLOCK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let unlocked = *UNLOCK
            .get_or_init(|| std::env::var_os("VICTORY_UNLOCK_DLL_LOAD").is_some_and(|v| v == "1"));
        cached_module(
            &MODULES,
            dll.to_ascii_lowercase(),
            unlocked,
            load,
            |extra| {
                // SAFETY: our concurrent LoadLibraryExA acquired this extra reference.
                unsafe { windows_sys::Win32::Foundation::FreeLibrary(extra as _) };
            },
        )
    }

    /// Loading a DLL can run callbacks under the Windows loader lock. Releasing the cache
    /// lock during a load lets those callbacks look up an import on another thread.
    fn cached_module(
        modules: &Mutex<HashMap<String, usize>>,
        key: String,
        unlocked: bool,
        load: impl FnOnce(&str) -> Option<usize>,
        release: impl FnOnce(usize),
    ) -> Option<usize> {
        let lock = || modules.lock().unwrap_or_else(|e| e.into_inner());
        let mut cache = lock();
        if let Some(&module) = cache.get(&key) {
            return Some(module);
        }
        if unlocked {
            drop(cache);
            let handle = load(&key)?;
            cache = lock();
            if let Some(&module) = cache.get(&key) {
                drop(cache);
                release(handle);
                return Some(module);
            }
            cache.insert(key, handle);
            Some(handle)
        } else {
            let handle = load(&key)?;
            cache.insert(key, handle);
            Some(handle)
        }
    }

    fn load(key: &str) -> Option<usize> {
        let name = CString::new(key).ok()?;
        // SAFETY: a NUL-terminated allow-listed name, restricted to System32.
        let handle = unsafe {
            LoadLibraryExA(
                name.as_ptr().cast(),
                std::ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        };
        (!handle.is_null()).then_some(handle as usize)
    }

    #[cfg(test)]
    mod cache_tests {
        use super::*;

        #[test]
        fn reentrant_publication_releases_only_the_extra_reference() {
            let cache = Mutex::new(HashMap::new());
            let mut released = None;
            let module = cached_module(
                &cache,
                "a".into(),
                true,
                |_| {
                    let mut map = cache.try_lock().unwrap();
                    map.insert("a".into(), 11);
                    Some(12)
                },
                |extra| released = Some(extra),
            );
            assert_eq!(module, Some(11));
            assert_eq!(released, Some(12));
            assert_eq!(
                cached_module(&cache, "a".into(), true, |_| panic!(), |_| panic!()),
                Some(11)
            );
        }

        #[test]
        fn failed_load_is_retryable_and_legacy_load_holds_the_lock() {
            let cache = Mutex::new(HashMap::new());
            for unlocked in [true, false] {
                assert_eq!(
                    cached_module(
                        &cache,
                        "a".into(),
                        unlocked,
                        |_| {
                            assert_eq!(cache.try_lock().is_ok(), unlocked);
                            None
                        },
                        |_| panic!()
                    ),
                    None
                );
            }
            assert_eq!(
                cached_module(&cache, "a".into(), false, |_| Some(42), |_| panic!()),
                Some(42)
            );
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub(super) fn resolve(_dll: &str, _symbol: &victory_pe::ImportSymbol) -> Option<usize> {
        None
    }
}
