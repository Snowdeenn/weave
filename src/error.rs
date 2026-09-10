/// Pool construction failure.
#[derive(Debug)]
pub enum BuildError {
    /// At least one worker is required.
    ZeroThreads,
    /// Thread names cannot contain a NUL byte.
    InvalidThreadName,
    /// The OS could not create a worker.
    Spawn(std::io::Error),
}
impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ZeroThreads => write!(f, "a pool needs at least one worker"),
            Self::InvalidThreadName => write!(f, "thread name contains a NUL byte"),
            Self::Spawn(e) => write!(f, "cannot start worker: {e}"),
        }
    }
}
impl std::error::Error for BuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(e) => Some(e),
            _ => None,
        }
    }
}
