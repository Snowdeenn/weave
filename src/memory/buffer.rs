//! Fixed-capacity, owned NUMA buffers for Linux.

use super::linux::MappedRegion;
use super::{MemoryError, NumaPolicy};

/// A fixed-capacity allocation containing a prefix of initialized `T` values.
///
/// The policy applies to the buffer's own storage, not to allocations owned by
/// its elements (for example a String's character storage). It records the
/// accepted policy, not an observation or permanent guarantee of page location.
/// Elements are destroyed before the underlying mapping is released.
///
/// This version rejects zero capacity and zero-sized types, never grows, and
/// does not implement Send or Sync. Borrowed slices may still be used across
/// threads when their element type and borrowing rules permit it.
///
/// # Example
///
/// ```no_run
/// use weave::memory::{buffer::NumaBuffer, MemoryError, NumaPolicy};
/// use weave::topology::NumaNodeId;
///
/// # fn example() -> Result<(), MemoryError> {
/// // Choose an online memory node allowed for this thread; node 0 is an example.
/// let policy = NumaPolicy::Bind(NumaNodeId::new(0));
/// let mut buffer = NumaBuffer::<u64>::try_with_capacity(2, policy)?;
/// assert!(buffer.is_empty());
/// assert!(buffer.try_push(42).is_ok());
/// assert_eq!(buffer.as_slice(), &[42]);
/// # Ok(())
/// # }
/// ```
pub struct NumaBuffer<T> {
    region: MappedRegion<T>,
    len: usize,
}

impl<T: Default> NumaBuffer<T> {
    /// Constructs `size` elements by calling `T::default()` separately for each.
    ///
    /// The NUMA policy is applied before element initialization. This does not
    /// require Clone or Debug and is not equivalent to filling storage with zeros.
    ///
    /// # Errors
    /// Returns the same allocation, discovery and policy errors as
    /// [`Self::try_with_capacity`]. Size is measured in elements, not bytes.
    ///
    /// # Panics
    /// If Default panics, the panic propagates. During unwinding, all elements
    /// already inserted are destroyed and the mapping is released. As usual,
    /// cleanup cannot be guaranteed when the process aborts.
    pub fn try_new(size: usize, policy: NumaPolicy) -> Result<Self, MemoryError> {
        let mut buffer = Self::try_with_capacity(size, policy)?;
        for _ in 0..buffer.region.capacity() {
            assert!(
                buffer.try_push(T::default()).is_ok(),
                "new buffer must have room for every requested element"
            );
        }
        Ok(buffer)
    }
}
impl<T> NumaBuffer<T> {
    /// Reserves storage for `capacity` elements without constructing any values.
    ///
    /// The returned buffer has length zero. Pages are not explicitly touched by
    /// this constructor; subsequent writes allocate pages under the policy.
    ///
    /// # Errors
    /// Rejects zero capacity, zero-sized types, arithmetic overflow and mapping
    /// sizes above isize::MAX (including alignment padding). Also propagates
    /// sysfs discovery, mask allocation, alignment and Linux syscall failures.
    /// There is no silent fallback when Linux rejects the requested policy.
    ///
    /// A successful mapping is not a guarantee of available physical memory:
    /// failures when pages are later touched may terminate the process instead
    /// of producing a Rust error.
    pub fn try_with_capacity(capacity: usize, policy: NumaPolicy) -> Result<Self, MemoryError> {
        if capacity == 0 || capacity > isize::MAX as usize {
            return Err(MemoryError::InvalidSize);
        }

        let sysfs_root = std::path::Path::new("/sys/devices/system");

        let known_nodes = crate::topology::linux::read_online_numa_nodes(sysfs_root)?;
        let mut region = MappedRegion::<T>::try_allocate(capacity)?;
        region.set_numa_policy(policy, known_nodes.as_slice())?;
        Ok(NumaBuffer { region, len: 0 })
    }

    /// Returns the initialized elements as a shared slice.
    pub fn as_slice(&self) -> &[T] {
        // SAFETY: la région fournit un pointeur non nul, aligné pour T, et assez
        // de stockage pour capacity éléments, dans la limite isize::MAX.
        // Seuls les len premiers éléments sont initialisés, avec len <= capacity.
        // La slice retournée est liée à l'emprunt de self : le mapping ne peut
        // donc pas être libéré pendant son utilisation. Cet emprunt partagé
        // empêche aussi toute mutation via l'API sûre du buffer.
        unsafe { std::slice::from_raw_parts(self.region.data_ptr(), self.len) }
    }

