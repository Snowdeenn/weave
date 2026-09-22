#![cfg(target_os = "linux")]

use std::{
    collections::BTreeSet,
    io,
    sync::{Arc, Barrier},
};
use weave::{ThreadPoolBuilder, WorkerLayout, topology::Topology};

/// Returns the logical CPUs on which the calling thread may currently run.
fn current_affinity() -> io::Result<BTreeSet<usize>> {
    let mut cpu_set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    let result =
        unsafe { libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut cpu_set) };

    if result == -1 {
        return Err(io::Error::last_os_error());
    }

    Ok((0..libc::CPU_SETSIZE as usize)
        .filter(|&cpu| unsafe { libc::CPU_ISSET(cpu, &cpu_set) })
        .collect())
}

/// Returns the logical CPU executing the calling thread at this instant.
fn current_cpu() -> io::Result<usize> {
    let cpu = unsafe { libc::sched_getcpu() };
    if cpu == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(cpu as usize)
    }
}

fn planned_cpus(layout: &WorkerLayout) -> BTreeSet<usize> {
    layout
        .workers()
        .iter()
        .map(|worker| worker.cpu().get())
        .collect()
}

/// Sysfs may expose online CPUs that a container or cpuset forbids this process
/// from using. Such an environment cannot exercise a layout containing them.
fn environment_allows(layout: &WorkerLayout, allowed: &BTreeSet<usize>) -> bool {
    planned_cpus(layout).is_subset(allowed)
}

#[test]
fn building_a_layout_pool_does_not_change_the_callers_affinity() {
    let before = current_affinity().expect("cannot read the test thread affinity");
    let topology = Topology::discover().expect("cannot discover the machine topology");
    let layout = WorkerLayout::one_per_physical_core(&topology);

    if !environment_allows(&layout, &before) {
        eprintln!(
            "skipping affinity assertion: the discovered layout contains CPUs forbidden to this process"
        );
        return;
    }

    let pool = ThreadPoolBuilder::new()
        .worker_layout(layout)
        .try_build()
        .expect("cannot construct the layout-based pool");
    let after = current_affinity().expect("cannot reread the test thread affinity");

    assert_eq!(after, before);
    drop(pool);
}

#[test]
fn workers_run_on_the_cpus_selected_by_the_layout() {
    let allowed = current_affinity().expect("cannot read the test thread affinity");
    let topology = Topology::discover().expect("cannot discover the machine topology");
    let layout = WorkerLayout::one_per_physical_core(&topology);

    if !environment_allows(&layout, &allowed) {
        eprintln!(
            "skipping affinity assertion: the discovered layout contains CPUs forbidden to this process"
        );
        return;
    }

    let expected = planned_cpus(&layout);
    let worker_count = layout.worker_count();
    let pool = ThreadPoolBuilder::new()
        .worker_layout(layout)
        .try_build()
        .expect("cannot construct the layout-based pool");

    // Each task waits after occupying one worker. The barrier can only open
    // after every worker has taken exactly one of these tasks.
    let barrier = Arc::new(Barrier::new(worker_count + 1));
    let handles = (0..worker_count)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            pool.submit(move || {
                barrier.wait();
                current_cpu().expect("cannot determine the worker's current CPU")
            })
        })
        .collect::<Vec<_>>();

    barrier.wait();

    let actual = handles
        .into_iter()
        .map(|handle| handle.join().expect("worker task panicked"))
        .collect::<BTreeSet<_>>();

    assert_eq!(actual, expected);
}
