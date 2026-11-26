//! The fork sampled its submit worker on slower cores 1,529 times in 24,021 samples.
//! Its worker stayed on fast cores in the next run. Frame time still needs a local A/B.
//! Approach after XCL 7b029e2: fit processor requests to the affinity the host permits.

use windows_sys::Win32::System::SystemInformation::{
    CpuSetInformation, GROUP_AFFINITY, GetSystemCpuSetInformation, SYSTEM_CPU_SET_INFORMATION,
};
use windows_sys::Win32::System::Threading::{
    GetActiveProcessorGroupCount, GetCurrentProcess, GetCurrentThread, GetProcessAffinityMask,
    GetProcessDefaultCpuSets, GetThreadGroupAffinity, GetThreadSelectedCpuSets,
    SetThreadSelectedCpuSets,
};

/// Select the fastest available class only when this worker has a choice of classes.
fn fast_ids(records: &[SYSTEM_CPU_SET_INFORMATION], allowed: usize) -> Vec<u32> {
    let mut choices = Vec::new();
    for info in records.iter().filter(|r| r.Type == CpuSetInformation) {
        // SAFETY: the record type identifies the CPU set arm. AllFlags is its flags byte.
        let set = unsafe { info.Anonymous.CpuSet };
        let flags = unsafe { set.Anonymous1.AllFlags };
        let parked = flags & 1 != 0;
        let reserved_elsewhere = flags & 2 != 0 && flags & 4 == 0;
        if set.Group == 0
            && allowed
                .checked_shr(u32::from(set.LogicalProcessorIndex))
                .unwrap_or(0)
                & 1
                != 0
            && !parked
            && !reserved_elsewhere
        {
            choices.push((set.Id, set.EfficiencyClass));
        }
    }
    let Some(best) = choices.iter().map(|&(_, class)| class).max() else {
        return Vec::new();
    };
    if choices.iter().all(|&(_, class)| class == best) {
        return Vec::new();
    }
    choices
        .into_iter()
        .filter_map(|(id, class)| (class == best).then_some(id))
        .collect()
}

pub(crate) fn select(worker: &str) {
    if std::env::var_os("VICTORY_FAST_CORES").is_none_or(|v| v != "1") {
        return;
    }
    // SAFETY: pseudo-handles for this worker and its process. Zero-length calls query sizes.
    let (thread, process) = unsafe { (GetCurrentThread(), GetCurrentProcess()) };
    let mut group = GROUP_AFFINITY::default();
    let (mut allowed, mut system, mut count) = (0, 0, 0);
    // Preserve any explicit CPU set policy. A group-relative mask is unambiguous on one group.
    if unsafe {
        GetActiveProcessorGroupCount() != 1
            || GetThreadGroupAffinity(thread, &mut group) == 0
            || GetProcessAffinityMask(process, &mut allowed, &mut system) == 0
            || GetProcessDefaultCpuSets(process, std::ptr::null_mut(), 0, &mut count) == 0
            || count != 0
            || GetThreadSelectedCpuSets(thread, std::ptr::null_mut(), 0, &mut count) == 0
            || count != 0
    } {
        return;
    }
    let mut bytes = 0;
    // SAFETY: size query, followed by a buffer of at least the required size and alignment.
    unsafe { GetSystemCpuSetInformation(std::ptr::null_mut(), 0, &mut bytes, process, 0) };
    if bytes == 0 {
        return;
    }
    let mut buffer = vec![0u64; (bytes as usize).div_ceil(8)];
    let capacity = bytes;
    if unsafe {
        GetSystemCpuSetInformation(buffer.as_mut_ptr().cast(), capacity, &mut bytes, process, 0)
    } == 0
        || bytes > capacity
    {
        return;
    }
    let mut records = Vec::new();
    let mut at = 0;
    while at < bytes as usize {
        if (bytes as usize) - at < size_of::<SYSTEM_CPU_SET_INFORMATION>() {
            return;
        }
        // SAFETY: the complete record header and known payload fit in the initialized buffer.
        let info = unsafe {
            buffer
                .as_ptr()
                .cast::<u8>()
                .add(at)
                .cast::<SYSTEM_CPU_SET_INFORMATION>()
                .read_unaligned()
        };
        let size = info.Size as usize;
        if size < size_of::<SYSTEM_CPU_SET_INFORMATION>() || size > bytes as usize - at {
            return;
        }
        records.push(info);
        at += size;
    }
    let ids = fast_ids(&records, allowed & group.Mask);
    if !ids.is_empty() {
        // SAFETY: all IDs came from Windows. The slice stays alive during the call.
        let applied =
            unsafe { SetThreadSelectedCpuSets(thread, ids.as_ptr(), ids.len() as u32) } != 0;
        tracing::info!(
            worker,
            fast_sets = ids.len(),
            applied,
            "fast core selection"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::SystemInformation::{
        SYSTEM_CPU_SET_INFORMATION_0, SYSTEM_CPU_SET_INFORMATION_0_0,
        SYSTEM_CPU_SET_INFORMATION_0_0_0,
    };

    fn record(id: u32, index: u8, class: u8, flags: u8) -> SYSTEM_CPU_SET_INFORMATION {
        SYSTEM_CPU_SET_INFORMATION {
            Size: size_of::<SYSTEM_CPU_SET_INFORMATION>() as u32,
            Type: CpuSetInformation,
            Anonymous: SYSTEM_CPU_SET_INFORMATION_0 {
                CpuSet: SYSTEM_CPU_SET_INFORMATION_0_0 {
                    Id: id,
                    LogicalProcessorIndex: index,
                    EfficiencyClass: class,
                    Anonymous1: SYSTEM_CPU_SET_INFORMATION_0_0_0 { AllFlags: flags },
                    ..Default::default()
                },
            },
        }
    }

    #[test]
    fn chooses_only_eligible_fast_cores_on_a_hybrid_host() {
        let sets = [
            record(10, 0, 0, 0),
            record(11, 1, 1, 0),
            record(12, 2, 2, 1),
            record(13, 3, 2, 2),
            record(14, 4, 1, 6),
            record(15, 5, 3, 0),
        ];
        assert_eq!(fast_ids(&sets, 0b11111), [11, 14]);
        assert!(fast_ids(&sets, 0b10010).is_empty());
        assert!(fast_ids(&sets, 0).is_empty());
    }
}
