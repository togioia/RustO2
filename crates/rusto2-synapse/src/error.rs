use std::path::PathBuf;

use thiserror::Error;

/// Errors raised while reading WSO2 artifacts or executing a mediation flow.
///
/// The parser is deliberately strict: an unknown mediator, endpoint or security
/// scheme produces an error instead of being skipped. That is the difference
/// between "looks like WSO2" and "behaves like WSO2".
#[derive(Debug, Error)]
pub enum SynapseError {
    #[error("invalid Synapse XML: {0}")]
    Xml(String),

    #[error("unsupported mediator <{name}> at {location}")]
    UnsupportedMediator { name: String, location: String },

    #[error("unsupported Synapse artifact <{0}>")]
    UnsupportedArtifact(String),

    #[error("unsupported endpoint configuration: {0}")]
    UnsupportedEndpoint(String),

    #[error("unsupported security scheme: {0}")]
    UnsupportedSecurity(String),

    #[error("invalid API definition: {0}")]
    ApiDefinition(String),

    #[error("invalid CAR package {path}: {reason}")]
    Car { path: PathBuf, reason: String },

    #[error("artifact path not found: {0}")]
    NotFound(PathBuf),

    #[error("invalid URI template: {0}")]
    UriTemplate(String),

    #[error("duplicate {kind} definition: {name}")]
    Duplicate { kind: &'static str, name: String },

    #[error("unknown sequence reference: {0}")]
    UnknownSequence(String),

    #[error("unknown endpoint reference: {0}")]
    UnknownEndpoint(String),

    #[error("mediation failed: {0}")]
    Mediation(String),

    #[error("unsupported expression: {0}")]
    UnsupportedExpression(String),

    #[error("transport error: {0}")]
    Transport(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl SynapseError {
    pub(crate) fn xml(message: impl Into<String>) -> Self {
        Self::Xml(message.into())
    }
}
