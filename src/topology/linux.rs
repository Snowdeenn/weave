//! Linux hardware-topology discovery through sysfs.
//!
//! The backend reads kernel-provided text files below `/sys/devices/system`:
//!
//! - `cpu/online` identifies the logical CPUs currently online;
//! - `cpu/cpuX/topology/core_id` identifies a CPU's physical core;
//! - `cpu/cpuX/topology/physical_package_id` identifies its package or socket;
//! - `node/online` identifies the online NUMA nodes;
//! - `node/nodeX/cpulist` associates logical CPUs with a NUMA node.
//!
//! Discovery is deliberately split into small operations. File-reading
//! functions preserve the failing path, parsers validate the Linux list
//! syntax, and [`build_numa_layout`] checks cross-file invariants. The final
//! [`discover_from`] function only orchestrates those stages. Accepting a
//! sysfs root as an argument keeps the backend deterministic and testable with
//! synthetic directory trees.
//!
//! This module describes what Linux reports. It does not select worker counts,
//! set CPU affinity, allocate NUMA-local memory, or choose stealing policies.

use std::vec;

use super::*;

/// Parses Linux's compact list syntax into sorted, unique numeric identifiers.
///
/// A list contains comma-separated identifiers and inclusive ranges, such as
/// `0-3,8,10-11`. Descending ranges, duplicate identifiers, empty elements and
/// malformed integers are rejected rather than silently normalized.
fn parse_id_list(input: &str) -> Result<Vec<usize>, TopologyError> {
    let mut ids = Vec::new();
    for element in input.trim().split(',') {
        if element.contains('-') {
            let mut parts = element.split('-');
            let Ok(start) = parts.next().unwrap().parse::<usize>() else {
                return Err(TopologyError::InvalidIdList {
                    input: element.to_string(),
                    reason: "Impossible de parse l'id cpu",
                });
            };
            let Ok(end) = parts.next().unwrap().parse::<usize>() else {
                return Err(TopologyError::InvalidIdList {
                    input: element.to_string(),
                    reason: "Impossible de parse l'id cpu",
                });
            };
            if parts.next().is_some() {
                return Err(TopologyError::InvalidIdList {
                    input: element.to_string(),
                    reason: "La range de cpu id devrait avoir que 2 borne",
                });
            }
            if start <= end {
                for id in start..=end {
                    ids.push(id);
                }
            } else {
                return Err(TopologyError::InvalidIdList {
                    input: element.to_string(),
                    reason: "La fin de la range des cpu est inférieur au début: start > end",
                });
            }
        } else {
            let Ok(id) = element.parse::<usize>() else {
                return Err(TopologyError::InvalidIdList {
                    input: element.to_string(),
                    reason: "Impossible de parse l'id du cpu",
                });
            };
            ids.push(id);
        }
    }

    ids.sort_unstable_by_key(|id| *id);
    if ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(TopologyError::InvalidIdList {
            input: input.to_string(),
            reason: "Les doublons de cpu id ne sont pas accepter",
        });
    }
    Ok(ids)
}

/// Parses a Linux list as strongly typed logical-CPU identifiers.
pub(super) fn parse_cpu_list(input: &str) -> Result<Vec<CpuId>, TopologyError> {
    let ids = parse_id_list(input)?;
    Ok(ids.into_iter().map(CpuId::new).collect())
}

/// Parses a Linux list as strongly typed NUMA-node identifiers.
pub(super) fn parse_numa_node_list(input: &str) -> Result<Vec<NumaNodeId>, TopologyError> {
    let ids = parse_id_list(input)?;
    Ok(ids.into_iter().map(NumaNodeId::new).collect())
}

/// Reads the logical CPUs reported online by `cpu/online`.
///
/// Online CPUs are used instead of assuming that every `cpuX` directory is
/// currently usable. CPU identifiers may be sparse and are returned sorted.
pub(super) fn read_online_cpus(sysfs_root: &std::path::Path) -> Result<Vec<CpuId>, TopologyError> {
    let path = sysfs_root.join("cpu/online");
    let file = match std::fs::read_to_string(&path) {
        Ok(f) => f,
        Err(e) => {
            return Err(TopologyError::Io { path, error: e });
        }
    };
    let cpus = parse_cpu_list(&file)?;
    Ok(cpus)
}

