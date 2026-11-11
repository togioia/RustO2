//! WSO2 Synapse artifact parsing and mediation.
//!
//! This crate implements the subset of WSO2's Synapse runtime that RustO2 can
//! execute faithfully: API definitions, resource URI templates, in/out/fault
//! sequences and the mediators listed in [`mediate`]. Unsupported mediators are
//! rejected at load time instead of being silently ignored.

pub mod apim;
pub mod bundle;
pub mod car;
pub mod error;
pub mod expression;
pub mod mediate;
pub mod message;
pub mod sequence;
pub mod synapse;
pub mod template;
pub mod transport;

pub use apim::{ApiDefinition, OperationDefinition};
pub use bundle::ArtifactBundle;
pub use error::SynapseError;
pub use mediate::{MediationPlan, MediationResult};
pub use message::{MessageContext, Payload, PropertyScope};
pub use sequence::{Mediator, Sequence};
pub use synapse::{ApiArtifact, ResourceArtifact};
pub use transport::{Transport, UpstreamRequest, UpstreamResponse};
