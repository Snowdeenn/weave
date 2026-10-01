//! Owned memory and NUMA policy support.

use std::collections::TryReserveError;

use crate::topology::{NumaNodeId, TopologyError};

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "linux")]
pub mod buffer;
/// Failure to allocate memory or configure its NUMA policy.
#[derive(Debug)]
pub enum MemoryError {
    /// The requested mapping length is invalid.
    InvalidSize,
    /// The requested node is absent from the supplied node set.
    UnknownNode(NumaNodeId),
    /// The node identifier cannot be represented by the Linux mask interface.
    InvalidNodeId(NumaNodeId),
    /// A Linux operation failed; the original OS error is preserved.
    Os(std::io::Error),
    /// Reserving the mask failed, including capacity overflow.
    Allocation(TryReserveError),
    /// Discovering the known NUMA nodes failed.
    Topology(TopologyError),
    /// The alignement of the data failed
    Align,
}

impl From<TopologyError> for MemoryError {
    fn from(error: TopologyError) -> Self {
        Self::Topology(error)
    }
}

impl From<TryReserveError> for MemoryError {
    fn from(error: TryReserveError) -> Self {
        Self::Allocation(error)
    }
}

impl std::fmt::Display for MemoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSize => write!(f, "invalid memory mapping size"),
            Self::Align => write!(f, "memory mapping alignment failed"),
            Self::UnknownNode(id) => write!(f, "unknown NUMA node {}", id.get()),
            Self::InvalidNodeId(id) => write!(f, "unrepresentable NUMA node {}", id.get()),
            Self::Os(error) => write!(f, "memory operation failed: {error}"),
            Self::Allocation(error) => write!(f, "NUMA mask allocation failed: {error}"),
            Self::Topology(error) => write!(f, "NUMA node discovery failed: {error}"),
        }
    }
}

impl std::error::Error for MemoryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Os(error) => Some(error),
            Self::Allocation(error) => Some(error),
            Self::Topology(error) => Some(error),
            _ => None,
        }
    }
}

/// Policy associated with a region, not an observation of resident pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumaPolicy {
    /// Restricts future page allocations to the selected node when accepted.
    /// Applying this policy does not migrate pages that already exist.
    Bind(NumaNodeId),
}
