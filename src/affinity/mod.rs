use crate::topology::CpuId;

#[cfg(target_os = "linux")]
mod linux;

/// Failure to bind a worker thread to a logical CPU.
///
/// CPU identifiers describe the discovered topology; the operating system may
/// still reject a placement, for example because the CPU is unavailable or
/// restricted by the execution environment.
#[derive(Debug)]
pub enum AffinityError {
    /// The CPU identifier cannot be represented by the implementation's CPU mask.
    /// This does not mean that the CPU is absent from the machine.
    CpuOutOfRange(CpuId),
    /// CPU affinity is not implemented on this platform.
    UnsupportedPlatform,
    /// The operating system rejected the affinity request.
    Os(std::io::Error),
}

impl std::fmt::Display for AffinityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CpuOutOfRange(cpu) => {
                write!(
                    f,
                    "CPU {} cannot be represented by the affinity mask",
                    cpu.get()
                )
            }
            Self::UnsupportedPlatform => {
                write!(f, "CPU affinity is not supported on this platform")
            }
            Self::Os(error) => write!(f, "cannot set CPU affinity: {error}"),
        }
    }
}

impl std::error::Error for AffinityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Os(error) => Some(error),
            Self::CpuOutOfRange(_) | Self::UnsupportedPlatform => None,
        }
    }
}

#[cfg(target_os = "linux")]
pub(crate) use linux::pin_current_thread;

#[cfg(not(target_os = "linux"))]
pub(crate) fn pin_current_thread(_cpu: CpuId) -> Result<(), AffinityError> {
    Err(AffinityError::UnsupportedPlatform)
}
