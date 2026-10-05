//! Windows hardware-topology discovery through `GetLogicalProcessorInformationEx`.
//!
//! This backend translates Windows processor relationships into the portable
//! [`Topology`] model. Discovery has three stages:
//!
//! 1. [`build_buffer`] queries the required byte count and retrieves a snapshot.
//! 2. [`parse_buffer`] decodes that snapshot into owned CPU membership lists.
//! 3. [`discover_from`] joins core, package and NUMA memberships into
//!    [`LogicalCpu`] and [`NumaNode`] records.
//!
//! Callers constructing a complete topology should request `RelationAll`.
//! Requesting only one relationship is useful for inspecting a buffer, but does
//! not supply the package, core and NUMA data required by the final stage.
//!
//! # Buffer representation
//!
//! Windows writes a sequence of variable-sized
//! `SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX` entries. Each entry starts with a
//! four-byte relationship discriminator and a four-byte total size, followed by
//! the corresponding union member. The size includes the header. Entries must
//! therefore be traversed using their reported `Size`, not Rust's
//! `size_of::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>()`.
//!
//! The storage is a `Vec<u64>` to provide eight-byte alignment on Windows.
//! Its elements are allocation units, not decoded topology values. Casting a
//! pointer changes how the bytes are interpreted; it does not convert them or
//! change the allocation. The initialized storage also makes padding harmless
//! to retain. Only the byte length returned by Windows is parsed; allocation
//! rounding may leave additional unused bytes at the end.
//!
//! # CPU identity and memberships
//!
//! A Windows logical CPU is identified by `(processor group, bit index)`.
//! On 64-bit Windows, a group contains at most 64 logical CPUs. An affinity
//! mask describes membership within one group; a set bit is one logical CPU,
//! not necessarily one physical core. SMT siblings share a core membership.
//!
//! [`encode_cpu`] uses `group * 64 + index` as this library's Windows [`CpuId`]
//! convention. These values can be sparse and are not vector indices. They
//! should be decoded back into the group and local index for affinity calls.
//! They are not Windows-supplied global processor indices.
//!
//! NUMA IDs preserve `NodeNumber`. Windows does not supply numeric core or
//! package IDs in these entries, so the final stage assigns IDs from their
//! positions in the parsed lists. Such IDs identify this discovery snapshot;
//! stability across rediscovery or reboot is not guaranteed. NUMA membership
//! remains independent of package membership.
//!
//! # Current limits
//!
//! The parser currently reads only the first affinity in a package or NUMA
//! entry. The bindings' one-element arrays are placeholders for variable-sized
//! trailing arrays: copying the fixed Rust struct does not copy subsequent
//! affinities. Complete support for multiple groups requires reading those
//! elements from the original entry with count and size checks.
//!
//! Group-summary, cache, die and module entries are ignored. Groups are retained
//! through CPU identities rather than represented as portable topology objects.
//! Discovery describes reported hardware membership, not the subset allowed by
//! process affinity, CPU sets, or a particular worker's execution environment.
//!
//! The size query and data query are separate calls. A topology change between
//! them can cause `ERROR_INSUFFICIENT_BUFFER`; this version propagates that error
//! rather than reallocating and retrying. Final construction rejects missing
//! memberships, but currently selects the first matching package or NUMA node
//! and does not reject duplicate or conflicting memberships. Output retains
//! discovery order instead of explicitly sorting portable identifiers.
//!
//! This module only discovers topology. Thread pinning and NUMA memory placement
//! belong to the affinity and memory backends.
//!
//! # Errors and safety
//!
//! API failures preserve their Win32 code in [`TopologyError::Windows`].
//! Truncated entries and missing required memberships use `ERROR_INVALID_DATA`.
//! The initial `ERROR_INSUFFICIENT_BUFFER` response is expected during sizing.
//! Allocation failures follow the standard `Vec` allocation behavior.
//!
//! Raw pointers remain within the lifetime of the owned buffer. Entry bounds
//! are checked before reading the header or fixed payload; unaligned reads
//! avoid requiring each entry address to have Rust's typed alignment. The
//! internal buffer invariant is that `len` does not exceed allocated storage.
//!
//! Windows API references:
//! - [GetLogicalProcessorInformationEx](https://learn.microsoft.com/en-us/windows/win32/api/sysinfoapi/nf-sysinfoapi-getlogicalprocessorinformationex)
//! - [Processor groups](https://learn.microsoft.com/en-us/windows/win32/procthread/processor-groups)
//! - [PROCESSOR_RELATIONSHIP](https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-processor_relationship)

