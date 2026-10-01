use super::linux::MappedRegion;
use super::{MemoryError, NumaPolicy};

pub struct NumaBuffer {
    region: MappedRegion,
}

impl NumaBuffer {
    pub fn try_new(size: usize, policy: NumaPolicy) -> Result<Self, MemoryError> {
        if size == 0 || size > isize::MAX as usize {
            return Err(MemoryError::InvalidSize);
        }

        let sysfs_root = std::path::Path::new("/sys/devices/system");

        let known_nodes = crate::topology::linux::read_online_numa_nodes(sysfs_root)?;
        let mut region = MappedRegion::try_allocate(size)?;
        region.set_numa_policy(policy, known_nodes.as_slice())?;

        // SAFETY: region owns a live mapping writable for length() bytes.
        // No references to its data have been exposed, and u8 has alignment 1.
        // The NUMA policy was applied before this first write to the pages.
        unsafe {
            region.adr.write_bytes(0, region.length());
        };
        Ok(NumaBuffer { region })
    }

    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: la plage appartient à un unique mapping vivant, lisible sur
        // region.length() octets et correctement aligné pour u8. Le constructeur
        // a initialisé tous ces octets et vérifié la limite isize::MAX.
        // La slice retournée est liée à l'emprunt de self : le mapping ne peut
        // donc pas être libéré pendant son utilisation. Cet emprunt partagé
        // empêche aussi toute mutation via l'API sûre du buffer.
        unsafe { std::slice::from_raw_parts(self.region.adr, self.region.length()) }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: la plage appartient à un unique mapping vivant, lisible et
        // accessible en écriture sur region.length() octets, aligné pour u8.
        // Le constructeur a initialisé tous ces octets et vérifié isize::MAX.
        // La slice retournée est liée à l'emprunt exclusif de self : le mapping
        // reste vivant et aucun autre accès aux données via l'API sûre du buffer
        // n'est possible pendant cet emprunt.
        unsafe { std::slice::from_raw_parts_mut(self.region.adr, self.region.length()) }
    }

    /// Return the number of bytes in the `NumaBuffer`
    pub fn len(&self) -> usize {
        self.region.length()
    }

    /// Return true if the `NumaBuffer` contain no element
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Return the assigned [`NumaPolicy`] of the `NumaBuffer`
    pub fn policy(&self) -> NumaPolicy {
        self.region
            .policy()
            .expect("NumaBuffer always has a successfully applied NUMA policy")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology::NumaNodeId;

    #[test]
    fn rejects_zero_length_before_discovery() {
        assert!(matches!(
            NumaBuffer::try_new(0, NumaPolicy::Bind(NumaNodeId::new(0))),
            Err(MemoryError::InvalidSize)
        ));
    }

    #[test]
    fn rejects_lengths_above_the_slice_limit_before_allocation() {
        for size in [isize::MAX as usize + 1, usize::MAX] {
            assert!(matches!(
                NumaBuffer::try_new(size, NumaPolicy::Bind(NumaNodeId::new(0))),
                Err(MemoryError::InvalidSize)
            ));
        }
    }

    // The following tests exercise the real constructor, not a fabricated
    // buffer that bypasses its policy invariant. Run explicitly with:
    // cargo test --lib memory::buffer::tests -- --ignored
    // They require sysfs, procfs and permission to call mbind. A denied syscall
    // fails the test rather than being silently counted as a successful test.
    fn allowed_policy() -> NumaPolicy {
        // Use memory permissions of the calling thread, not CPU affinity or an
        // assumption that node 0 is allowed. The first list item may be a range.
        let status = std::fs::read_to_string("/proc/thread-self/status")
            .expect("cannot read this thread's allowed memory nodes");
        let list = status
            .lines()
            .find_map(|line| line.strip_prefix("Mems_allowed_list:"))
            .expect("missing Mems_allowed_list")
            .trim();
        let node = list
            .split([',', '-'])
            .next()
            .unwrap()
            .parse::<usize>()
            .expect("invalid or empty Mems_allowed_list");
        NumaPolicy::Bind(NumaNodeId::new(node))
    }

    #[test]
    #[ignore = "requires Linux NUMA discovery and an allowed mbind syscall"]
    fn one_byte_buffer_is_zeroed_and_reports_its_metadata() {
        let policy = allowed_policy();
        let buffer = NumaBuffer::try_new(1, policy).expect("buffer allocation failed");
        assert_eq!(buffer.len(), 1);
        assert!(!buffer.is_empty());
        assert_eq!(buffer.as_slice(), &[0]);
        // This checks recorded policy, not physical page residency.
        assert_eq!(buffer.policy(), policy);
    }

    #[test]
    #[ignore = "requires Linux NUMA discovery and an allowed mbind syscall"]
    fn slices_cover_the_requested_length_across_page_boundaries() {
        // SAFETY: sysconf takes a constant selector and no pointers.
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        assert!(page_size > 0);
        let size = 2 * page_size as usize + 17;
        let policy = allowed_policy();
        let mut buffer = NumaBuffer::try_new(size, policy).expect("buffer allocation failed");
        assert_eq!(buffer.len(), size);
        assert!(!buffer.is_empty());
        assert_eq!(buffer.as_slice().len(), size);
        assert!(buffer.as_slice().iter().all(|&byte| byte == 0));
        {
            let bytes = buffer.as_mut_slice();
            assert_eq!(bytes.len(), size);
            for (index, byte) in bytes.iter_mut().enumerate() {
                *byte = (index % 251) as u8;
            }
        }
        for (index, &byte) in buffer.as_slice().iter().enumerate() {
            assert_eq!(byte, (index % 251) as u8);
        }
        buffer.as_mut_slice().fill(0xa5);
        assert!(buffer.as_slice().iter().all(|&byte| byte == 0xa5));
        assert_eq!(buffer.policy(), policy);
    }

    #[test]
    #[ignore = "requires Linux NUMA discovery and an allowed mbind syscall"]
    fn buffers_are_independent_and_dropping_one_preserves_the_other() {
        let policy = allowed_policy();
        let mut first = NumaBuffer::try_new(17, policy).expect("first allocation failed");
        let mut second = NumaBuffer::try_new(17, policy).expect("second allocation failed");
        first.as_mut_slice().fill(11);
        assert_eq!(second.as_slice(), &[0; 17]);
        second.as_mut_slice().fill(29);
        assert_eq!(first.as_slice(), &[11; 17]);
        drop(first);
        assert_eq!(second.as_slice(), &[29; 17]);
        second.as_mut_slice()[16] = 31;
        assert_eq!(second.as_slice()[16], 31);
    }

    #[test]
    #[ignore = "requires Linux NUMA sysfs discovery"]
    fn rejects_a_node_absent_from_the_machine() {
        let node = NumaNodeId::new(usize::MAX);
        assert!(matches!(
            NumaBuffer::try_new(1, NumaPolicy::Bind(node)),
            Err(MemoryError::UnknownNode(rejected)) if rejected == node
        ));
    }
}
