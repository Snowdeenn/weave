//! Owned Windows virtual allocations with an explicit NUMA preference.
//!
//! `VirtualAllocExNuma` reserves and commits storage in the current process.
//! Physical pages are allocated when touched, preferably on the selected node;
//! Windows may use another node when the preferred node lacks available pages.
//! Consequently this backend accepts `NumaPolicy::Prefer`, and rejects strict
//! `Bind` without allocating. It never reports a preference as a strict binding.
//!
//! Node discovery uses `GetNumaAvailableMemoryNodeEx` rather than CPU membership:
//! memory-only nodes may have no CPUs and are still legitimate allocation targets.
//! The discovered snapshot is validated before allocation; Windows provides final
//! validation if the machine's topology changes in the meantime.
//!
//! The region owns the original allocation base and a possibly shifted typed
//! data pointer. Extra alignment padding supports types aligned beyond the OS
//! allocation granularity. `VirtualFreeEx(MEM_RELEASE)` always receives the
//! original base and zero size. Element initialization and destruction belong to
//! `NumaBuffer`; this owner only allocates and releases raw storage.
//!
//! No physical residency, page locking or migration is requested or verified.
//! See [VirtualAllocExNuma](https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-virtualallocexnuma).

use super::{MemoryError, NumaPolicy};
use crate::topology::NumaNodeId;
use windows_sys::Win32::System::{Memory, Threading};

/// Discovers valid memory nodes, retaining sparse Windows node numbers.
///
/// A failed availability query for a candidate means it is not a usable memory
/// node in this snapshot. A valid node with zero free bytes remains a member:
/// preference allocation may use another node. Failure to query the highest
/// node, or finding no valid memory nodes, is returned as an error.
pub(super) fn read_memory_nodes() -> Result<Vec<NumaNodeId>, MemoryError> {
    let mut highest = 0u32;
    // SAFETY: highest is writable and lives for the call.
    if unsafe { Threading::GetNumaHighestNodeNumber(&mut highest) } == 0 {
        return Err(MemoryError::Os(std::io::Error::last_os_error()));
    }
    let highest = u16::try_from(highest)
        .map_err(|_| MemoryError::InvalidNodeId(NumaNodeId::new(highest as usize)))?;
    let mut nodes = Vec::new();
    for node in 0..=highest {
        let mut available = 0u64;
        // SAFETY: available is a live writable output; no page allocation occurs.
        if unsafe { Threading::GetNumaAvailableMemoryNodeEx(node, &mut available) } != 0 {
            nodes.push(NumaNodeId::new(usize::from(node)));
        }
    }
    if nodes.is_empty() {
        return Err(MemoryError::Os(std::io::Error::other(
            "no Windows memory nodes found",
        )));
    }
    Ok(nodes)
}

/// Validates an allocation's node preference against an explicit snapshot.
fn preferred_node(policy: NumaPolicy, known_nodes: &[NumaNodeId]) -> Result<u32, MemoryError> {
    let id = match policy {
        NumaPolicy::Prefer(id) => id,
        NumaPolicy::Bind(_) => return Err(MemoryError::UnsupportedPolicy(policy)),
    };
    if !known_nodes.contains(&id) {
        return Err(MemoryError::UnknownNode(id));
    }
    // Extended NUMA discovery uses USHORT identifiers. Reject rather than
    // truncating even though VirtualAllocExNuma itself takes a DWORD.
    let node = u16::try_from(id.get()).map_err(|_| MemoryError::InvalidNodeId(id))?;
    Ok(u32::from(node))
}

/// Exclusive owner of uninitialized, writable storage for `capacity` values.
pub(super) struct MappedRegion<T> {
    /// Original reservation base, required by VirtualFreeEx.
    base: *mut u8,
    /// Aligned typed pointer within the reservation.
    data: *mut T,
    capacity: usize,
    /// Accepted preference; not a physical page-residency observation.
    policy: NumaPolicy,
}

impl<T> MappedRegion<T> {
    /// Checks arithmetic, slice limits and supported policy before allocation.
    pub(super) fn validate_request(
        capacity: usize,
        policy: NumaPolicy,
    ) -> Result<usize, MemoryError> {
        if capacity == 0 || std::mem::size_of::<T>() == 0 {
            return Err(MemoryError::InvalidSize);
        }
        let bytes = capacity
            .checked_mul(std::mem::size_of::<T>())
            .and_then(|bytes| bytes.checked_add(std::mem::align_of::<T>() - 1))
            .filter(|bytes| *bytes <= isize::MAX as usize)
            .ok_or(MemoryError::InvalidSize)?;
        if matches!(policy, NumaPolicy::Bind(_)) {
            return Err(MemoryError::UnsupportedPolicy(policy));
        }
        Ok(bytes)
    }

