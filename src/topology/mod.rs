//! Passive hardware-topology data.
//!
//! This module describes relationships reported by the operating system. It
//! deliberately does not decide how many workers to create or where work and
//! data should be placed.

use std::collections::HashSet;

mod linux;

/// Operating-system identifier of a logical CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CpuId(usize);

impl CpuId {
    /// Creates an identifier from its operating-system value.
    pub const fn new(id: usize) -> Self {
        Self(id)
    }

    /// Returns the operating-system value.
    pub const fn get(self) -> usize {
        self.0
    }
}

/// Operating-system identifier of a CPU package or socket.
///
/// The value remains signed because Linux may report a negative identifier
/// when the platform cannot provide package information.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackageId(i32);

impl PackageId {
    /// Creates an identifier from its operating-system value.
    pub const fn new(id: i32) -> Self {
        Self(id)
    }

    /// Returns the operating-system value.
    pub const fn get(self) -> i32 {
        self.0
    }
}

/// Identity of a physical core within a package.
///
/// Linux core numbers are not necessarily unique across packages, so both
/// components participate in equality and hashing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CoreId {
    package: PackageId,
    id: i32,
}

impl CoreId {
    /// Creates a physical-core identity.
    pub const fn new(package: PackageId, id: i32) -> Self {
        Self { package, id }
    }

    /// Returns the package containing this core.
    pub const fn package(self) -> PackageId {
        self.package
    }

    /// Returns the core number reported within the package.
    pub const fn get(self) -> i32 {
        self.id
    }
}

/// Operating-system identifier of a NUMA node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NumaNodeId(usize);

impl NumaNodeId {
    /// Creates an identifier from its operating-system value.
    pub const fn new(id: usize) -> Self {
        Self(id)
    }

    /// Returns the operating-system value.
    pub const fn get(self) -> usize {
        self.0
    }
}

/// A logical CPU and its placement in the hardware topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogicalCpu {
    id: CpuId,
    core: CoreId,
    numa_node: NumaNodeId,
}

impl LogicalCpu {
    /// Returns the logical CPU identifier.
    pub const fn id(&self) -> CpuId {
        self.id
    }

    /// Returns the physical core containing this logical CPU.
    pub const fn core(&self) -> CoreId {
        self.core
    }

    /// Returns the package containing this logical CPU.
    pub const fn package(&self) -> PackageId {
        self.core.package()
    }

    /// Returns the NUMA node containing this logical CPU.
    pub const fn numa_node(&self) -> NumaNodeId {
        self.numa_node
    }
}

/// A NUMA node and its online logical CPUs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumaNode {
    id: NumaNodeId,
    cpus: Vec<CpuId>,
}

impl NumaNode {
    /// Returns the node identifier.
    pub const fn id(&self) -> NumaNodeId {
        self.id
    }

    /// Returns the online logical CPUs belonging to this node.
    pub fn cpus(&self) -> &[CpuId] {
        &self.cpus
    }
}

/// Passive description of logical CPUs, physical cores, packages and NUMA nodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Topology {
    logical_cpus: Vec<LogicalCpu>,
    numa_nodes: Vec<NumaNode>,
}

impl Topology {
    /// Returns all online logical CPUs in ascending identifier order.
    pub fn logical_cpus(&self) -> &[LogicalCpu] {
        &self.logical_cpus
    }

    /// Returns all NUMA nodes in ascending identifier order.
    pub fn numa_nodes(&self) -> &[NumaNode] {
        &self.numa_nodes
    }

    /// Returns the number of online logical CPUs.
    pub fn logical_cpu_count(&self) -> usize {
        self.logical_cpus.len()
    }

    /// Returns the number of distinct physical cores.
    pub fn physical_core_count(&self) -> usize {
        self.logical_cpus
            .iter()
            .map(LogicalCpu::core)
            .collect::<HashSet<_>>()
            .len()
    }

    /// Returns the number of distinct packages.
    pub fn package_count(&self) -> usize {
        self.logical_cpus
            .iter()
            .map(LogicalCpu::package)
            .collect::<HashSet<_>>()
            .len()
    }

    /// Returns the number of NUMA nodes.
    pub fn numa_node_count(&self) -> usize {
        self.numa_nodes.len()
    }
}

#[derive(Debug)]
pub enum TopologyError {
    InvalidCpuList {
        input: String,
        reason: &'static str,
    },
    Io {
        path: std::path::PathBuf,
        error: std::io::Error,
    },
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_distinguish_smt_cores_and_packages() {
        let package_0 = PackageId::new(0);
        let package_1 = PackageId::new(1);
        let node_0 = NumaNodeId::new(0);
        let node_1 = NumaNodeId::new(1);
        let topology = Topology {
            logical_cpus: vec![
                LogicalCpu {
                    id: CpuId::new(0),
                    core: CoreId::new(package_0, 0),
                    numa_node: node_0,
                },
                LogicalCpu {
                    id: CpuId::new(8),
                    core: CoreId::new(package_0, 0),
                    numa_node: node_0,
                },
                LogicalCpu {
                    id: CpuId::new(2),
                    core: CoreId::new(package_0, 1),
                    numa_node: node_0,
                },
                LogicalCpu {
                    id: CpuId::new(7),
                    core: CoreId::new(package_1, 0),
                    numa_node: node_1,
                },
            ],
            numa_nodes: vec![
                NumaNode {
                    id: node_0,
                    cpus: vec![CpuId::new(0), CpuId::new(2), CpuId::new(8)],
                },
                NumaNode {
                    id: node_1,
                    cpus: vec![CpuId::new(7)],
                },
            ],
        };

        assert_eq!(topology.logical_cpu_count(), 4);
        assert_eq!(topology.physical_core_count(), 3);
        assert_eq!(topology.package_count(), 2);
        assert_eq!(topology.numa_node_count(), 2);
        assert_eq!(topology.logical_cpus[0].package(), package_0);
        assert_eq!(
            topology.numa_nodes[0].cpus(),
            &[CpuId::new(0), CpuId::new(2), CpuId::new(8)]
        );
    }
}