/// Reads the NUMA nodes reported online by `node/online`.
///
/// Absence of this file is interpreted by [`discover_from`] as an UMA system;
/// other I/O errors remain failures and are not hidden by that fallback.
pub(super) fn read_online_numa_nodes(
    sysfs_root: &std::path::Path,
) -> Result<Vec<NumaNodeId>, TopologyError> {
    let path = sysfs_root.join("node/online");
    let file = match std::fs::read_to_string(&path) {
        Ok(f) => f,
        Err(e) => {
            return Err(TopologyError::Io { path, error: e });
        }
    };
    let node = parse_numa_node_list(&file)?;
    Ok(node)
}

/// Reads one signed integer from a sysfs topology file.
///
/// Core and package identifiers remain signed so the representation does not
/// silently reinterpret a negative kernel value as a very large unsigned one.
fn read_i32(path: &std::path::Path) -> Result<i32, TopologyError> {
    let file = match std::fs::read_to_string(path) {
        Ok(f) => f,
        Err(e) => {
            return Err(TopologyError::Io {
                path: path.into(),
                error: e,
            });
        }
    };
    match file.trim().parse::<i32>() {
        Ok(i) => Ok(i),
        Err(e) => Err(TopologyError::InvalidInteger {
            path: path.into(),
            value: e,
        }),
    }
}

/// Reads the physical-core identity for one logical CPU.
///
/// Linux `core_id` values are not necessarily globally unique. The result
/// therefore combines `core_id` with `physical_package_id` in a [`CoreId`].
pub(super) fn read_core_id(
    sysfs_root: &std::path::Path,
    cpu: CpuId,
) -> Result<CoreId, TopologyError> {
    let topology_dir = sysfs_root
        .join("cpu")
        .join(format!("cpu{}", cpu.get()))
        .join("topology");

    let core_path = topology_dir.join("core_id");
    let package_path = topology_dir.join("physical_package_id");

    let core_id = read_i32(&core_path)?;
    let package_id = read_i32(&package_path)?;

    Ok(CoreId::new(PackageId::new(package_id), core_id))
}

/// Reads the logical CPUs associated with one NUMA node.
///
/// This function reports the raw `nodeX/cpulist` membership. Filtering against
/// the online CPU set and validating uniqueness are responsibilities of
/// [`build_numa_layout`].
pub(super) fn read_numa_node_cpus(
    sysfs_root: &std::path::Path,
    node: NumaNodeId,
) -> Result<Vec<CpuId>, TopologyError> {
    let target_path = sysfs_root
        .join("node")
        .join(format!("node{}", node.get()))
        .join("cpulist");
    let file = match std::fs::read_to_string(&target_path) {
        Ok(f) => f,
        Err(e) => {
            return Err(TopologyError::Io {
                path: target_path,
                error: e,
            });
        }
    };

    let cpus = parse_cpu_list(&file)?;
    Ok(cpus)
}

/// Validates NUMA memberships and constructs both directions of the relation.
///
/// CPUs not present in `online_cpus` are discarded because a node's `cpulist`
/// may contain CPUs that are currently offline. Every online CPU must then
/// occur in exactly one node. The returned vector represents `node -> CPUs`,
/// while the map supports efficient `CPU -> node` lookup during construction
/// of [`LogicalCpu`] values.
///
/// Nodes and their CPU lists are sorted to keep debug output and tests stable.
pub(super) fn build_numa_layout(
    online_cpus: &[CpuId],
    memberships: Vec<(NumaNodeId, Vec<CpuId>)>,
) -> Result<(Vec<NumaNode>, std::collections::HashMap<CpuId, NumaNodeId>), TopologyError> {
    let online_cpus = online_cpus
        .iter()
        .copied()
        .collect::<std::collections::HashSet<CpuId>>();
    let mut numa_nodes = Vec::new();
    let mut cpu_to_node = std::collections::HashMap::new();

    for (numa_node_id, mut numa_cpus) in memberships {
        numa_cpus.retain(|id| online_cpus.contains(id));
        numa_cpus.sort_unstable();

        for cpu in numa_cpus.iter() {
            if let Some(first_node) = cpu_to_node.insert(*cpu, numa_node_id) {
                return Err(TopologyError::CpuInMultipleNumaNodes {
                    cpu: *cpu,
                    first: first_node,
                    second: numa_node_id,
                });
            }
        }

        numa_nodes.push(NumaNode {
            id: numa_node_id,
            cpus: numa_cpus,
        });
    }

    for cpu in online_cpus {
        if !cpu_to_node.contains_key(&cpu) {
            return Err(TopologyError::CpuWithoutNumaNode { cpu });
        }
    }

    numa_nodes.sort_unstable_by_key(|node| node.id().get());

    Ok((numa_nodes, cpu_to_node))
}

