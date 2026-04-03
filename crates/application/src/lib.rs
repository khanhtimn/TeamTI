pub mod enrichment_orchestrator;
pub mod error;
pub mod events;
pub mod ports;

// Keep v1 services module for adapter compatibility during transition
pub mod dto;
pub mod services;

pub use enrichment_orchestrator::EnrichmentOrchestrator;
pub use error::AppError;
pub use events::{AcoustIdRequest, TrackScanned};
