#[derive(Debug)]
pub enum WaveError {
    PanicOnJob(String)
}

impl std::fmt::Display for WaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PanicOnJob(s) => write!(f, "{s}"),
        }
    }
}
impl std::error::Error for WaveError {}