/// Discovers a complete Linux topology below an explicit sysfs root.
///
/// The explicit root separates filesystem access from the public platform
/// entry point and allows tests to provide synthetic sysfs trees. If
/// `node/online` does not exist, all online CPUs are assigned to a synthetic
/// node zero, representing a single uniform memory domain. Other NUMA read
/// errors are propagated.
///
/// The returned [`Topology`] contains only online logical CPUs. Physical cores
/// are identified by `(package_id, core_id)`, so SMT siblings share a core
/// identity without being collapsed into one logical CPU.
pub(super) fn discover_from(sysfs_root: &std::path::Path) -> Result<Topology, TopologyError> {
    let online_cpus = read_online_cpus(sysfs_root)?;

    let memberships = match read_online_numa_nodes(sysfs_root) {
        Ok(node_ids) => {
            let mut m = Vec::new();
            for numa_node in &node_ids {
                let cpus = read_numa_node_cpus(sysfs_root, *numa_node)?;
                m.push((*numa_node, cpus));
            }
            m
        }
        Err(TopologyError::Io { error, .. }) if error.kind() == std::io::ErrorKind::NotFound => {
            vec![(NumaNodeId::new(0), online_cpus.clone())]
        }
        Err(e) => return Err(e),
    };

    let (numa_nodes, cpu_to_node) = build_numa_layout(&online_cpus, memberships)?;

    let mut logical_cpus = Vec::new();

    for cpu in online_cpus {
        let core = read_core_id(sysfs_root, cpu)?;
        let Some(numa_node) = cpu_to_node.get(&cpu) else {
            return Err(TopologyError::CpuWithoutNumaNode { cpu });
        };

        logical_cpus.push(LogicalCpu {
            id: cpu,
            core,
            numa_node: *numa_node,
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
    use std::{
        fs,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    struct FakeSysfs {
        root: PathBuf,
    }

    impl FakeSysfs {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir()
                .join(format!("weave-topology-{}-{unique}", std::process::id()));
            fs::create_dir_all(root.join("cpu")).unwrap();
            Self { root }
        }

        fn root(&self) -> &Path {
            &self.root
        }

        fn write_numa_cpulist(&self, node: NumaNodeId, contents: &str) {
            let node_dir = self.root.join("node").join(format!("node{}", node.get()));
            fs::create_dir_all(&node_dir).unwrap();
            fs::write(node_dir.join("cpulist"), contents).unwrap();
        }

        fn write_cpu_topology(&self, cpu: CpuId, core: &str, package: &str) {
            let topology_dir = self
                .root
                .join("cpu")
                .join(format!("cpu{}", cpu.get()))
                .join("topology");
            fs::create_dir_all(&topology_dir).unwrap();
            fs::write(topology_dir.join("core_id"), core).unwrap();
            fs::write(topology_dir.join("physical_package_id"), package).unwrap();
        }
    }

    impl Drop for FakeSysfs {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn ids(values: &[usize]) -> Vec<CpuId> {
        values.iter().copied().map(CpuId::new).collect()
    }

    fn node_ids(values: &[usize]) -> Vec<NumaNodeId> {
        values.iter().copied().map(NumaNodeId::new).collect()
    }

    #[test]
    fn parses_one_cpu() {
        assert_eq!(parse_cpu_list("0").unwrap(), ids(&[0]));
    }

    #[test]
    fn parses_non_contiguous_cpu_ids() {
        assert_eq!(parse_cpu_list("0,2,7").unwrap(), ids(&[0, 2, 7]));
    }

    #[test]
    fn parses_an_inclusive_range() {
        assert_eq!(parse_cpu_list("0-3").unwrap(), ids(&[0, 1, 2, 3]));
    }

    #[test]
    fn parses_mixed_ids_ranges_and_trailing_newline() {
        assert_eq!(
            parse_cpu_list("0-3,8,10-11\n").unwrap(),
            ids(&[0, 1, 2, 3, 8, 10, 11])
        );
    }

    #[test]
    fn returns_cpu_ids_in_ascending_order() {
        assert_eq!(parse_cpu_list("7,0,4-5").unwrap(), ids(&[0, 4, 5, 7]));
    }

    #[test]
    fn rejects_empty_input() {
        assert!(parse_cpu_list("").is_err());
    }

    #[test]
    fn rejects_non_numeric_cpu_ids() {
        assert!(parse_cpu_list("0,two,7").is_err());
    }

    #[test]
    fn rejects_descending_ranges() {
        assert!(parse_cpu_list("3-1").is_err());
    }

    #[test]
    fn rejects_ranges_with_too_many_bounds() {
        assert!(parse_cpu_list("1-2-3").is_err());
    }

    #[test]
    fn rejects_empty_list_elements() {
        assert!(parse_cpu_list("0,,2").is_err());
    }

    #[test]
    fn rejects_duplicate_cpu_ids() {
        assert!(parse_cpu_list("0-2,2").is_err());
    }

    #[test]
    fn reads_online_cpus_from_a_sysfs_root() {
        let sysfs = FakeSysfs::new();
        fs::write(sysfs.root().join("cpu/online"), "0-2,7\n").unwrap();

        assert_eq!(read_online_cpus(sysfs.root()).unwrap(), ids(&[0, 1, 2, 7]));
    }

    #[test]
    fn parses_non_contiguous_numa_node_ids_and_ranges() {
        assert_eq!(
            parse_numa_node_list("0-1,4\n").unwrap(),
            node_ids(&[0, 1, 4])
        );
    }

    #[test]
    fn reads_online_numa_nodes_from_a_sysfs_root() {
        let sysfs = FakeSysfs::new();
        fs::create_dir_all(sysfs.root().join("node")).unwrap();
        fs::write(sysfs.root().join("node/online"), "0-1,4\n").unwrap();

        assert_eq!(
            read_online_numa_nodes(sysfs.root()).unwrap(),
            node_ids(&[0, 1, 4])
        );
    }

    #[test]
    fn reports_a_missing_online_numa_nodes_file() {
        let sysfs = FakeSysfs::new();
        let expected_path = sysfs.root().join("node/online");

        let error = read_online_numa_nodes(sysfs.root()).unwrap_err();

        assert!(matches!(
            error,
            TopologyError::Io { path, .. } if path == expected_path
        ));
    }

    #[test]
    fn reads_cpus_belonging_to_a_numa_node() {
        let sysfs = FakeSysfs::new();
        sysfs.write_numa_cpulist(NumaNodeId::new(0), "0-3,8\n");

        assert_eq!(
            read_numa_node_cpus(sysfs.root(), NumaNodeId::new(0)).unwrap(),
            ids(&[0, 1, 2, 3, 8])
        );
    }

    #[test]
    fn reads_cpus_from_a_non_contiguous_numa_node_id() {
        let sysfs = FakeSysfs::new();
        sysfs.write_numa_cpulist(NumaNodeId::new(4), "2,7\n");

        assert_eq!(
            read_numa_node_cpus(sysfs.root(), NumaNodeId::new(4)).unwrap(),
            ids(&[2, 7])
        );
    }

    #[test]
    fn reports_a_missing_numa_node_cpulist() {
        let sysfs = FakeSysfs::new();
        let expected_path = sysfs.root().join("node/node3/cpulist");

        let error = read_numa_node_cpus(sysfs.root(), NumaNodeId::new(3)).unwrap_err();

        assert!(matches!(
            error,
            TopologyError::Io { path, .. } if path == expected_path
        ));
    }

    #[test]
    fn reports_an_invalid_numa_node_cpulist() {
        let sysfs = FakeSysfs::new();
        sysfs.write_numa_cpulist(NumaNodeId::new(2), "0,two,7\n");

        let error = read_numa_node_cpus(sysfs.root(), NumaNodeId::new(2)).unwrap_err();

        assert!(matches!(error, TopologyError::InvalidIdList { .. }));
    }

    #[test]
    fn reads_core_and_package_ids_for_a_cpu() {
        let sysfs = FakeSysfs::new();
        sysfs.write_cpu_topology(CpuId::new(7), "2\n", "1\n");

        assert_eq!(
            read_core_id(sysfs.root(), CpuId::new(7)).unwrap(),
            CoreId::new(PackageId::new(1), 2)
        );
    }

    #[test]
    fn same_core_number_in_different_packages_identifies_different_cores() {
        let sysfs = FakeSysfs::new();
        sysfs.write_cpu_topology(CpuId::new(0), "2\n", "0\n");
        sysfs.write_cpu_topology(CpuId::new(7), "2\n", "1\n");

        let first = read_core_id(sysfs.root(), CpuId::new(0)).unwrap();
        let second = read_core_id(sysfs.root(), CpuId::new(7)).unwrap();

        assert_ne!(first, second);
        assert_eq!(first, CoreId::new(PackageId::new(0), 2));
        assert_eq!(second, CoreId::new(PackageId::new(1), 2));
    }

    #[test]
    fn reports_a_missing_core_id_file() {
        let sysfs = FakeSysfs::new();
        let expected_path = sysfs.root().join("cpu/cpu4/topology/core_id");

        let error = read_core_id(sysfs.root(), CpuId::new(4)).unwrap_err();

        assert!(matches!(
            error,
            TopologyError::Io { path, .. } if path == expected_path
        ));
    }

    #[test]
    fn reports_a_non_numeric_core_id() {
        let sysfs = FakeSysfs::new();
        let cpu = CpuId::new(3);
        sysfs.write_cpu_topology(cpu, "not-a-core\n", "0\n");
        let expected_path = sysfs.root().join("cpu/cpu3/topology/core_id");

        let error = read_core_id(sysfs.root(), cpu).unwrap_err();

        assert!(matches!(
            error,
            TopologyError::InvalidInteger { path, .. } if path == expected_path
        ));
    }

    #[test]
    fn builds_a_sorted_numa_layout_for_online_cpus() {
        let online_cpus = ids(&[0, 2, 7]);
        let memberships = vec![
            (NumaNodeId::new(4), ids(&[7])),
            (NumaNodeId::new(0), ids(&[2, 0])),
        ];

        let (nodes, cpu_to_node) = build_numa_layout(&online_cpus, memberships).unwrap();

        assert_eq!(
            nodes,
            vec![
                NumaNode {
                    id: NumaNodeId::new(0),
                    cpus: ids(&[0, 2]),
                },
                NumaNode {
                    id: NumaNodeId::new(4),
                    cpus: ids(&[7]),
                },
            ]
        );
        assert_eq!(cpu_to_node.get(&CpuId::new(0)), Some(&NumaNodeId::new(0)));
        assert_eq!(cpu_to_node.get(&CpuId::new(2)), Some(&NumaNodeId::new(0)));
        assert_eq!(cpu_to_node.get(&CpuId::new(7)), Some(&NumaNodeId::new(4)));
    }

    #[test]
    fn ignores_offline_cpus_in_numa_memberships() {
        let online_cpus = ids(&[0, 2]);
        let memberships = vec![(NumaNodeId::new(0), ids(&[0, 1, 2, 3]))];

        let (nodes, cpu_to_node) = build_numa_layout(&online_cpus, memberships).unwrap();

        assert_eq!(nodes[0].cpus(), ids(&[0, 2]));
        assert!(!cpu_to_node.contains_key(&CpuId::new(1)));
        assert!(!cpu_to_node.contains_key(&CpuId::new(3)));
    }

    #[test]
    fn rejects_an_online_cpu_present_in_multiple_numa_nodes() {
        let online_cpus = ids(&[0, 2]);
        let memberships = vec![
            (NumaNodeId::new(0), ids(&[0, 2])),
            (NumaNodeId::new(1), ids(&[2])),
        ];

        let error = build_numa_layout(&online_cpus, memberships).unwrap_err();

        assert!(matches!(
            error,
            TopologyError::CpuInMultipleNumaNodes {
                cpu,
                first,
                second,
            } if cpu == CpuId::new(2)
                && first == NumaNodeId::new(0)
                && second == NumaNodeId::new(1)
        ));
    }

    #[test]
    fn rejects_an_online_cpu_without_a_numa_node() {
        let online_cpus = ids(&[0, 2, 7]);
        let memberships = vec![(NumaNodeId::new(0), ids(&[0, 2]))];

        let error = build_numa_layout(&online_cpus, memberships).unwrap_err();

        assert!(matches!(
            error,
            TopologyError::CpuWithoutNumaNode { cpu } if cpu == CpuId::new(7)
        ));
    }

    #[test]
    fn discovers_a_complete_topology_from_synthetic_sysfs() {
        let sysfs = FakeSysfs::new();
        fs::write(sysfs.root().join("cpu/online"), "0,2,7-8\n").unwrap();

        sysfs.write_cpu_topology(CpuId::new(0), "0\n", "0\n");
        sysfs.write_cpu_topology(CpuId::new(8), "0\n", "0\n");
        sysfs.write_cpu_topology(CpuId::new(2), "1\n", "0\n");
        sysfs.write_cpu_topology(CpuId::new(7), "0\n", "1\n");

        sysfs.write_numa_cpulist(NumaNodeId::new(0), "0,2,8,99\n");
        sysfs.write_numa_cpulist(NumaNodeId::new(4), "7\n");
        fs::write(sysfs.root().join("node/online"), "4,0\n").unwrap();

        let topology = discover_from(sysfs.root()).unwrap();

        assert_eq!(topology.logical_cpu_count(), 4);
        assert_eq!(topology.physical_core_count(), 3);
        assert_eq!(topology.package_count(), 2);
        assert_eq!(topology.numa_node_count(), 2);

        assert_eq!(
            topology
                .logical_cpus()
                .iter()
                .map(|cpu| (cpu.id(), cpu.core(), cpu.numa_node()))
                .collect::<Vec<_>>(),
            vec![
                (
                    CpuId::new(0),
                    CoreId::new(PackageId::new(0), 0),
                    NumaNodeId::new(0),
                ),
                (
                    CpuId::new(2),
                    CoreId::new(PackageId::new(0), 1),
                    NumaNodeId::new(0),
                ),
                (
                    CpuId::new(7),
                    CoreId::new(PackageId::new(1), 0),
                    NumaNodeId::new(4),
                ),
                (
                    CpuId::new(8),
                    CoreId::new(PackageId::new(0), 0),
                    NumaNodeId::new(0),
                ),
            ]
        );
        assert_eq!(topology.numa_nodes()[0].id(), NumaNodeId::new(0));
        assert_eq!(topology.numa_nodes()[0].cpus(), ids(&[0, 2, 8]));
        assert_eq!(topology.numa_nodes()[1].id(), NumaNodeId::new(4));
        assert_eq!(topology.numa_nodes()[1].cpus(), ids(&[7]));
    }

    #[test]
    fn discovers_a_synthetic_numa_node_when_node_online_is_absent() {
        let sysfs = FakeSysfs::new();
        fs::write(sysfs.root().join("cpu/online"), "0,2\n").unwrap();
        sysfs.write_cpu_topology(CpuId::new(0), "0\n", "0\n");
        sysfs.write_cpu_topology(CpuId::new(2), "1\n", "0\n");

        let topology = discover_from(sysfs.root()).unwrap();

        assert_eq!(topology.logical_cpu_count(), 2);
        assert_eq!(topology.numa_node_count(), 1);
        assert_eq!(topology.numa_nodes()[0].id(), NumaNodeId::new(0));
        assert_eq!(topology.numa_nodes()[0].cpus(), ids(&[0, 2]));
        assert!(
            topology
                .logical_cpus()
                .iter()
                .all(|cpu| cpu.numa_node() == NumaNodeId::new(0))
        );
    }

    #[test]
    fn does_not_use_the_uma_fallback_for_other_node_online_errors() {
        let sysfs = FakeSysfs::new();
        fs::write(sysfs.root().join("cpu/online"), "0\n").unwrap();
        sysfs.write_cpu_topology(CpuId::new(0), "0\n", "0\n");
        fs::create_dir_all(sysfs.root().join("node/online")).unwrap();
        let expected_path = sysfs.root().join("node/online");

        let error = discover_from(sysfs.root()).unwrap_err();

        assert!(matches!(
            error,
            TopologyError::Io { path, error }
                if path == expected_path
                    && error.kind() != std::io::ErrorKind::NotFound
        ));
    }
}
