//! Runtime endpoint discovery, admission and selection state.

pub mod manager;
pub mod registry;

pub use manager::EndpointManager;
pub use registry::EndpointManagerRegistry;

pub use crate::domain::substituter::model::EndpointSnapshot;
