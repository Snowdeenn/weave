use crate::topology::CpuId;

#[cfg(target_os = "linux")]
mod linux;

#[derive(Debug)]
pub(crate) enum AffinityError {
    CpuOutOfRange(CpuId),
    UnsupportedPlatform,
    Os(std::io::Error),
}

#[cfg(target_os = "linux")]
pub(crate) use linux::pin_current_thread;

#[cfg(not(target_os = "linux"))]
pub(crate) fn pin_current_thread(_cpu: CpuId) -> Result<(), AffinityError> {
    Err(AffinityError::UnsupportedPlatform)
}
