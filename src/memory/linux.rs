use super::{MemoryError, NumaPolicy};
use crate::topology::NumaNodeId;

/// A single-node Linux bitmask and the raw syscall's `maxnode` argument.
/// Linux's get_nodes decrements maxnode before reading the mask, so this
/// argument is one greater than the number of meaningful bits, not a word count.
#[derive(Debug)]
struct NodeMask {
    words: Vec<libc::c_ulong>,
    max_node: libc::c_ulong,
}

impl NodeMask {
    fn try_new(id: NumaNodeId, known_nodes: &[NumaNodeId]) -> Result<Self, MemoryError> {
        // Check membership before allocating: IDs are sparse, not array indices.
        if !known_nodes.contains(&id) {
            return Err(MemoryError::UnknownNode(id));
        }
        // Linux's get_nodes() decrements the raw syscall's maxnode argument
        // before reading the mask. To include bit `id`, it must therefore see
        // id + 1 bits AFTER that decrement: pass id + 2, not id + 1.
        // For node 0, the mask is [0b1]: maxnode = 1 becomes 0, so Linux treats
        // it as an empty node set and MPOL_BIND fails with EINVAL. Passing 2
        // makes Linux read one bit and correctly select node 0.
        // This extra unit belongs to the syscall argument, not to the mask:
        // the number of allocated words below still only needs to cover `id`.
        // Reference: Linux mm/mempolicy.c, get_nodes().
        let max_node = id
            .get()
            .checked_add(2)
            .and_then(|count| libc::c_ulong::try_from(count).ok())
            .ok_or(MemoryError::InvalidNodeId(id))?;
        let bits = libc::c_ulong::BITS as usize;
        let word_index = id.get() / bits;
        let offset = id.get() % bits;
        let word_count = word_index
            .checked_add(1)
            .ok_or(MemoryError::InvalidNodeId(id))?;
        let mut words = Vec::new();
        words.try_reserve_exact(word_count)?;
        words.resize(word_count, 0);
        words[word_index] = (1 as libc::c_ulong) << offset;
        Ok(Self { words, max_node })
    }
}

pub(super) struct MappedRegion<T> {
    base: *mut u8,     // Adresse retournée par mmap.
    mapped_len: usize, // Longueur du mapping en octets.
    data: *mut T,      // Adresse alignée où seront construits les T.
    capacity: usize,   // Nombre de T que le stockage peut accueillir.
    policy: Option<NumaPolicy>,
}

