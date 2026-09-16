use super::*;
pub(super) fn parse_cpu_list(input: &str) -> Result<Vec<CpuId>, TopologyError> {
    let mut cpus = Vec::new();
    for element in input.trim().split(',') {
        if element.contains('-') {
            let mut parts = element.split('-');
            let Ok(start) = parts.next().unwrap().parse::<usize>() else {
                return Err(TopologyError::InvalidCpuList {
                    input: element.to_string(),
                    reason: "Impossible de parse l'id cpu",
                });
            };
            let Ok(end) = parts.next().unwrap().parse::<usize>() else {
                return Err(TopologyError::InvalidCpuList {
                    input: element.to_string(),
                    reason: "Impossible de parse l'id cpu",
                });
            };
            if parts.next().is_some() {
                return Err(TopologyError::InvalidCpuList {
                    input: element.to_string(),
                    reason: "La range de cpu id devrait avoir que 2 borne",
                });
            }
            if start <= end {
                for id in start..=end {
                    cpus.push(CpuId::new(id));
                }
            } else {
                return Err(TopologyError::InvalidCpuList {
                    input: element.to_string(),
                    reason: "La fin de la range des cpu est inférieur au début: start > end",
                });
            }
        } else {
            let Ok(id) = element.parse::<usize>() else {
                return Err(TopologyError::InvalidCpuList {
                    input: element.to_string(),
                    reason: "Impossible de parse l'id du cpu",
                });
            };
            cpus.push(CpuId::new(id));
        }
    }

    cpus.sort_unstable_by_key(|id| id.get());
    if cpus.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(TopologyError::InvalidCpuList {
            input: input.to_string(),
            reason: "Les doublons de cpu id ne sont pas accepter",
        });
    }
    Ok(cpus)
}

pub(super) fn read_online_cpus(sysfs_root: &std::path::Path) -> Result<Vec<CpuId>, TopologyError> {
    let path = sysfs_root.join("cpu/online");
    let file = match std::fs::read_to_string(&path) {
        Ok(f) => f,
        Err(e) => {
            return Err(TopologyError::Io {
                path: path,
                error: e,
            });
        }
    };
    let cpus = parse_cpu_list(&file)?;
    Ok(cpus)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    struct FakeSysfs {
        root: PathBuf,
    }

    impl FakeSysfs {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir()
                .join(format!("weave-topology-{}-{unique}", std::process::id()));
            fs::create_dir_all(root.join("cpu")).unwrap();
            Self { root }
        }

        fn root(&self) -> &Path {
            &self.root
        }
    }

    impl Drop for FakeSysfs {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn ids(values: &[usize]) -> Vec<CpuId> {
        values.iter().copied().map(CpuId::new).collect()
    }

    #[test]
    fn parses_one_cpu() {
        assert_eq!(parse_cpu_list("0").unwrap(), ids(&[0]));
    }

    #[test]
    fn parses_non_contiguous_cpu_ids() {
        assert_eq!(parse_cpu_list("0,2,7").unwrap(), ids(&[0, 2, 7]));
    }

    #[test]
    fn parses_an_inclusive_range() {
        assert_eq!(parse_cpu_list("0-3").unwrap(), ids(&[0, 1, 2, 3]));
    }

    #[test]
    fn parses_mixed_ids_ranges_and_trailing_newline() {
        assert_eq!(
            parse_cpu_list("0-3,8,10-11\n").unwrap(),
            ids(&[0, 1, 2, 3, 8, 10, 11])
        );
    }

    #[test]
    fn returns_cpu_ids_in_ascending_order() {
        assert_eq!(parse_cpu_list("7,0,4-5").unwrap(), ids(&[0, 4, 5, 7]));
    }

    #[test]
    fn rejects_empty_input() {
        assert!(parse_cpu_list("").is_err());
    }

    #[test]
    fn rejects_non_numeric_cpu_ids() {
        assert!(parse_cpu_list("0,two,7").is_err());
    }

    #[test]
    fn rejects_descending_ranges() {
        assert!(parse_cpu_list("3-1").is_err());
    }

    #[test]
    fn rejects_ranges_with_too_many_bounds() {
        assert!(parse_cpu_list("1-2-3").is_err());
    }

    #[test]
    fn rejects_empty_list_elements() {
        assert!(parse_cpu_list("0,,2").is_err());
    }

    #[test]
    fn rejects_duplicate_cpu_ids() {
        assert!(parse_cpu_list("0-2,2").is_err());
    }

    #[test]
    fn reads_online_cpus_from_a_sysfs_root() {
        let sysfs = FakeSysfs::new();
        fs::write(sysfs.root().join("cpu/online"), "0-2,7\n").unwrap();

        assert_eq!(read_online_cpus(sysfs.root()).unwrap(), ids(&[0, 1, 2, 7]));
    }
}
