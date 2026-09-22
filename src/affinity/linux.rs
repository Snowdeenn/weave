use super::AffinityError;
use crate::topology::CpuId;

fn cpu_set_for(cpu: CpuId) -> Result<libc::cpu_set_t, AffinityError> {
    if cpu.get() >= libc::CPU_SETSIZE as usize {
        return Err(AffinityError::CpuOutOfRange(cpu));
    }

    let mut cpu_set: libc::cpu_set_t = unsafe { std::mem::zeroed() };

    unsafe {
        libc::CPU_ZERO(&mut cpu_set);
        libc::CPU_SET(cpu.get(), &mut cpu_set);
    }

    Ok(cpu_set)
}

pub(crate) fn pin_current_thread(cpu: CpuId) -> Result<(), AffinityError> {
    let cpu_set = cpu_set_for(cpu)?;

    unsafe {
        let result_code =
            libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &cpu_set);

        if result_code == -1 {
            return Err(AffinityError::Os(std::io::Error::last_os_error()));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_mask_containing_only_the_requested_cpu() {
        let selected = 2;

        let cpu_set = cpu_set_for(CpuId::new(selected)).unwrap();

        for cpu in 0..libc::CPU_SETSIZE as usize {
            assert_eq!(unsafe { libc::CPU_ISSET(cpu, &cpu_set) }, cpu == selected);
        }
    }

    #[test]
    fn rejects_a_cpu_outside_the_fixed_mask() {
        let cpu = CpuId::new(libc::CPU_SETSIZE as usize);

        let error = cpu_set_for(cpu).unwrap_err();

        assert!(matches!(error, AffinityError::CpuOutOfRange(id) if id == cpu));
    }

    #[test]
    fn pins_a_dedicated_thread_to_one_allowed_cpu() {
        std::thread::spawn(|| {
            let allowed = current_affinity().expect("cannot read the test thread affinity");
            let selected = (0..libc::CPU_SETSIZE as usize)
                .find(|&cpu| unsafe { libc::CPU_ISSET(cpu, &allowed) })
                .expect("the test thread has no allowed CPU");

            pin_current_thread(CpuId::new(selected)).expect("cannot pin the test thread");

            let actual = current_affinity().expect("cannot read the pinned thread affinity");
            let actual_cpus = (0..libc::CPU_SETSIZE as usize)
                .filter(|&cpu| unsafe { libc::CPU_ISSET(cpu, &actual) })
                .collect::<Vec<_>>();
            assert_eq!(actual_cpus, vec![selected]);
        })
        .join()
        .expect("the affinity test thread panicked");
    }

    fn current_affinity() -> Result<libc::cpu_set_t, std::io::Error> {
        let mut cpu_set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut cpu_set)
        };

        if result == -1 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(cpu_set)
        }
    }
}