impl<T> MappedRegion<T> {
    pub fn try_allocate(capacity: usize) -> Result<Self, MemoryError> {
        if capacity == 0 || std::mem::size_of::<T>() == 0 {
            return Err(MemoryError::InvalidSize);
        }

        let Some(data_bytes) = capacity.checked_mul(std::mem::size_of::<T>()) else {
            return Err(MemoryError::InvalidSize);
        };
        let Some(mapped_bytes) = data_bytes.checked_add(std::mem::align_of::<T>() - 1) else {
            return Err(MemoryError::InvalidSize);
        };

        if mapped_bytes > isize::MAX as usize {
            return Err(MemoryError::InvalidSize);
        }

        // SAFETY: Linux chooses the address of a fresh anonymous mapping; no
        // existing mapping is replaced. No file descriptor or input pointer is
        // used. MAP_FAILED is checked before storing the owned region.
        let adr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                mapped_bytes,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_ANON | libc::MAP_PRIVATE,
                -1,
                0,
            )
        };
        if adr == libc::MAP_FAILED {
            return Err(MemoryError::Os(std::io::Error::last_os_error()));
        }

        let ptr = adr.cast::<u8>();
        let offset = ptr.align_offset(std::mem::align_of::<T>());

        if offset > std::mem::align_of::<T>() - 1 {
            // SAFETY: mmap succeeded and ownership has not yet been transferred
            // into a MappedRegion. Release the original mapping on this error.
            let _ = unsafe { libc::munmap(adr, mapped_bytes) };
            return Err(MemoryError::Align);
        }

        let data = ptr.wrapping_add(offset).cast();

        Ok(MappedRegion {
            base: adr.cast(),
            mapped_len: mapped_bytes,
            policy: None,
            capacity,
            data,
        })
    }

    /// Applies a policy after checking membership in the caller's node snapshot.
    ///
    /// The caller supplies known memory nodes, not a count or a CPU affinity
    /// list. This keeps discovery outside the mapping owner. A snapshot is not
    /// proof that a node is still online or permitted by the current cpuset:
    /// Linux performs the final validation and may reject the operation.
    ///
    /// Call before touching pages to guide their initial allocation. No pages
    /// are migrated or inspected here. `policy` records only the last policy
    /// successfully applied through this object and is unchanged on failure.
    pub(super) fn set_numa_policy(
        &mut self,
        policy: NumaPolicy,
        known_nodes: &[NumaNodeId],
    ) -> Result<(), MemoryError> {
        match policy {
            NumaPolicy::Bind(id) => {
                let mask = NodeMask::try_new(id, known_nodes)?;
                // SAFETY: self owns a live, page-aligned mapping. The mask is
                // initialized, covers max_node - 1 bits (Linux decrements this
                // argument before reading), and stays alive throughout
                // the call. Argument widths match the Linux syscall ABI. Zero
                // flags request neither migration nor a residency check.
                let result = unsafe {
                    libc::syscall(
                        libc::SYS_mbind,
                        self.base,
                        self.mapped_len,
                        libc::MPOL_BIND,
                        mask.words.as_ptr(),
                        mask.max_node,
                        0 as libc::c_uint,
                    )
                };
                if result == -1 {
                    return Err(MemoryError::Os(std::io::Error::last_os_error()));
                }
                self.policy = Some(NumaPolicy::Bind(id));
            }
        };

        Ok(())
    }

    pub(super) fn length(&self) -> usize {
        self.mapped_len
    }

    pub(super) fn data_ptr(&self) -> *mut T {
        self.data
    }

    pub(super) fn capacity(&self) -> usize {
        self.capacity
    }

    pub(super) fn policy(&self) -> Option<NumaPolicy> {
        self.policy
    }
}

