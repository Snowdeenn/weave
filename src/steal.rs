//! Deterministic victim orders computed once during pool construction.
//!
//! Pools without a layout use circular order. Layout-based pools prefer the
//! same physical core, then other cores in the same NUMA node, then remote
//! nodes. Within each category, layout order breaks ties. These are scheduling
//! heuristics, not guarantees about cache contents or memory placement.
//! Priorities and local/global queue selection remain the scheduler's concern.

use crate::{WorkerLayout, WorkerPlacement};

/// Snapshot of successful transfers from other workers' local queues.
///
/// Categories are mutually exclusive and describe planned worker placements,
/// not the location of task data or measured memory traffic. Local pops, global
/// queue pops and unsuccessful steal attempts do not increment these counters.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StealStats {
    /// Transfers between workers sharing a physical core.
    pub same_core: usize,
    /// Transfers within a NUMA node, excluding same-core transfers.
    pub same_numa: usize,
    /// Transfers between different NUMA nodes.
    pub remote_numa: usize,
    /// Transfers in pools constructed without hardware placements.
    pub unknown: usize,
}

impl StealStats {
    /// Returns the total number of successful steals in this snapshot.
    pub fn total(&self) -> usize {
        self.same_core + self.same_numa + self.remote_numa + self.unknown
    }
}

/// One victim list per worker, excluding itself and containing no duplicates.
pub(crate) struct StealPlan {
    victims: Vec<Vec<usize>>,
    placements: Option<Vec<WorkerPlacement>>,
}

impl StealPlan {
    /// Visits subsequent indices, wrapping to zero after the final worker.
    pub(crate) fn circular(worker_count: usize) -> Self {
        let mut victims = Vec::new();
        for curr_worker in 0..worker_count {
            let mut workers = Vec::new();
            for offset in 1..worker_count {
                let victim = (curr_worker + offset) % worker_count;
                workers.push(victim);
            }
            victims.push(workers);
        }
        StealPlan {
            victims,
            placements: None,
        }
    }

    /// Prefers same-core, same-node, then remote victims in layout order.
    pub(crate) fn topology_aware(layout: &WorkerLayout) -> Self {
        let mut victims = Vec::new();
        for curr_worker in layout.workers() {
            let mut same_core = Vec::new();
            let mut same_node = Vec::new();
            let mut other = Vec::new();
            for worker in layout.workers() {
                if curr_worker.worker_index() == worker.worker_index() {
                    continue;
                }

                if curr_worker.core() == worker.core() {
                    same_core.push(worker.worker_index());
                } else if curr_worker.numa_node() == worker.numa_node() {
                    same_node.push(worker.worker_index());
                } else {
                    other.push(worker.worker_index());
                }
            }
            same_core.append(&mut same_node);
            same_core.append(&mut other);

            let workers = same_core;
            victims.push(workers);
        }

        StealPlan {
            victims,
            placements: Some(layout.workers().to_vec()),
        }
    }

    /// Returns the ordered victims for a valid worker index.
    pub(crate) fn victims_for(&self, worker_index: usize) -> &[usize] {
        &self.victims[worker_index]
    }

    /// Classifies one successful transfer using the saved placement metadata.
    pub(crate) fn record_steal(&self, thief: usize, victim: usize, stats: &mut StealStats) {
        match &self.placements {
            None => stats.unknown += 1,
            Some(placements) => {
                let source = &placements[thief];
                let target = &placements[victim];
                if source.core() == target.core() {
                    stats.same_core += 1;
                } else if source.numa_node() == target.numa_node() {
                    stats.same_numa += 1;
                } else {
                    stats.remote_numa += 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology::{CoreId, CpuId, NumaNodeId, PackageId, topology_fixture};

    fn sample_layout() -> WorkerLayout {
        let topology = topology_fixture(&[
            (
                CpuId::new(0),
                CoreId::new(PackageId::new(0), 0),
                NumaNodeId::new(0),
            ),
            (
                CpuId::new(1),
                CoreId::new(PackageId::new(1), 0),
                NumaNodeId::new(1),
            ),
            (
                CpuId::new(2),
                CoreId::new(PackageId::new(0), 1),
                NumaNodeId::new(0),
            ),
            (
                CpuId::new(4),
                CoreId::new(PackageId::new(0), 0),
                NumaNodeId::new(0),
            ),
            (
                CpuId::new(6),
                CoreId::new(PackageId::new(1), 2),
                NumaNodeId::new(1),
            ),
        ]);
        WorkerLayout::one_per_logical_cpu(&topology)
    }

    #[test]
    fn circular_starts_after_each_worker_and_wraps_around() {
        let plan = StealPlan::circular(4);

        assert_eq!(plan.victims_for(0), &[1, 2, 3]);
        assert_eq!(plan.victims_for(1), &[2, 3, 0]);
        assert_eq!(plan.victims_for(2), &[3, 0, 1]);
        assert_eq!(plan.victims_for(3), &[0, 1, 2]);
    }

    #[test]
    fn topology_aware_prioritizes_core_then_numa_then_remote_workers() {
        let plan = StealPlan::topology_aware(&sample_layout());

        // Worker 3 is its SMT sibling, worker 2 is NUMA-local, and workers 1
        // and 4 are remote. Their relative order follows the layout.
        assert_eq!(plan.victims_for(0), &[3, 2, 1, 4]);
        assert_eq!(plan.victims_for(3), &[0, 2, 1, 4]);

        // Equal local core numbers in different packages are not siblings.
        assert_eq!(plan.victims_for(1), &[4, 0, 2, 3]);
    }

    #[test]
    fn topology_aware_lists_every_other_worker_exactly_once() {
        let layout = sample_layout();
        let plan = StealPlan::topology_aware(&layout);

        for worker in 0..layout.worker_count() {
            let mut actual = plan.victims_for(worker).to_vec();
            actual.sort_unstable();
            let expected = (0..layout.worker_count())
                .filter(|&candidate| candidate != worker)
                .collect::<Vec<_>>();

            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn empty_and_single_worker_plans_have_no_victims() {
        let empty_topology = topology_fixture(&[]);
        let empty_layout = WorkerLayout::one_per_logical_cpu(&empty_topology);
        let single_topology = topology_fixture(&[(
            CpuId::new(7),
            CoreId::new(PackageId::new(2), 3),
            NumaNodeId::new(4),
        )]);
        let single_layout = WorkerLayout::one_per_logical_cpu(&single_topology);

        assert!(StealPlan::circular(0).victims.is_empty());
        assert!(StealPlan::circular(1).victims_for(0).is_empty());
        assert!(StealPlan::topology_aware(&empty_layout).victims.is_empty());
        assert!(
            StealPlan::topology_aware(&single_layout)
                .victims_for(0)
                .is_empty()
        );
    }
}
