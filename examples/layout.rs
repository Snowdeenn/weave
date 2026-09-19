use weave::{WorkerLayout, topology::Topology};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let topology = Topology::discover()?;

    let logical = WorkerLayout::one_per_logical_cpu(&topology);
    let physical = WorkerLayout::one_per_physical_core(&topology);

    println!("{logical:#?}");
    println!("{physical:#?}");
    Ok(())
}