    /// Returns the initialized elements as an exclusive slice.
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        // SAFETY: la région fournit un pointeur non nul, aligné pour T, et assez
        // de stockage accessible en écriture, dans la limite isize::MAX.
        // Seuls les len premiers éléments sont initialisés, avec len <= capacity.
        // La slice retournée est liée à l'emprunt exclusif de self : le mapping
        // reste vivant et aucun autre accès aux données via l'API sûre du buffer
        // n'est possible pendant cet emprunt.
        unsafe { std::slice::from_raw_parts_mut(self.region.data_ptr(), self.len) }
    }

    /// Appends a value, or returns it unchanged if the buffer is full.
    ///
    /// On rejection, length and existing elements are unchanged. This method
    /// never grows the allocation.
    pub fn try_push(&mut self, value: T) -> Result<(), T> {
        let len = self.len;

        if len >= self.region.capacity() {
            return Err(value);
        }

        // SAFETY: len < capacity garantit que cet emplacement aligné pour T
        // appartient au mapping vivant. Il ne contient pas encore de valeur
        // initialisée à détruire. &mut self garantit l'exclusivité de l'accès.
        unsafe {
            let end = self.region.data_ptr().add(len);
            std::ptr::write(end, value);
        };
        self.len += 1;
        Ok(())
    }

    /// Returns the storage pointer without extending its lifetime.
    ///
    /// Only the first len elements are initialized. The caller must uphold
    /// aliasing and initialization invariants when using this pointer; writing
    /// into spare storage does not update len or arrange destruction of values.
    pub fn as_mut_ptr(&mut self) -> *mut T {
        self.region.data_ptr()
    }

    /// Returns the number of initialized elements.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns true if there are no initialized elements.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the applied policy, not the observed placement of the pages.
    pub fn policy(&self) -> NumaPolicy {
        self.region
            .policy()
            .expect("NumaBuffer always has a successfully applied NUMA policy")
    }
}

