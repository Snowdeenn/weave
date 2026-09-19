//! Passive hardware-topology discovery and representation.
//!
//! A machine has several simultaneous topology dimensions. A logical CPU
//! belongs to a physical core, that core belongs to a package or socket, and
//! the logical CPU also belongs to a NUMA memory domain. These relationships
//! must not be inferred from numeric proximity: identifiers may be sparse,
//! physical-core numbers may repeat across packages, and a package is not
//! necessarily equivalent to one NUMA node.
//!
//! [`Topology`](crate::topology::Topology) records these operating-system
//! relationships without making runtime policy decisions. In particular, it
//! does not choose a worker count, pin threads, place memory, or define a
//! work-stealing order. Those decisions belong to later worker-layout and
//! scheduler layers.
//!
//! Discovery currently uses Linux sysfs and includes only online logical CPUs.
//! On a Linux system without an exposed NUMA interface, discovery represents
//! the uniform memory domain as one synthetic node with identifier zero. Other
//! platforms return
//! [`TopologyError::UnsupportedPlatform`](crate::topology::TopologyError::UnsupportedPlatform).
//!
//! # Example
//!
//! ```no_run
//! use weave::topology::Topology;
//!
//! let topology = Topology::discover().expect("hardware topology discovery failed");
//! println!("{topology:#?}");
//! println!("logical CPUs: {}", topology.logical_cpu_count());
//! println!("physical cores: {}", topology.physical_core_count());
//! println!("packages: {}", topology.package_count());
//! println!("NUMA nodes: {}", topology.numa_node_count());
//! ```

use std::collections::HashSet;

#[cfg(target_os = "linux")]
mod linux;

/// Operating-system identifier of a logical CPU.
///
/// A logical CPU is an execution context visible to the operating system. Two
/// logical CPUs may be SMT siblings sharing the same [`CoreId`], so this type
/// must not be interpreted as a physical-core identifier.
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

/// Operating-system identifier of a NUMA memory node.
///
/// Node identifiers may be sparse and have no implied relationship with
/// package identifiers.
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
///
/// The value exposes both the physical-core relation and the NUMA relation.
/// Keeping them independent avoids the incorrect assumption that sockets and
/// NUMA nodes always have a one-to-one correspondence.
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

/// A NUMA memory domain and its online logical CPUs.
///
/// CPU identifiers are stored in ascending order. The type describes
/// membership only; it does not expose allocation or memory-affinity policy.
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
///
/// Logical CPUs are the primary records. Counts of physical cores and packages
/// are derived from their identities, while NUMA nodes retain the inverse
/// node-to-CPU relation useful for inspection and future worker layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Topology {
    logical_cpus: Vec<LogicalCpu>,
    numa_nodes: Vec<NumaNode>,
}

