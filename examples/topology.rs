use weave::topology::Topology;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let topology = Topology::discover()?;

    println!("{topology:#?}");
    println!("logical CPUs: {}", topology.logical_cpu_count());
    println!("physical cores: {}", topology.physical_core_count());
    println!("packages: {}", topology.package_count());
    println!("NUMA nodes: {}", topology.numa_node_count());

    Ok(())
}