impl<T> Drop for MappedRegion<T> {
    fn drop(&mut self) {
        // SAFETY: cette adresse et cette longueur désignent le mapping
        // possédé exclusivement par self, qui n'a pas encore été libéré.
        let _ = unsafe { libc::munmap(self.base.cast(), self.mapped_len) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allocate(length: usize) -> MappedRegion<u8> {
        MappedRegion::try_allocate(length).expect("mapping allocation failed")
    }

    #[test]
    fn mask_sets_exactly_one_bit_across_word_boundaries() {
        let bits = libc::c_ulong::BITS as usize;
        for value in [0, bits - 1, bits, bits + 1, 2 * bits - 1, 2 * bits] {
            let id = NumaNodeId::new(value);
            let mask = NodeMask::try_new(id, &[id]).unwrap();
            assert_eq!(mask.max_node as usize, value + 2);
            assert_eq!(mask.words.len(), value / bits + 1);
            for (index, word) in mask.words.iter().enumerate() {
                let expected = if index == value / bits {
                    (1 as libc::c_ulong) << (value % bits)
                } else {
                    0
                };
                assert_eq!(*word, expected);
            }
        }
    }

    #[test]
    fn sparse_nodes_are_validated_by_membership_not_count() {
        let known = [NumaNodeId::new(0), NumaNodeId::new(2)];
        assert!(NodeMask::try_new(known[1], &known).is_ok());
        assert!(matches!(NodeMask::try_new(NumaNodeId::new(1), &known),
            Err(MemoryError::UnknownNode(id)) if id.get() == 1));
    }

    #[test]
    fn unknown_huge_node_is_rejected_before_mask_allocation() {
        let id = NumaNodeId::new(usize::MAX - 1);
        assert!(matches!(NodeMask::try_new(id, &[NumaNodeId::new(0)]),
            Err(MemoryError::UnknownNode(rejected)) if rejected == id));
        assert!(matches!(
            NodeMask::try_new(NumaNodeId::new(0), &[]),
            Err(MemoryError::UnknownNode(_))
        ));
    }

    #[test]
    fn unrepresentable_max_node_is_rejected_without_allocation() {
        for value in [usize::MAX - 1, usize::MAX] {
            let id = NumaNodeId::new(value);
            assert!(matches!(NodeMask::try_new(id, &[id]),
                Err(MemoryError::InvalidNodeId(rejected)) if rejected == id));
        }
    }

    #[test]
    fn rejected_node_leaves_policy_unchanged() {
        let mut mapping = allocate(1);
        assert_eq!(mapping.policy, None);
        assert!(matches!(
            mapping.set_numa_policy(NumaPolicy::Bind(NumaNodeId::new(2)), &[]),
            Err(MemoryError::UnknownNode(_))
        ));
        assert_eq!(mapping.policy, None);
    }

    #[test]
    fn reservation_failure_converts_and_preserves_its_source() {
        fn reserve_too_much() -> Result<(), MemoryError> {
            let mut words: Vec<libc::c_ulong> = Vec::new();
            words.try_reserve_exact(usize::MAX)?;
            Ok(())
        }
        let error = reserve_too_much().unwrap_err();
        assert!(matches!(error, MemoryError::Allocation(_)));
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn rejects_zero_length() {
        assert!(matches!(
            MappedRegion::<u8>::try_allocate(0),
            Err(MemoryError::InvalidSize)
        ));
    }

    #[test]
    fn rejects_zero_sized_types_under_the_current_contract() {
        assert!(matches!(
            MappedRegion::<()>::try_allocate(10),
            Err(MemoryError::InvalidSize)
        ));
    }

    #[test]
    fn rejects_overflow_and_sizes_above_the_pointer_limit() {
        // Multiplication overflow, then a representable size above isize::MAX.
        for capacity in [usize::MAX, isize::MAX as usize / 8 + 1] {
            assert!(matches!(
                MappedRegion::<u64>::try_allocate(capacity),
                Err(MemoryError::InvalidSize)
            ));
        }
        assert!(matches!(
            MappedRegion::<u8>::try_allocate(isize::MAX as usize + 1),
            Err(MemoryError::InvalidSize)
        ));
    }

    fn assert_storage<T>(region: &MappedRegion<T>, capacity: usize) {
        let alignment = std::mem::align_of::<T>();
        let data_bytes = capacity.checked_mul(std::mem::size_of::<T>()).unwrap();
        assert_eq!(region.capacity, capacity);
        assert!(!region.base.is_null());
        assert!(!region.data.is_null());
        assert_eq!(region.data as usize % alignment, 0);
        let offset = (region.data as usize)
            .checked_sub(region.base as usize)
            .unwrap();
        assert!(offset < alignment);
        assert!(offset.checked_add(data_bytes).unwrap() <= region.mapped_len);
        assert!(region.mapped_len <= isize::MAX as usize);
        assert_eq!(region.policy(), None);
    }

    #[test]
    fn typed_capacity_is_in_elements_and_every_slot_is_writable() {
        let region = MappedRegion::<u64>::try_allocate(19).unwrap();
        assert_storage(&region, 19);
        // SAFETY: the checks above establish alignment and bounds. Each u64 is
        // explicitly initialized before reading, without creating references.
        unsafe {
            for index in 0..region.capacity {
                region.data.add(index).write(index as u64 + 100);
            }
            for index in 0..region.capacity {
                assert_eq!(region.data.add(index).read(), index as u64 + 100);
            }
        }
    }

    #[test]
    fn strongly_aligned_storage_has_room_for_the_last_element() {
        #[repr(align(65536))]
        struct Aligned(u8);
        let region = MappedRegion::<Aligned>::try_allocate(3).unwrap();
        assert_storage(&region, 3);
        // SAFETY: all three slots are aligned and wholly inside the mapping.
        // Initialize/read only the u8 field; padding is not read as a value.
        unsafe {
            for index in 0..3 {
                std::ptr::addr_of_mut!((*region.data.add(index)).0).write(index as u8);
            }
            for index in 0..3 {
                assert_eq!(
                    std::ptr::addr_of!((*region.data.add(index)).0).read(),
                    index as u8
                );
            }
        }
    }

    #[test]
    fn drop_unmaps_every_page_in_an_isolated_process() {
        const CHILD: &str = "WEAVE_MAPPING_DROP_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            // Other tests allocate concurrently. Run only this test in a child
            // so they cannot reuse the addresses between Drop and mincore.
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "memory::linux::tests::drop_unmaps_every_page_in_an_isolated_process",
                    "--test-threads=1",
                ])
                .env(CHILD, "1")
                .output()
                .expect("cannot launch the isolated mapping test");
            assert!(
                output.status.success(),
                "isolated test failed:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        // SAFETY: sysconf requires no memory arguments.
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        assert!(page_size > 0);
        let page_size = page_size as usize;
        let length = page_size.checked_mul(2).unwrap().checked_add(17).unwrap();
        for _ in 0..32 {
            let mapping = allocate(length);
            let base = mapping.base as usize;
            let mut residency = 0u8;
            for page in 0..3 {
                // SAFETY: mincore queries one aligned page and writes one byte
                // into residency. It does not dereference the queried address
                // in Rust, nor require the page to be physically resident.
                let result = unsafe {
                    libc::mincore(
                        (base + page * page_size) as *mut libc::c_void,
                        page_size,
                        &mut residency,
                    )
                };
                assert_eq!(result, 0, "page must be mapped before Drop");
            }
            drop(mapping);
            for page in 0..3 {
                // SAFETY: the output byte is valid. The old address is passed
                // only as a query to Linux; no freed memory is dereferenced.
                let result = unsafe {
                    libc::mincore(
                        (base + page * page_size) as *mut libc::c_void,
                        page_size,
                        &mut residency,
                    )
                };
                let error = std::io::Error::last_os_error();
                assert_eq!(result, -1, "page {page} remained mapped after Drop");
                assert_eq!(error.raw_os_error(), Some(libc::ENOMEM));
            }
        }
    }

    #[test]
    fn one_byte_mapping_is_zero_initialized_and_writable() {
        let mapping = allocate(1);
        assert_eq!(mapping.mapped_len, 1);
        assert_eq!(mapping.capacity, 1);
        // SAFETY: mmap succeeded for one readable/writable byte, and the mapping
        // remains alive. Raw pointer operations do not create aliased references.
        unsafe {
            assert_eq!(mapping.data.read(), 0);
            mapping.data.write(0xa5);
            assert_eq!(mapping.data.read(), 0xa5);
        }
        // Normal scope exit invokes Drop; never dereference the pointer afterward.
    }

    #[test]
    fn non_page_multiple_mapping_preserves_bytes_across_page_boundaries() {
        // SAFETY: sysconf takes a constant selector and no pointers.
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        assert!(page_size > 0, "cannot determine the page size");
        let page_size = page_size as usize;
        let length = page_size.checked_mul(2).unwrap().checked_add(17).unwrap();
        let mapping = allocate(length);
        assert_eq!(mapping.mapped_len, length);
        assert_eq!(mapping.capacity, length);
        assert_eq!(mapping.base as usize % page_size, 0);

        // SAFETY: all offsets are strictly below the requested mapping length.
        // The mapping stays alive and is accessed only by this test thread.
        unsafe {
            for offset in 0..length {
                assert_eq!(mapping.data.add(offset).read(), 0);
                mapping.data.add(offset).write((offset % 251) as u8);
            }
            for offset in 0..length {
                assert_eq!(mapping.data.add(offset).read(), (offset % 251) as u8);
            }
        }
    }

    #[test]
    fn mappings_are_independent_and_dropping_one_preserves_the_other() {
        let first = allocate(1);
        let second = allocate(1);
        assert_ne!(first.data, second.data);
        // SAFETY: these are two live, independent, writable one-byte mappings.
        unsafe {
            first.data.write(11);
            second.data.write(29);
            assert_eq!(first.data.read(), 11);
        }
        drop(first);
        // SAFETY: only first was dropped; second still owns its mapping.
        unsafe {
            assert_eq!(second.data.read(), 29);
            second.data.write(31);
            assert_eq!(second.data.read(), 31);
        }
    }
}
