//! Windows worker-thread pinning through processor-group affinity.
//!
//! CPU identifiers use the topology backend's `group * 64 + index` convention.
//! A single-bit `GROUP_AFFINITY` restricts the calling thread to that logical
//! CPU, including on machines with multiple processor groups. Windows validates
//! that the selected CPU exists and that the execution environment permits it.
//! This does not change other threads' affinities or place their memory.

use super::AffinityError;
use crate::topology::CpuId;
use windows_sys::Win32::System::{SystemInformation::GROUP_AFFINITY, Threading};

/// Converts a topology CPU ID to an affinity containing exactly one CPU.
///
/// Rejects group numbers that do not fit the Windows `u16` representation and
/// bit indices that do not fit the target's native affinity mask. Representable
/// identities may still identify absent CPUs; the OS checks that at pinning.
fn group_affinity_for(cpu: CpuId) -> Result<GROUP_AFFINITY, AffinityError> {
    let group = u16::try_from(cpu.get() / 64).map_err(|_| AffinityError::CpuOutOfRange(cpu))?;
    let index = (cpu.get() % 64) as u32;
    let mask = 1usize
        .checked_shl(index)
        .ok_or(AffinityError::CpuOutOfRange(cpu))?;

    Ok(GROUP_AFFINITY {
        Group: group,
        Mask: mask,
        Reserved: [0; 3],
    })
}

/// Permanently restricts the calling worker to the requested logical CPU.
///
/// The affinity remains until changed again or until the thread exits. The
/// current-thread pseudo-handle requires no allocation or `CloseHandle` call.
/// We deliberately do not request the previous affinity because worker startup
/// does not restore it. On failure, preserve the original Windows OS error.
pub(crate) fn pin_current_thread(cpu: CpuId) -> Result<(), AffinityError> {
    let affinity = group_affinity_for(cpu)?;

    // SAFETY: the pseudo-handle refers to this thread; affinity is initialized
    // and readable for the call, and the optional previous-affinity output is
    // null. Windows validates the group and CPU mask.
    let result = unsafe {
        Threading::SetThreadGroupAffinity(
            Threading::GetCurrentThread(),
            &affinity,
            std::ptr::null_mut(),
        )
    };

    if result == 0 {
        return Err(AffinityError::Os(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_one_bit_masks_with_group_local_numbering() {
        for (id, group, index) in [(0, 0, 0), (3, 0, 3), (64, 1, 0), (67, 1, 3)] {
            let affinity = group_affinity_for(CpuId::new(id)).unwrap();
            assert_eq!(affinity.Group, group);
            assert_eq!(affinity.Mask, 1usize << index);
            assert_eq!(affinity.Reserved, [0; 3]);
        }
        #[cfg(target_pointer_width = "64")]
        {
            let affinity = group_affinity_for(CpuId::new(127)).unwrap();
            assert_eq!(affinity.Group, 1);
            assert_eq!(affinity.Mask, 1usize << 63);
        }
    }

    #[test]
    fn rejects_group_numbers_that_would_be_truncated() {
        let cpu = CpuId::new((usize::from(u16::MAX) + 1) * 64);
        assert!(matches!(group_affinity_for(cpu),
            Err(AffinityError::CpuOutOfRange(id)) if id == cpu));
        assert!(matches!(
            group_affinity_for(CpuId::new(usize::MAX)),
            Err(AffinityError::CpuOutOfRange(_))
        ));
    }

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn rejects_bits_outside_a_32_bit_affinity_mask() {
        assert!(matches!(
            group_affinity_for(CpuId::new(32)),
            Err(AffinityError::CpuOutOfRange(_))
        ));
    }

    #[test]
    fn pins_a_dedicated_thread_to_one_allowed_cpu() {
        // Pinning ends with this dedicated thread; the test runner retains its
        // original affinity. Select from the thread's allowed primary group.
        std::thread::spawn(|| {
            let allowed = current_affinity().expect("cannot read thread affinity");
            assert_ne!(allowed.Mask, 0);
            let index = allowed.Mask.trailing_zeros() as usize;
            let cpu = CpuId::new(usize::from(allowed.Group) * 64 + index);

            pin_current_thread(cpu).expect("cannot pin Windows test thread");
            let actual = current_affinity().expect("cannot read pinned affinity");
            assert_eq!(actual.Group, allowed.Group);
            assert_eq!(actual.Mask, 1usize << index);
        })
        .join()
        .expect("Windows affinity test thread panicked");
    }

    #[test]
    fn preserves_the_os_error_for_an_absent_group() {
        std::thread::spawn(|| {
            // This representable group is outside the active group range.
            let count = unsafe { Threading::GetActiveProcessorGroupCount() };
            assert!(count > 0);
            let cpu = CpuId::new(usize::from(count) * 64);
            match pin_current_thread(cpu) {
                Err(AffinityError::Os(error)) => assert!(error.raw_os_error().is_some()),
                other => panic!("expected a Windows OS error, got {other:?}"),
            }
        })
        .join()
        .expect("Windows affinity error test thread panicked");
    }

    fn current_affinity() -> Result<GROUP_AFFINITY, std::io::Error> {
        let mut affinity = GROUP_AFFINITY::default();
        // SAFETY: the output is writable for the duration of the call, and
        // GetCurrentThread supplies a valid pseudo-handle for this thread.
        let result = unsafe {
            Threading::GetThreadGroupAffinity(Threading::GetCurrentThread(), &mut affinity)
        };
        if result == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(affinity)
        }
    }
}
