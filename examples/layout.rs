use weave::{ThreadPoolBuilder, WorkerLayout, topology::Topology};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let topology = Topology::discover()?;

    let logical = WorkerLayout::one_per_logical_cpu(&topology);
    let physical = WorkerLayout::one_per_physical_core(&topology);

    println!("{logical:#?}");
    println!("{physical:#?}");

    let pool = ThreadPoolBuilder::new()
        .thread_name("weave-physical")
        .worker_layout(physical)
        .try_build()?;
    println!("started {} pinned workers", pool.num_threads());

    Ok(())
}
