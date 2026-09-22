//! Hardware-placement plans for Weave workers.
//!
//! A [`Topology`](crate::topology::Topology) is a passive description of the
//! machine: it reports which logical CPUs share a physical core, package, or
//! NUMA node. A [`WorkerLayout`] is the next layer in the architecture: it
//! turns those facts into an explicit decision about how many workers should
//! exist and which logical CPU each worker is intended to use.
//!
//! A layout is only a plan. Constructing one does not create threads or ask the
//! operating system to pin a worker to a CPU. Passing it to
//! [`ThreadPoolBuilder::worker_layout`](crate::ThreadPoolBuilder::worker_layout)
//! applies the plan while the pool starts. Keeping planning separate from
//! enforcement lets layout policies remain deterministic, platform-independent,
//! and easy to test.
//!
//! Worker indices are software identities and must not be interpreted as a
//! measure of hardware proximity. Future schedulers should use the explicit
//! core and NUMA relationships stored in each [`WorkerPlacement`] instead of
//! assuming, for example, that workers 0 and 1 are physically adjacent.
//!
//! Layouts will be constructed through topology-aware policies such as one
//! worker per logical CPU or one worker per physical core. Their fields remain
//! private so callers cannot create duplicate indices, contradictory hardware
//! relationships, or other invalid placements directly.

use crate::topology::{CoreId, CpuId, NumaNodeId, PackageId};

/// Planned hardware placement of one Weave worker.
///
/// The placement associates a contiguous worker index with a logical CPU and
/// records the physical core and NUMA node reported for that CPU. Package
/// identity is derived from [`CoreId`], avoiding a second stored value that
/// could contradict the core's package.
///
/// This value expresses scheduling intent only. A pool built from its containing
/// [`WorkerLayout`] applies that intent before accepting work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerPlacement {
    worker_index: usize,
    cpu: CpuId,
    core: CoreId,
    numa_node: NumaNodeId,
}

impl WorkerPlacement {
    /// Returns the worker's contiguous index within its layout.
    pub const fn worker_index(&self) -> usize {
        self.worker_index
    }

    /// Returns the logical CPU selected for this worker.
    pub const fn cpu(&self) -> CpuId {
        self.cpu
    }

    /// Returns the physical core containing the selected logical CPU.
    pub const fn core(&self) -> CoreId {
        self.core
    }

    /// Returns the package containing the selected logical CPU.
    pub const fn package(&self) -> PackageId {
        self.core.package()
    }

    /// Returns the NUMA node containing the selected logical CPU.
    pub const fn numa_node(&self) -> NumaNodeId {
        self.numa_node
    }
}

/// Deterministic hardware-placement plan for a set of Weave workers.
///
/// Placements are stored in worker-index order, and valid layouts use
/// contiguous indices beginning at zero. The layout owns its placement data so
/// it can later be passed from configuration code to thread creation and the
/// scheduler without borrowing the source topology.
///
/// This type deliberately contains no scheduling or affinity operations. It
/// answers only "where should each worker be placed?". Pool construction
/// enforces the CPU placements; future schedulers can also use the recorded
/// topology relationships when selecting work-stealing victims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerLayout {
    workers: Vec<WorkerPlacement>,
}

impl WorkerLayout {
    /// Creates a layout containing one worker for every online logical CPU.
    ///
    /// Placements follow the logical-CPU order of `topology`, and worker indices
    /// are contiguous starting at zero. SMT siblings are retained as distinct
    /// workers because each sibling is a separate logical CPU.
    ///
    /// This method only constructs a placement plan; it does not create worker
    /// threads or apply CPU affinity by itself.
    pub fn one_per_logical_cpu(topology: &crate::topology::Topology) -> Self {
        let mut layout = Vec::new();
        for (index, cpu) in topology.logical_cpus().iter().enumerate() {
            let placement = WorkerPlacement {
                worker_index: index,
                cpu: cpu.id(),
                core: cpu.core(),
                numa_node: cpu.numa_node(),
            };
            layout.push(placement);
        }
        WorkerLayout { workers: layout }
    }

    /// Creates a layout containing one worker for every physical core.
    ///
    /// When several logical CPUs are SMT siblings on the same physical core, the
    /// first logical CPU in topology order is selected as the core's representative.
    /// Since topology CPUs are ordered by identifier, this deterministically selects
    /// the sibling with the smallest [`CpuId`].
    ///
    /// Core identity includes the package identifier, so cores with the same local
    /// number in different packages remain distinct. Worker indices are contiguous
    /// starting at zero.
    ///
    /// This method only constructs a placement plan; it does not create worker
    /// threads or apply CPU affinity by itself.
    pub fn one_per_physical_core(topology: &crate::topology::Topology) -> Self {
        let mut layout = Vec::new();
        let mut cores: std::collections::HashSet<CoreId> = std::collections::HashSet::new();

        for cpu in topology.logical_cpus().iter() {
            if cores.insert(cpu.core()) {
                let placement = WorkerPlacement {
                    worker_index: layout.len(),
                    cpu: cpu.id(),
                    core: cpu.core(),
                    numa_node: cpu.numa_node(),
                };
                layout.push(placement);
            }
        }
        WorkerLayout { workers: layout }
    }
    /// Returns the planned placements in worker-index order.
    pub fn workers(&self) -> &[WorkerPlacement] {
        &self.workers
    }