use windows_sys::Win32::Foundation as win_foundation;
use windows_sys::Win32::System::SystemInformation as win_info;

use super::{CoreId, CpuId, LogicalCpu, NumaNode, NumaNodeId, PackageId, Topology, TopologyError};

/// Owned, aligned storage for one successful Windows topology query.
///
/// Constructed by [`build_buffer`]; the allocation may be larger than the valid
/// byte length because it is rounded to whole `u64` elements. No pointers into
/// this storage may outlive the buffer or survive its reallocation.
pub(super) struct ProcessorInfoBuffer {
    /// Initialized allocation units containing the raw Windows entries.
    inner: Vec<u64>,
    /// Valid byte count returned by Windows, excluding unused allocation bytes.
    len: usize,
}

/// Group-local identity of a Windows logical processor.
///
/// Keep both components when comparing membership: index zero in group zero
/// and index zero in group one identify different logical CPUs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct WindowsCpu {
    /// Windows processor group number.
    group: u16,
    /// Bit position within that group's affinity mask (0..64 on 64-bit Windows).
    index: u8,
}

/// Copies the fixed payload of an entry, excluding its eight-byte header.
///
/// Returns `None` if the reported entry size cannot contain `T`. This reads
/// only the fixed Rust representation, including the first element of any
/// trailing-array placeholder; it does not decode all variable-sized elements.
///
/// # Safety
///
/// `entry` must point to readable memory spanning `size` bytes and remain valid
/// throughout the call. The bytes after the header must be a valid value of
/// `T`, and `T` must match the entry's relationship discriminator. `Copy` alone
/// does not guarantee arbitrary bytes are valid for a type. Current callers
/// use the integer-based Windows FFI payload structures. `8 + size_of::<T>()`
/// must not overflow. Alignment is not required because the read is unaligned.
unsafe fn read_payload<T: Copy>(entry: *const u8, size: usize) -> Option<T> {
    if size < 8 + std::mem::size_of::<T>() {
        return None;
    }

    Some(unsafe { entry.add(8).cast::<T>().read_unaligned() })
}

/// Queries the required byte count for one Windows relationship selector.
///
/// A null output pointer and zero capacity deliberately provoke
/// `ERROR_INSUFFICIENT_BUFFER`, which Windows uses to report the required size.
/// That response is accepted; all other API errors are returned unchanged.
fn get_buffer_length(relation: i32) -> Result<u32, win_foundation::WIN32_ERROR> {
    let mut bytes = 0u32;
    let result = unsafe {
        win_info::GetLogicalProcessorInformationEx(relation, std::ptr::null_mut(), &mut bytes)
    };

    if result == win_foundation::FALSE {
        let error = unsafe { win_foundation::GetLastError() };
        if error != win_foundation::ERROR_INSUFFICIENT_BUFFER {
            return Err(error);
        }
    }
    Ok(bytes)
}

/// Retrieves raw topology entries for `relation` into owned aligned storage.
///
/// `relation` is a Windows `LOGICAL_PROCESSOR_RELATIONSHIP` constant. Both the
/// sizing query and the data query use the same selector. `RelationAll` supplies
/// the relationships needed by [`discover_from`]. Capacity is measured in
/// bytes when passed to Windows, and in `u64` elements when allocating storage.
///
/// # Errors
///
/// Returns [`TopologyError::Windows`] for a failed sizing or data query. A
/// larger required size on the second call is propagated without a retry.
pub(super) fn build_buffer(relation: i32) -> Result<ProcessorInfoBuffer, TopologyError> {
    let mut bytes = get_buffer_length(relation)?;
    // Des u64 garantissent aussi un alignement de 8 octets.
    let mut buffer = vec![0u64; (bytes as usize).div_ceil(8)];

    let result = unsafe {
        win_info::GetLogicalProcessorInformationEx(relation, buffer.as_mut_ptr().cast(), &mut bytes)
    };

    if result == win_foundation::FALSE {
        return Err(unsafe { win_foundation::GetLastError() }.into());
    }

    Ok(ProcessorInfoBuffer {
        inner: buffer,
        len: bytes as usize,
    })
}