impl Topology {
    /// Discovers the hardware topology reported by the operating system.
    ///
    /// On Linux this reads sysfs below `/sys/devices/system`. Only CPUs reported
    /// in `cpu/online` are included. A missing NUMA interface is represented as
    /// one synthetic node rather than treated as a discovery failure.
    ///
    /// # Errors
    ///
    /// Returns [`TopologyError`] when the platform is unsupported, a required
    /// topology file cannot be read or parsed, or the reported CPU-to-NUMA
    /// memberships are inconsistent.
    pub fn discover() -> Result<Self, TopologyError> {
        #[cfg(target_os = "linux")]
        {
            let sysfs_root = std::path::Path::new("/sys/devices/system");
            linux::discover_from(sysfs_root)
        }

        #[cfg(not(target_os = "linux"))]
        {
            Err(TopologyError::UnsupportedPlatform)
        }
    }
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
/// Failure while reading, parsing, or validating hardware-topology data.
///
/// Discovery treats operating-system topology information as untrusted input:
/// files may disappear during CPU hotplug, contain identifiers that cannot be
/// parsed, or describe inconsistent CPU-to-NUMA-node relationships. These
/// errors preserve enough context to identify the failing file or relation.
#[derive(Debug)]
pub enum TopologyError {
    /// Hardware-topology discovery is not implemented for the current platform.
    UnsupportedPlatform,
    /// A Linux list of operating-system identifiers is malformed.
    ///
    /// This includes invalid integers, descending ranges, duplicate identifiers,
    /// and ranges with a number of bounds other than two.
    InvalidIdList {
        /// The complete list, or malformed list element, that could not be parsed.
        input: String,
        /// Human-readable explanation of the violated list rule.
        reason: &'static str,
    },
    /// A topology file could not be read.
    Io {
        /// Exact sysfs path whose read operation failed.
        path: std::path::PathBuf,
        /// Original operating-system I/O error.
        error: std::io::Error,
    },
    /// A topology file expected to contain one signed integer was malformed.
    InvalidInteger {
        /// Exact sysfs path containing the malformed value.
        path: std::path::PathBuf,
        /// Original integer parsing error.
        value: std::num::ParseIntError,
    },
    /// One online logical CPU was associated with more than one NUMA node.
    ///
    /// A valid topology requires every online CPU to have exactly one NUMA
    /// association. Accepting both nodes would make future scheduling and data
    /// placement decisions ambiguous.
    CpuInMultipleNumaNodes {
        /// Logical CPU with conflicting NUMA memberships.
        cpu: CpuId,
        /// NUMA node encountered first while constructing the topology.
        first: NumaNodeId,
        /// Later NUMA node that reported the same logical CPU.
        second: NumaNodeId,
    },
    /// An online logical CPU was not associated with any discovered NUMA node.
    ///
    /// On systems without a NUMA sysfs interface, discovery creates one
    /// synthetic node containing every online CPU. This error therefore denotes
    /// an inconsistent exposed NUMA topology rather than an ordinary UMA host.
    CpuWithoutNumaNode {
        /// Online logical CPU for which no NUMA membership was found.
        cpu: CpuId,
    },
}

impl std::fmt::Display for TopologyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedPlatform => {
                write!(
                    f,
                    "hardware topology discovery is unsupported on this platform"
                )
            }
            Self::InvalidIdList { input, reason } => {
                write!(
                    f,
                    "invalid operating-system identifier list {input:?}: {reason}"
                )
            }
            Self::Io { path, error } => {
                write!(f, "cannot read topology file {}: {error}", path.display())
            }
            Self::InvalidInteger { path, value } => write!(
                f,
                "invalid integer in topology file {}: {value}",
                path.display()
            ),
            Self::CpuInMultipleNumaNodes { cpu, first, second } => write!(
                f,
                "online CPU {} belongs to both NUMA node {} and NUMA node {}",
                cpu.get(),
                first.get(),
                second.get()
            ),
            Self::CpuWithoutNumaNode { cpu } => {
                write!(
                    f,
                    "online CPU {} does not belong to any NUMA node",
                    cpu.get()
                )
            }
        }
    }
}

impl std::error::Error for TopologyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { error, .. } => Some(error),
            Self::InvalidInteger { value, .. } => Some(value),
            _ => None,
        }
    }
}

#[cfg(test)]
pub(crate) fn topology_fixture(entries: &[(CpuId, CoreId, NumaNodeId)]) -> Topology {
    let logical_cpus = entries
        .iter()
        .map(|&(id, core, numa_node)| LogicalCpu {
            id,
            core,
            numa_node,
        })
        .collect::<Vec<_>>();

    let mut cpus_by_node = std::collections::BTreeMap::<NumaNodeId, Vec<CpuId>>::new();
    for &(cpu, _, node) in entries {
        cpus_by_node.entry(node).or_default().push(cpu);
    }
    let numa_nodes = cpus_by_node
        .into_iter()
        .map(|(id, mut cpus)| {
            cpus.sort_unstable();
            NumaNode { id, cpus }
        })
        .collect();

    Topology {
        logical_cpus,
        numa_nodes,
    }
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
