use super::MemoryError;
struct Mapping {
    adr: *mut u8,
    length: usize,
}

impl Mapping {
    pub fn try_allocate(length: usize) -> Result<Self, MemoryError> {
        if length == 0 {
            return Err(MemoryError::InvalidSize);
        }
        // SAFETY: Si l'allocation on return proprement l'erreur
        // que l'utilisateur doit gérer proprement
        let adr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                length,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_ANON | libc::MAP_PRIVATE,
                -1,
                0,
            )
        };
        if adr == libc::MAP_FAILED {
            return Err(MemoryError::Os(std::io::Error::last_os_error()));
        }
        Ok(Mapping {
            adr: adr.cast(),
            length,
        })
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: cette adresse et cette longueur désignent le mapping
        // possédé exclusivement par self, qui n'a pas encore été libéré.
        let _ = unsafe { libc::munmap(self.adr.cast(), self.length) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allocate(length: usize) -> Mapping {
        // MemoryError does not implement Debug yet, so avoid Result::unwrap.
        match Mapping::try_allocate(length) {
            Ok(mapping) => mapping,
            Err(MemoryError::InvalidSize) => panic!("valid length {length} was rejected"),
            Err(MemoryError::Os(error)) => panic!("mmap failed for {length} bytes: {error}"),
        }
    }

    #[test]
    fn rejects_zero_length() {
        assert!(matches!(
            Mapping::try_allocate(0),
            Err(MemoryError::InvalidSize)
        ));
    }

    #[test]
    fn drop_unmaps_every_page_in_an_isolated_process() {
        const CHILD: &str = "WEAVE_MAPPING_DROP_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            // Other tests allocate concurrently. Run only this test in a child
            // so they cannot reuse the addresses between Drop and mincore.
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "memory::linux::tests::drop_unmaps_every_page_in_an_isolated_process",
                    "--test-threads=1",
                ])
                .env(CHILD, "1")
                .output()
                .expect("cannot launch the isolated mapping test");
            assert!(
                output.status.success(),
                "isolated test failed:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        // SAFETY: sysconf requires no memory arguments.
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        assert!(page_size > 0);
        let page_size = page_size as usize;
        let length = page_size.checked_mul(2).unwrap().checked_add(17).unwrap();
        for _ in 0..32 {
            let mapping = allocate(length);
            let base = mapping.adr as usize;
            let mut residency = 0u8;
            for page in 0..3 {
                // SAFETY: mincore queries one aligned page and writes one byte
                // into residency. It does not dereference the queried address
                // in Rust, nor require the page to be physically resident.
                let result = unsafe {
                    libc::mincore(
                        (base + page * page_size) as *mut libc::c_void,
                        page_size,
                        &mut residency,
                    )
                };
                assert_eq!(result, 0, "page must be mapped before Drop");
            }
            drop(mapping);
            for page in 0..3 {
                // SAFETY: the output byte is valid. The old address is passed
                // only as a query to Linux; no freed memory is dereferenced.
                let result = unsafe {
                    libc::mincore(
                        (base + page * page_size) as *mut libc::c_void,
                        page_size,
                        &mut residency,
                    )
                };
                let error = std::io::Error::last_os_error();
                assert_eq!(result, -1, "page {page} remained mapped after Drop");
                assert_eq!(error.raw_os_error(), Some(libc::ENOMEM));
            }
        }
    }

    #[test]
    fn one_byte_mapping_is_zero_initialized_and_writable() {
        let mapping = allocate(1);
        assert_eq!(mapping.length, 1);
        // SAFETY: mmap succeeded for one readable/writable byte, and the mapping
        // remains alive. Raw pointer operations do not create aliased references.
        unsafe {
            assert_eq!(mapping.adr.read(), 0);
            mapping.adr.write(0xa5);
            assert_eq!(mapping.adr.read(), 0xa5);
        }
        // Normal scope exit invokes Drop; never dereference the pointer afterward.
    }

    #[test]
    fn non_page_multiple_mapping_preserves_bytes_across_page_boundaries() {
        // SAFETY: sysconf takes a constant selector and no pointers.
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        assert!(page_size > 0, "cannot determine the page size");
        let page_size = page_size as usize;
        let length = page_size.checked_mul(2).unwrap().checked_add(17).unwrap();
        let mapping = allocate(length);
        assert_eq!(mapping.length, length);
        assert_eq!(mapping.adr as usize % page_size, 0);

        // SAFETY: all offsets are strictly below the requested mapping length.
        // The mapping stays alive and is accessed only by this test thread.
        unsafe {
            for offset in 0..length {
                assert_eq!(mapping.adr.add(offset).read(), 0);
                mapping.adr.add(offset).write((offset % 251) as u8);
            }
            for offset in 0..length {
                assert_eq!(mapping.adr.add(offset).read(), (offset % 251) as u8);
            }
        }
    }

    #[test]
    fn mappings_are_independent_and_dropping_one_preserves_the_other() {
        let first = allocate(1);
        let second = allocate(1);
        assert_ne!(first.adr, second.adr);
        // SAFETY: these are two live, independent, writable one-byte mappings.
        unsafe {
            first.adr.write(11);
            second.adr.write(29);
            assert_eq!(first.adr.read(), 11);
        }
        drop(first);
        // SAFETY: only first was dropped; second still owns its mapping.
        unsafe {
            assert_eq!(second.adr.read(), 29);
            second.adr.write(31);
            assert_eq!(second.adr.read(), 31);
        }
    }
}