/// Expands one group affinity into ascending group-local CPU identities.
///
/// Only set bits are emitted. For example, mask `0b0101` in group two gives
/// `(2, 0)` and `(2, 2)`. An empty mask yields an empty list. The original mask
/// is copied; clearing its lowest set bit only changes the local working value.
fn cpus_from_affinity(affinity: win_info::GROUP_AFFINITY) -> Vec<WindowsCpu> {
    let mut mask = affinity.Mask;
    let mut cpus = Vec::new();

    while mask != 0 {
        let index = mask.trailing_zeros() as u8;

        cpus.push(WindowsCpu {
            group: affinity.Group,
            index,
        });

        // Retirer le bit à 1 le plus à droite.
        mask &= mask - 1;
    }

    cpus
}

/// Owned intermediate memberships independent of the raw Windows allocation.
///
/// A CPU may appear once in each dimension: core, package and NUMA node. These
/// are complementary memberships, not duplicate CPU records. Lists preserve
/// entry order, with CPUs from each mask ordered by ascending bit position.
/// Parsing does not validate consistency across the three dimensions.
pub(super) struct ParsedTopology {
    /// One logical-CPU membership list for each physical core entry.
    cores: Vec<Vec<WindowsCpu>>,
    /// One membership list for each physical package entry.
    packages: Vec<Vec<WindowsCpu>>,
    /// Windows NUMA node numbers and their decoded logical-CPU memberships.
    numa_nodes: Vec<(NumaNodeId, Vec<WindowsCpu>)>,
}

/// Decodes core, package and NUMA memberships from a raw topology snapshot.
///
/// Consumes the buffer and copies the useful data into owned lists. Each
/// iteration checks the remaining header length, total entry size and fixed
/// payload size, then advances by the entry's reported size. Unknown or unused
/// relationships are skipped, allowing the `RelationAll` response to include
/// caches and other hardware descriptions.
///
/// Only the first group affinity of each entry is currently expanded. See the
/// module-level limits before using this parser on hardware spanning groups.
///
/// # Errors
///
/// Returns [`TopologyError::Windows`] with `ERROR_INVALID_DATA` for truncated
/// headers, sizes smaller than the header, entries extending past the buffer,
/// or payloads smaller than their fixed Windows representation.
pub(super) fn parse_buffer(buffer: ProcessorInfoBuffer) -> Result<ParsedTopology, TopologyError> {
    let bytes = buffer.len;
    let buffer_ptr = buffer.inner.as_ptr().cast::<u8>();
    let mut offset = 0usize;

    let mut packages: Vec<Vec<WindowsCpu>> = Vec::new();
    let mut cores: Vec<Vec<WindowsCpu>> = Vec::new();
    let mut numa_nodes: Vec<(crate::topology::NumaNodeId, Vec<WindowsCpu>)> = Vec::new();

    while offset < bytes as usize {
        if bytes as usize - offset < 8 {
            return Err(win_foundation::ERROR_INVALID_DATA.into());
        }

        let (relationship, size) = unsafe {
            let entry = buffer_ptr.add(offset);
            (
                entry.cast::<i32>().read_unaligned(),
                entry.add(4).cast::<u32>().read_unaligned() as usize,
            )
        };

        if size < 8 || size > bytes as usize - offset {
            return Err(win_foundation::ERROR_INVALID_DATA.into());
        }

        let entry = unsafe { buffer_ptr.add(offset) };

        match relationship {
            win_info::RelationProcessorCore => {
                let processor =
                    unsafe { read_payload::<win_info::PROCESSOR_RELATIONSHIP>(entry, size) }
                        .ok_or(win_foundation::ERROR_INVALID_DATA)?;

                let cpus = cpus_from_affinity(processor.GroupMask[0]);
                cores.push(cpus);
            }
            win_info::RelationProcessorPackage => {
                let processor =
                    unsafe { read_payload::<win_info::PROCESSOR_RELATIONSHIP>(entry, size) }
                        .ok_or(win_foundation::ERROR_INVALID_DATA)?;

                let cpus = cpus_from_affinity(processor.GroupMask[0]);
                packages.push(cpus);
            }
            win_info::RelationNumaNode | win_info::RelationNumaNodeEx => {
                let numa = unsafe { read_payload::<win_info::NUMA_NODE_RELATIONSHIP>(entry, size) }
                    .ok_or(win_foundation::ERROR_INVALID_DATA)?;

                let numa_id = numa.NodeNumber;
                let cpus = unsafe { cpus_from_affinity(numa.Anonymous.GroupMask) };
                numa_nodes.push((NumaNodeId::new(numa_id as usize), cpus));
            }
            _ => {} // Ignorer les caches et les autres relations.
        }

        // Passer à l’entrée suivante, selon sa taille réelle.
        offset += size;
    }
    Ok(ParsedTopology {
        cores,
        packages,
        numa_nodes,
    })
}

