use crate::WorkerLayout;

pub(crate) struct StealPlan {
    victims: Vec<Vec<usize>>,
}

impl StealPlan {
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
        StealPlan { victims }
    }

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

        StealPlan { victims }
    }

    pub(crate) fn victims_for(&self, worker_index: usize) -> &[usize] {
        &self.victims[worker_index]
    }
}
