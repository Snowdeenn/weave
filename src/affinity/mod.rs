use crate::topology::CpuId;

mod linux;

pub enum AffinityError {
    CpuOutOfRange(CpuId),
    UnsupportedPlatform,
    Os(std::io::Error)
}

#[cfg(target_os = "linux")]
pub use linux::pin_current_thread;


