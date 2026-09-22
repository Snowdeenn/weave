use std::{error::Error, io};
use weave::{AffinityError, BuildError, topology::CpuId};

#[test]
fn affinity_errors_without_a_cause_describe_the_failure() {
    let error = AffinityError::CpuOutOfRange(CpuId::new(1234));
    assert_eq!(
        error.to_string(),
        "CPU 1234 cannot be represented by the affinity mask"
    );
    assert!(error.source().is_none());

    let error = AffinityError::UnsupportedPlatform;
    assert_eq!(
        error.to_string(),
        "CPU affinity is not supported on this platform"
    );
    assert!(error.source().is_none());
}

#[test]
fn build_error_preserves_worker_context_and_the_full_cause_chain() {
    // Preserve a raw OS code without relying on platform-specific message text.
    let os_error = io::Error::from_raw_os_error(22);
    let os_message = os_error.to_string();
    let error = BuildError::Affinity {
        worker_index: 2,
        cpu: CpuId::new(7),
        source: AffinityError::Os(os_error),
    };

    assert_eq!(
        error.to_string(),
        format!("cannot pin worker 2 to CPU 7: cannot set CPU affinity: {os_message}")
    );
    let affinity = error
        .source()
        .unwrap()
        .downcast_ref::<AffinityError>()
        .unwrap();
    let os = affinity
        .source()
        .unwrap()
        .downcast_ref::<io::Error>()
        .unwrap();
    assert_eq!(os.raw_os_error(), Some(22));
    assert!(os.source().is_none());
}

#[test]
fn build_error_preserves_an_affinity_failure_without_an_os_cause() {
    let error = BuildError::Affinity {
        worker_index: 3,
        cpu: CpuId::new(1234),
        source: AffinityError::CpuOutOfRange(CpuId::new(1234)),
    };
    let affinity = error
        .source()
        .unwrap()
        .downcast_ref::<AffinityError>()
        .unwrap();
    assert!(matches!(affinity, AffinityError::CpuOutOfRange(cpu) if cpu.get() == 1234));
    assert!(affinity.source().is_none());
}