/// Encodes `(group, index)` using the library's `group * 64 + index` convention.
///
/// The index must be below 64; identities produced by [`cpus_from_affinity`]
/// satisfy that condition. The result is sparse across partially filled groups
/// and must not be used as an index into a densely packed CPU vector.
fn encode_cpu(cpu: WindowsCpu) -> CpuId {
    CpuId::new(usize::from(cpu.group) * 64 + usize::from(cpu.index))
}

/// Reverses [`encode_cpu`] for a valid Windows-encoded CPU identifier.
///
/// The caller must supply an ID from this encoding with a group representable
/// as `u16`. Arbitrary portable IDs are not validated: an oversized group would
/// be truncated by the cast. Decoding does not establish that a CPU is online.
fn decode_cpu(id: CpuId) -> WindowsCpu {
    WindowsCpu {
        group: (id.get() / 64) as u16,
        index: (id.get() % 64) as u8,
    }
}

/// Joins parsed memberships into the portable hardware-topology model.
///
/// For each core, its first CPU identifies the containing package. Every CPU
/// in that core receives the same [`CoreId`]; its NUMA node is looked up
/// independently. CPU IDs use [`encode_cpu`], package IDs use package-list
/// positions, and core IDs use core-list positions. Node IDs retain Windows'
/// `NodeNumber`. The inverse node-to-CPU lists are then copied into [`NumaNode`].
///
/// Membership searches currently choose the first match. This function does
/// not verify that all CPUs in a core belong to the selected package, reject
/// duplicate CPU records, validate uniqueness of NUMA membership, or sort the
/// result. Package/core list positions must fit in `i32` for their identifiers.
///
/// # Errors
///
/// Returns [`TopologyError::Windows`] with `ERROR_INVALID_DATA` if a core is
/// empty, its first CPU has no package, or a core CPU has no NUMA membership.
/// Empty inputs currently produce an empty topology rather than an error.
pub(super) fn discover_from(parsed: ParsedTopology) -> Result<Topology, TopologyError> {
    let mut logical_cpus = Vec::new();
    let mut numa_nodes = Vec::new();

    for (core_index, core_cpus) in parsed.cores.iter().enumerate() {
        let first_cpu = core_cpus
            .first()
            .ok_or(win_foundation::ERROR_INVALID_DATA)?;

        let package_index = parsed
            .packages
            .iter()
            .position(|package| package.contains(first_cpu))
            .ok_or(win_foundation::ERROR_INVALID_DATA)?;

        let core_id = CoreId::new(PackageId::new(package_index as i32), core_index as i32);

        for cpu in core_cpus {
            let numa_node = parsed
                .numa_nodes
                .iter()
                .find(|(_, cpus)| cpus.contains(cpu))
                .map(|(id, _)| *id)
                .ok_or(win_foundation::ERROR_INVALID_DATA)?;

            logical_cpus.push(LogicalCpu {
                id: encode_cpu(*cpu),
                core: core_id,
                numa_node,
            });
        }
    }
    let nodes = parsed.numa_nodes;

    for (id, cpus) in nodes {
        numa_nodes.push(NumaNode {
            id: id,
            cpus: cpus.iter().map(|c| encode_cpu(*c)).collect(),
        });
    }
    Ok(Topology {
        logical_cpus,
        numa_nodes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    // Build fixtures from bytes rather than copying structs with potentially
    // uninitialized padding. Storage has the same alignment as the API buffer.
    fn buffer_from_bytes(bytes: &[u8]) -> ProcessorInfoBuffer {
        let mut inner = Vec::new();
        for chunk in bytes.chunks(8) {
            let mut word = [0u8; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            inner.push(u64::from_ne_bytes(word));
        }
        ProcessorInfoBuffer {
            inner,
            len: bytes.len(),
        }
    }

    fn entry(relationship: i32, size: usize) -> Vec<u8> {
        let mut bytes = vec![0; size];
        bytes[..4].copy_from_slice(&relationship.to_ne_bytes());
        bytes[4..8].copy_from_slice(&(size as u32).to_ne_bytes());
        bytes
    }

    fn write_affinity(bytes: &mut [u8], offset: usize, group: u16, mask: usize) {
        let mask_offset = offset + offset_of!(win_info::GROUP_AFFINITY, Mask);
        bytes[mask_offset..mask_offset + size_of::<usize>()].copy_from_slice(&mask.to_ne_bytes());
        let group_offset = offset + offset_of!(win_info::GROUP_AFFINITY, Group);
        bytes[group_offset..group_offset + 2].copy_from_slice(&group.to_ne_bytes());
    }

    fn processor_entry(relationship: i32, group: u16, mask: usize) -> Vec<u8> {
        let mut bytes = entry(
            relationship,
            8 + size_of::<win_info::PROCESSOR_RELATIONSHIP>(),
        );
        let count = 8 + offset_of!(win_info::PROCESSOR_RELATIONSHIP, GroupCount);
        bytes[count..count + 2].copy_from_slice(&1u16.to_ne_bytes());
        let affinity = 8 + offset_of!(win_info::PROCESSOR_RELATIONSHIP, GroupMask);
        write_affinity(&mut bytes, affinity, group, mask);
        bytes
    }

    fn numa_entry(relationship: i32, node: u32, group: u16, mask: usize) -> Vec<u8> {
        let mut bytes = entry(
            relationship,
            8 + size_of::<win_info::NUMA_NODE_RELATIONSHIP>(),
        );
        let number = 8 + offset_of!(win_info::NUMA_NODE_RELATIONSHIP, NodeNumber);
        bytes[number..number + 4].copy_from_slice(&node.to_ne_bytes());
        let count = 8 + offset_of!(win_info::NUMA_NODE_RELATIONSHIP, GroupCount);
        bytes[count..count + 2].copy_from_slice(&1u16.to_ne_bytes());
        let affinity = 8 + offset_of!(win_info::NUMA_NODE_RELATIONSHIP, Anonymous);
        write_affinity(&mut bytes, affinity, group, mask);
        bytes
    }

    fn assert_invalid<T>(result: Result<T, TopologyError>) {
        assert!(matches!(result, Err(TopologyError::Windows { code })
            if code == win_foundation::ERROR_INVALID_DATA));
    }

    #[test]
    fn expands_sparse_masks_without_confusing_groups() {
        let affinity = win_info::GROUP_AFFINITY {
            Group: 3,
            Mask: 0b100101,
            ..Default::default()
        };
        assert_eq!(
            cpus_from_affinity(affinity),
            vec![
                WindowsCpu { group: 3, index: 0 },
                WindowsCpu { group: 3, index: 2 },
                WindowsCpu { group: 3, index: 5 },
            ]
        );
        assert!(cpus_from_affinity(win_info::GROUP_AFFINITY::default()).is_empty());
    }

    #[test]
    fn cpu_encoding_preserves_group_and_highest_bit() {
        for group in [0, 1, u16::MAX] {
            for index in [0, 3, 63] {
                let cpu = WindowsCpu { group, index };
                assert_eq!(decode_cpu(encode_cpu(cpu)), cpu);
            }
        }
        assert_ne!(
            encode_cpu(WindowsCpu { group: 0, index: 3 }),
            encode_cpu(WindowsCpu { group: 1, index: 3 })
        );
        #[cfg(target_pointer_width = "64")]
        assert_eq!(
            cpus_from_affinity(win_info::GROUP_AFFINITY {
                Group: 1,
                Mask: 1usize << 63,
                ..Default::default()
            }),
            vec![WindowsCpu {
                group: 1,
                index: 63
            }]
        );
    }

    #[test]
    fn discovers_smt_packages_and_independent_sparse_numa_nodes() {
        // NUMA node 4 spans two cores inside package 0. Package 0 also
        // contains node 9: NUMA IDs and package IDs must not be equated.
        let mut bytes = Vec::new();
        bytes.extend(numa_entry(win_info::RelationNumaNodeEx, 4, 0, 0b111));
        bytes.extend(processor_entry(win_info::RelationProcessorCore, 0, 0b11));
        bytes.extend(processor_entry(
            win_info::RelationProcessorPackage,
            0,
            0b10111,
        ));
        bytes.extend(numa_entry(win_info::RelationNumaNode, 9, 0, 0b10000));
        bytes.extend(processor_entry(win_info::RelationProcessorCore, 0, 0b100));
        bytes.extend(processor_entry(win_info::RelationProcessorCore, 0, 0b10000));
        bytes.extend(processor_entry(win_info::RelationProcessorPackage, 1, 0b1));
        bytes.extend(processor_entry(win_info::RelationProcessorCore, 1, 0b1));
        bytes.extend(numa_entry(win_info::RelationNumaNodeEx, 12, 1, 0b1));

        let parsed = parse_buffer(buffer_from_bytes(&bytes)).unwrap();
        let topology = discover_from(parsed).unwrap();
        let cpus = topology.logical_cpus();
        assert_eq!(
            cpus.iter().map(|cpu| cpu.id().get()).collect::<Vec<_>>(),
            vec![0, 1, 2, 4, 64]
        );
        assert_eq!(topology.physical_core_count(), 4);
        assert_eq!(topology.package_count(), 2);
        assert_eq!(topology.numa_node_count(), 3);
        assert_eq!(cpus[0].core(), cpus[1].core());
        assert_ne!(cpus[1].core(), cpus[2].core());
        assert_eq!(cpus[0].package(), cpus[3].package());
        assert_ne!(cpus[3].package(), cpus[4].package());
        assert_eq!(
            cpus.iter()
                .map(|cpu| cpu.numa_node().get())
                .collect::<Vec<_>>(),
            vec![4, 4, 4, 9, 12]
        );
        assert_eq!(
            topology.numa_nodes()[0].cpus(),
            &[CpuId::new(0), CpuId::new(1), CpuId::new(2)]
        );
        assert_eq!(topology.numa_nodes()[2].cpus(), &[CpuId::new(64)]);
    }

    #[test]
    fn skips_unknown_variable_sized_entries_and_reads_unaligned_next_header() {
        let mut bytes = entry(0x1234, 13);
        bytes.extend(processor_entry(win_info::RelationProcessorCore, 2, 0b100));
        let parsed = parse_buffer(buffer_from_bytes(&bytes)).unwrap();
        assert_eq!(parsed.cores, vec![vec![WindowsCpu { group: 2, index: 2 }]]);
        assert!(parsed.packages.is_empty());
        assert!(parsed.numa_nodes.is_empty());
    }

    #[test]
    fn rejects_truncated_headers_and_invalid_entry_sizes() {
        for len in 1..8 {
            assert_invalid(parse_buffer(buffer_from_bytes(&vec![0; len])));
        }
        for size in [0u32, 7, 9, u32::MAX] {
            let mut bytes = entry(win_info::RelationProcessorCore, 8);
            bytes[4..8].copy_from_slice(&size.to_ne_bytes());
            assert_invalid(parse_buffer(buffer_from_bytes(&bytes)));
        }
    }

    #[test]
    fn rejects_truncated_payloads_for_each_supported_relationship() {
        for relationship in [
            win_info::RelationProcessorCore,
            win_info::RelationProcessorPackage,
            win_info::RelationNumaNode,
            win_info::RelationNumaNodeEx,
        ] {
            assert_invalid(parse_buffer(buffer_from_bytes(&entry(relationship, 8))));
        }
    }

    #[test]
    fn rejects_empty_cores_and_missing_required_memberships() {
        let cpu = WindowsCpu { group: 0, index: 0 };
        assert_invalid(discover_from(ParsedTopology {
            cores: vec![vec![]],
            packages: vec![],
            numa_nodes: vec![],
        }));
        assert_invalid(discover_from(ParsedTopology {
            cores: vec![vec![cpu]],
            packages: vec![],
            numa_nodes: vec![(NumaNodeId::new(0), vec![cpu])],
        }));
        assert_invalid(discover_from(ParsedTopology {
            cores: vec![vec![cpu]],
            packages: vec![vec![cpu]],
            numa_nodes: vec![],
        }));
    }

    #[test]
    fn queries_real_windows_topology_without_assuming_hardware_counts() {
        let topology = Topology::discover().expect("Windows topology discovery failed");
        assert!(!topology.logical_cpus().is_empty());
        assert!(topology.physical_core_count() > 0);
        assert!(topology.package_count() > 0);
        assert!(!topology.numa_nodes().is_empty());
        let mut ids = std::collections::HashSet::new();
        for cpu in topology.logical_cpus() {
            assert!(ids.insert(cpu.id()), "duplicate logical CPU");
            let node = topology
                .numa_nodes()
                .iter()
                .find(|node| node.id() == cpu.numa_node())
                .expect("missing NUMA node");
            assert!(node.cpus().contains(&cpu.id()));
        }
        for node in topology.numa_nodes() {
            for id in node.cpus() {
                assert!(
                    topology
                        .logical_cpus()
                        .iter()
                        .any(|cpu| cpu.id() == *id && cpu.numa_node() == node.id())
                );
            }
        }
    }
}
