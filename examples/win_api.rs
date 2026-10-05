use windows_sys::Win32::Foundation as foundation;
use windows_sys::Win32::System::SystemInformation as info;

// Lire le membre de la union situé après Relationship et Size.
// Le pointeur doit désigner une entrée Windows valide de `size` octets.
unsafe fn read_payload<T: Copy>(entry: *const u8, size: usize) -> Option<T> {
    if size < 8 + std::mem::size_of::<T>() {
        return None;
    }

    Some(unsafe { entry.add(8).cast::<T>().read_unaligned() })
}

fn main() -> Result<(), foundation::WIN32_ERROR> {
    let mut bytes = 0u32;

    // 1. Demander combien d’octets sont nécessaires.
    let result = unsafe {
        info::GetLogicalProcessorInformationEx(info::RelationAll, std::ptr::null_mut(), &mut bytes)
    };

    if result == foundation::FALSE {
        let error = unsafe { foundation::GetLastError() };
        if error != foundation::ERROR_INSUFFICIENT_BUFFER {
            return Err(error);
        }
    }

    // 2. Allouer assez de mémoire.
    // Des u64 garantissent aussi un alignement de 8 octets.
    let mut buffer = vec![0u64; (bytes as usize).div_ceil(8)];

    let result = unsafe {
        info::GetLogicalProcessorInformationEx(
            info::RelationAll,
            buffer.as_mut_ptr().cast(),
            &mut bytes,
        )
    };

    if result == foundation::FALSE {
        return Err(unsafe { foundation::GetLastError() });
    }

    // 3. Chaque entrée commence par deux champs de 4 octets :
    // Relationship, puis Size.
    let ptr = buffer.as_ptr().cast::<u8>();
    let mut offset = 0usize;
    let mut count = 0;
    while offset < bytes as usize {
        if bytes as usize - offset < 8 {
            return Err(foundation::ERROR_INVALID_DATA);
        }

        let (relationship, size) = unsafe {
            let entry = ptr.add(offset);
            (
                entry.cast::<i32>().read_unaligned(),
                entry.add(4).cast::<u32>().read_unaligned() as usize,
            )
        };

        if size < 8 || size > bytes as usize - offset {
            return Err(foundation::ERROR_INVALID_DATA);
        }

        let entry = unsafe { ptr.add(offset) };

        match relationship {
            info::RelationProcessorCore | info::RelationProcessorPackage => {
                let processor =
                    unsafe { read_payload::<info::PROCESSOR_RELATIONSHIP>(entry, size) }
                        .ok_or(foundation::ERROR_INVALID_DATA)?;

                let kind = if relationship == info::RelationProcessorCore {
                    "Cœur"
                } else {
                    "Package"
                };

                println!(
                    "{kind}: {} groupe(s), classe d’efficacité {}",
                    processor.GroupCount, processor.EfficiencyClass,
                );

                // Le binding expose seulement le premier élément du tableau variable.
                let affinity = processor.GroupMask[0];
                println!(
                    "  Premier groupe: {}, masque CPU {:#b}",
                    affinity.Group, affinity.Mask,
                );
            }
            info::RelationNumaNode | info::RelationNumaNodeEx => {
                let numa = unsafe { read_payload::<info::NUMA_NODE_RELATIONSHIP>(entry, size) }
                    .ok_or(foundation::ERROR_INVALID_DATA)?;

                println!(
                    "Nœud NUMA {}: {} groupe(s)",
                    numa.NodeNumber, numa.GroupCount,
                );

                let affinity = unsafe { numa.Anonymous.GroupMask };
                println!(
                    "  Premier groupe: {}, masque CPU {:#b}",
                    affinity.Group, affinity.Mask,
                );
            }
            info::RelationGroup => {
                let group = unsafe { read_payload::<info::GROUP_RELATIONSHIP>(entry, size) }
                    .ok_or(foundation::ERROR_INVALID_DATA)?;

                println!("Groupes actifs: {}", group.ActiveGroupCount);

                let first = group.GroupInfo[0];
                println!(
                    "  Groupe 0: {} processeurs logiques, masque CPU {:#b}",
                    first.ActiveProcessorCount, first.ActiveProcessorMask,
                );
            }
            _ => {} // Ignorer les caches et les autres relations.
        }

        // Passer à l’entrée suivante, selon sa taille réelle.
        offset += size;
        count += 1;
    }
    println!("{count} entrées de topologie au total (y compris les entrées ignorées).");

    Ok(())
}