    /// Returns the number of workers described by this layout.
    pub const fn worker_count(&self) -> usize {
        self.workers.len()
    }

    /// Returns `true` when the layout contains no worker placement.
    pub fn is_empty(&self) -> bool {
        self.workers.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology::topology_fixture;

    fn sample_topology() -> crate::topology::Topology {
        topology_fixture(&[
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
        ])
    }

    #[test]
    fn one_per_logical_cpu_creates_contiguous_worker_indices() {
        let topology = sample_topology();

        let layout = WorkerLayout::one_per_logical_cpu(&topology);

        assert_eq!(layout.worker_count(), 4);
        assert!(!layout.is_empty());
        assert_eq!(
            layout
                .workers()
                .iter()
                .map(WorkerPlacement::worker_index)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
    }

    #[test]
    fn one_per_logical_cpu_preserves_cpu_order_and_hardware_relations() {
        let topology = sample_topology();

        let layout = WorkerLayout::one_per_logical_cpu(&topology);

        assert_eq!(
            layout
                .workers()
                .iter()
                .map(|worker| {
                    (
                        worker.cpu(),
                        worker.core(),
                        worker.package(),
                        worker.numa_node(),
                    )
                })
                .collect::<Vec<_>>(),
            vec![
                (
                    CpuId::new(0),
                    CoreId::new(PackageId::new(0), 0),
                    PackageId::new(0),
                    NumaNodeId::new(0),
                ),
                (
                    CpuId::new(2),
                    CoreId::new(PackageId::new(0), 1),
                    PackageId::new(0),
                    NumaNodeId::new(0),
                ),
                (
                    CpuId::new(7),
                    CoreId::new(PackageId::new(1), 0),
                    PackageId::new(1),
                    NumaNodeId::new(4),
                ),
                (
                    CpuId::new(8),
                    CoreId::new(PackageId::new(0), 0),
                    PackageId::new(0),
                    NumaNodeId::new(0),
                ),
            ]
        );
    }

    #[test]
    fn one_per_logical_cpu_keeps_smt_siblings_as_distinct_workers() {
        let topology = sample_topology();

        let layout = WorkerLayout::one_per_logical_cpu(&topology);
        let workers_on_core_zero = layout
            .workers()
            .iter()
            .filter(|worker| worker.core() == CoreId::new(PackageId::new(0), 0))
            .map(WorkerPlacement::cpu)
            .collect::<Vec<_>>();

        assert_eq!(workers_on_core_zero, vec![CpuId::new(0), CpuId::new(8)]);
    }

    #[test]
    fn one_per_physical_core_selects_the_first_cpu_of_each_core() {
        let topology = sample_topology();

        let layout = WorkerLayout::one_per_physical_core(&topology);

        assert_eq!(layout.worker_count(), 3);
        assert_eq!(
            layout
                .workers()
                .iter()
                .map(WorkerPlacement::cpu)
                .collect::<Vec<_>>(),
            vec![CpuId::new(0), CpuId::new(2), CpuId::new(7)]
        );
        assert!(
            !layout
                .workers()
                .iter()
                .any(|worker| worker.cpu() == CpuId::new(8))
        );
    }

    #[test]
    fn one_per_physical_core_creates_contiguous_worker_indices() {
        let topology = sample_topology();

        let layout = WorkerLayout::one_per_physical_core(&topology);

        assert_eq!(
            layout
                .workers()
                .iter()
                .map(WorkerPlacement::worker_index)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn one_per_physical_core_distinguishes_equal_core_numbers_across_packages() {
        let topology = sample_topology();

        let layout = WorkerLayout::one_per_physical_core(&topology);
        let local_core_zero = layout
            .workers()
            .iter()
            .filter(|worker| worker.core().get() == 0)
            .map(|worker| (worker.cpu(), worker.package(), worker.numa_node()))
            .collect::<Vec<_>>();

        assert_eq!(
            local_core_zero,
            vec![
                (CpuId::new(0), PackageId::new(0), NumaNodeId::new(0)),
                (CpuId::new(7), PackageId::new(1), NumaNodeId::new(4)),
            ]
        );
    }
}