    /// Reserves and commits a fresh allocation with a validated NUMA preference.
    ///
    /// No `T` values are initialized here. Zero capacity, zero-sized types and
    /// overflowing layouts are rejected. No huge pages or privileges are needed.
    pub(super) fn try_allocate(
        capacity: usize,
        policy: NumaPolicy,
        known_nodes: &[NumaNodeId],
    ) -> Result<Self, MemoryError> {
        let bytes = Self::validate_request(capacity, policy)?;
        let node = preferred_node(policy, known_nodes)?;
        // SAFETY: the current-process pseudo-handle is valid. A null address
        // requests fresh storage, with no existing mappings replaced. The byte
        // count is nonzero and checked; failure is checked before pointer use.
        let base = unsafe {
            Memory::VirtualAllocExNuma(
                Threading::GetCurrentProcess(),
                std::ptr::null(),
                bytes,
                Memory::MEM_RESERVE | Memory::MEM_COMMIT,
                Memory::PAGE_READWRITE,
                node,
            )
        }
        .cast::<u8>();
        if base.is_null() {
            return Err(MemoryError::Os(std::io::Error::last_os_error()));
        }
        let offset = base.align_offset(std::mem::align_of::<T>());
        if offset > std::mem::align_of::<T>() - 1 {
            // SAFETY: release the new allocation on this error path, using
            // the original base and the zero size required by MEM_RELEASE.
            unsafe {
                Memory::VirtualFreeEx(
                    Threading::GetCurrentProcess(),
                    base.cast(),
                    0,
                    Memory::MEM_RELEASE,
                );
            }
            return Err(MemoryError::Align);
        }
        // SAFETY: padding covers this offset; capacity values remain in bounds.
        let data = unsafe { base.add(offset) }.cast();
        Ok(Self {
            base,
            data,
            capacity,
            policy,
        })
    }

    pub(super) fn data_ptr(&self) -> *mut T {
        self.data
    }
    pub(super) fn capacity(&self) -> usize {
        self.capacity
    }
    pub(super) fn policy(&self) -> Option<NumaPolicy> {
        Some(self.policy)
    }
}

impl<T> Drop for MappedRegion<T> {
    fn drop(&mut self) {
        // SAFETY: exclusively owned live reservation; release its original
        // base, not the aligned data pointer. The caller drops values first.
        // Drop cannot return an OS error; this mirrors the Linux mapping owner.
        unsafe {
            Memory::VirtualFreeEx(
                Threading::GetCurrentProcess(),
                self.base.cast(),
                0,
                Memory::MEM_RELEASE,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> NumaPolicy {
        NumaPolicy::Prefer(read_memory_nodes().unwrap()[0])
    }

    #[test]
    fn validates_sparse_nodes_and_rejects_strict_binding() {
        let node = NumaNodeId::new(7);
        assert_eq!(
            preferred_node(NumaPolicy::Prefer(node), &[node]).unwrap(),
            7
        );
        assert!(matches!(
            preferred_node(NumaPolicy::Prefer(NumaNodeId::new(0)), &[node]),
            Err(MemoryError::UnknownNode(_))
        ));
        let oversized = NumaNodeId::new(usize::from(u16::MAX) + 1);
        assert!(matches!(
            preferred_node(NumaPolicy::Prefer(oversized), &[oversized]),
            Err(MemoryError::InvalidNodeId(_))
        ));
        assert!(
            matches!(MappedRegion::<u8>::try_allocate(1, NumaPolicy::Bind(node), &[node]),
            Err(MemoryError::UnsupportedPolicy(NumaPolicy::Bind(id))) if id == node)
        );
    }

    #[test]
    fn rejects_invalid_layouts_before_allocating() {
        let policy = NumaPolicy::Prefer(NumaNodeId::new(0));
        for size in [0, isize::MAX as usize + 1, usize::MAX] {
            assert!(matches!(
                MappedRegion::<u8>::validate_request(size, policy),
                Err(MemoryError::InvalidSize)
            ));
        }
        assert!(matches!(
            MappedRegion::<()>::validate_request(1, policy),
            Err(MemoryError::InvalidSize)
        ));
        assert!(matches!(
            MappedRegion::<u64>::validate_request(usize::MAX / 8 + 1, policy),
            Err(MemoryError::InvalidSize)
        ));
    }

    #[test]
    fn supports_overaligned_values_and_releases_the_original_reservation() {
        #[repr(align(131072))]
        struct Aligned(u8);
        let region =
            MappedRegion::<Aligned>::try_allocate(2, policy(), &read_memory_nodes().unwrap())
                .unwrap();
        assert_eq!(
            region.data_ptr() as usize % std::mem::align_of::<Aligned>(),
            0
        );
        let base = region.base;
        // SAFETY: both typed slots are aligned and within the live allocation.
        unsafe {
            // Initialize the fields directly: do not create large, highly
            // aligned temporary values on the small test thread stack.
            std::ptr::addr_of_mut!((*region.data_ptr()).0).write(11);
            std::ptr::addr_of_mut!((*region.data_ptr().add(1)).0).write(29);
            assert_eq!((*region.data_ptr()).0, 11);
            assert_eq!((*region.data_ptr().add(1)).0, 29);
            std::ptr::drop_in_place(std::ptr::slice_from_raw_parts_mut(region.data_ptr(), 2));
        }
        drop(region);
        let mut information = Memory::MEMORY_BASIC_INFORMATION::default();
        // SAFETY: query address metadata without dereferencing released storage.
        let result = unsafe {
            Memory::VirtualQuery(
                base.cast(),
                &mut information,
                std::mem::size_of_val(&information),
            )
        };
        assert_ne!(result, 0);
        assert_eq!(information.State, Memory::MEM_FREE);
    }
}