impl<T> Drop for NumaBuffer<T> {
    fn drop(&mut self) {
        let element = core::ptr::slice_from_raw_parts_mut(self.as_mut_ptr(), self.len());

        // SAFETY: les len éléments sont initialisé, accessible exclusivement
        // et le mapping reste vivant pendant leur destruction
        unsafe {
            element.drop_in_place();
        }
        // MappedRegion gère la déallocation de la région
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology::NumaNodeId;

    // Per-thread state keeps the Default/Drop probes independent under parallel
    // test execution. The probe deliberately implements neither Clone nor Debug.
    std::thread_local! {
        static DEFAULT_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        static DROPS: std::cell::RefCell<Vec<usize>> = const { std::cell::RefCell::new(Vec::new()) };
        static PANIC_AT: std::cell::Cell<usize> = const { std::cell::Cell::new(usize::MAX) };
    }

    struct DefaultProbe {
        index: usize,
    }

    impl Default for DefaultProbe {
        fn default() -> Self {
            let index = DEFAULT_CALLS.with(|calls| {
                let index = calls.get();
                calls.set(index + 1);
                index
            });
            assert!(
                index != PANIC_AT.with(|at| at.get()),
                "intentional Default panic"
            );
            Self { index }
        }
    }

    impl Drop for DefaultProbe {
        fn drop(&mut self) {
            DROPS.with(|drops| drops.borrow_mut().push(self.index));
        }
    }

    fn reset_probe(panic_at: usize) {
        DEFAULT_CALLS.with(|calls| calls.set(0));
        DROPS.with(|drops| drops.borrow_mut().clear());
        PANIC_AT.with(|at| at.set(panic_at));
    }

    #[test]
    fn invalid_new_sizes_do_not_call_default() {
        reset_probe(usize::MAX);
        for size in [0, isize::MAX as usize + 1, usize::MAX] {
            assert!(matches!(
                NumaBuffer::<DefaultProbe>::try_new(size, NumaPolicy::Bind(NumaNodeId::new(0))),
                Err(MemoryError::InvalidSize)
            ));
        }
        assert_eq!(DEFAULT_CALLS.with(|calls| calls.get()), 0);
        DROPS.with(|drops| assert!(drops.borrow().is_empty()));
    }

    #[test]
    #[ignore = "requires Linux NUMA discovery and an allowed mbind syscall"]
    fn new_constructs_each_default_and_drops_every_element_once() {
        reset_probe(usize::MAX);
        let policy = allowed_policy();
        let buffer = NumaBuffer::<DefaultProbe>::try_new(4, policy).unwrap();
        assert_eq!(buffer.len(), 4);
        assert!(!buffer.is_empty());
        assert_eq!(buffer.policy(), policy);
        assert_eq!(
            buffer
                .as_slice()
                .iter()
                .map(|v| v.index)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        assert_eq!(DEFAULT_CALLS.with(|calls| calls.get()), 4);
        DROPS.with(|drops| assert!(drops.borrow().is_empty()));
        drop(buffer);
        DROPS.with(|drops| assert_eq!(*drops.borrow(), vec![0, 1, 2, 3]));
    }

    #[test]
    #[ignore = "requires Linux NUMA discovery and an allowed mbind syscall"]
    fn default_panic_drops_exactly_the_initialized_prefix() {
        let policy = allowed_policy();
        for panic_at in [0, 1, 3] {
            reset_probe(panic_at);
            let result =
                std::panic::catch_unwind(|| NumaBuffer::<DefaultProbe>::try_new(4, policy));
            // A syscall failure returns Ok(Err(_)), and must not count as a
            // successful panic-cleanup test. Verify the expected panic payload.
            let payload = match result {
                Err(payload) => payload,
                Ok(Err(error)) => panic!("construction failed before Default: {error}"),
                Ok(Ok(_)) => panic!("Default should have panicked"),
            };
            assert_eq!(
                payload.downcast_ref::<&str>(),
                Some(&"intentional Default panic")
            );
            assert_eq!(DEFAULT_CALLS.with(|calls| calls.get()), panic_at + 1);
            DROPS.with(|drops| assert_eq!(*drops.borrow(), (0..panic_at).collect::<Vec<_>>()));
        }
        reset_probe(usize::MAX);
    }

    #[test]
    fn rejects_zero_length_before_discovery() {
        assert!(matches!(
            NumaBuffer::<u8>::try_with_capacity(0, NumaPolicy::Bind(NumaNodeId::new(0))),
            Err(MemoryError::InvalidSize)
        ));
    }

    #[test]
    fn rejects_lengths_above_the_slice_limit_before_allocation() {
        for size in [isize::MAX as usize + 1, usize::MAX] {
            assert!(matches!(
                NumaBuffer::<u8>::try_with_capacity(size, NumaPolicy::Bind(NumaNodeId::new(0))),
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
    fn reserved_buffer_starts_empty_and_returns_rejected_value_when_full() {
        let policy = allowed_policy();
        let mut buffer =
            NumaBuffer::<String>::try_with_capacity(1, policy).expect("buffer allocation failed");
        assert_eq!(buffer.len(), 0);
        assert!(buffer.is_empty());
        assert!(buffer.as_slice().is_empty());
        assert!(buffer.as_mut_slice().is_empty());
        buffer.try_push(String::from("stored")).unwrap();
        assert_eq!(buffer.len(), 1);
        assert!(!buffer.is_empty());
        let rejected = buffer.try_push(String::from("returned")).unwrap_err();
        assert_eq!(rejected, "returned");
        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer.as_slice(), &[String::from("stored")]);
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
        let mut buffer =
            NumaBuffer::<u8>::try_with_capacity(size, policy).expect("buffer allocation failed");
        for _ in 0..size {
            buffer.try_push(0).unwrap();
        }
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
        let mut first =
            NumaBuffer::<u8>::try_with_capacity(17, policy).expect("first allocation failed");
        let mut second =
            NumaBuffer::<u8>::try_with_capacity(17, policy).expect("second allocation failed");
        for _ in 0..17 {
            first.try_push(0).unwrap();
            second.try_push(0).unwrap();
        }
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
            NumaBuffer::<u8>::try_with_capacity(1, NumaPolicy::Bind(node)),
            Err(MemoryError::UnknownNode(rejected)) if rejected == node
        ));
    }

    #[test]
    #[ignore = "requires Linux NUMA discovery and an allowed mbind syscall"]
    fn drop_destroys_only_initialized_elements_and_preserves_rejected_values() {
        use std::cell::Cell;
        use std::rc::Rc;
        struct Tracked(Rc<Cell<usize>>);
        impl Drop for Tracked {
            fn drop(&mut self) {
                self.0.set(self.0.get() + 1);
            }
        }
        let drops = Rc::new(Cell::new(0));
        let policy = allowed_policy();
        let empty = NumaBuffer::<Tracked>::try_with_capacity(4, policy).unwrap();
        drop(empty);
        assert_eq!(drops.get(), 0);

        let mut partial = NumaBuffer::<Tracked>::try_with_capacity(4, policy).unwrap();
        assert!(partial.try_push(Tracked(drops.clone())).is_ok());
        assert!(partial.try_push(Tracked(drops.clone())).is_ok());
        assert_eq!(partial.as_slice().len(), 2);
        assert_eq!(drops.get(), 0);
        drop(partial);
        assert_eq!(drops.get(), 2);

        let mut full = NumaBuffer::<Tracked>::try_with_capacity(1, policy).unwrap();
        assert!(full.try_push(Tracked(drops.clone())).is_ok());
        let rejected = full.try_push(Tracked(drops.clone())).err().unwrap();
        assert_eq!(drops.get(), 2);
        drop(full);
        assert_eq!(drops.get(), 3);
        drop(rejected);
        assert_eq!(drops.get(), 4);
    }
}
