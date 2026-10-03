//! Emprunter un buffer NUMA depuis des tâches Weave, sans copier son stockage.

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::io;
    use weave::ThreadPoolBuilder;
    use weave::memory::{NumaPolicy, buffer::NumaBuffer};
    use weave::topology::NumaNodeId;

    // Choisir un nœud autorisé pour ce thread, sans supposer que le nœud 0 l'est.
    let status = std::fs::read_to_string("/proc/thread-self/status")?;
    let node = status
        .lines()
        .find_map(|line| line.strip_prefix("Mems_allowed_list:"))
        .and_then(|list| list.trim().split([',', '-']).next())
        .ok_or_else(|| io::Error::other("liste des nœuds mémoire autorisés absente"))?
        .parse::<usize>()?;

    const LEN: usize = 10_003;
    const CHUNK_SIZE: usize = 1_024;
    let policy = NumaPolicy::Bind(NumaNodeId::new(node));
    let mut buffer = NumaBuffer::<u64>::try_new(LEN, policy)?;
    let pool = ThreadPoolBuilder::new().num_threads(4).try_build()?;
    let storage = buffer.as_slice().as_ptr();

    pool.scope(|scope| {
        // chunks_mut fournit des emprunts exclusifs disjoints, y compris
        // pour le dernier morceau, plus court que CHUNK_SIZE.
        for (chunk_index, chunk) in buffer.as_mut_slice().chunks_mut(CHUNK_SIZE).enumerate() {
            scope.spawn(move || {
                // move transfère uniquement l'emprunt du morceau à la tâche.
                for (offset, value) in chunk.iter_mut().enumerate() {
                    let index = chunk_index * CHUNK_SIZE + offset;
                    *value = 2 * index as u64;
                }
            });
        }
    });
    // Le scope attend toutes les tâches : le buffer est de nouveau accessible.
    assert_eq!(buffer.as_slice().as_ptr(), storage);

    // Partager aussi le buffer lui-même en lecture : &NumaBuffer<u64> peut
    // être capturé par plusieurs tâches grâce à son implémentation de Sync.
    let shared = &buffer;
    pool.scope(|scope| {
        for start in (0..LEN).step_by(CHUNK_SIZE) {
            scope.spawn(move || {
                let end = (start + CHUNK_SIZE).min(LEN);
                for index in start..end {
                    assert_eq!(shared.as_slice()[index], 2 * index as u64);
                }
            });
        }
    });

    assert_eq!(buffer.policy(), policy);
    println!("{LEN} valeurs traitées en place et vérifiées, politique Bind({node}).");
    // L'exécution des tâches ne garantit pas leur proximité avec les pages.
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("Cet exemple nécessite Linux et la prise en charge de mbind.");
}
