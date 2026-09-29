#[cfg(target_os = "linux")]
pub mod linux;

pub enum MemoryError {
    InvalidSize,
    Os(std::io::Error)
}
