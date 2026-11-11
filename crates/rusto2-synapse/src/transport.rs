use async_trait::async_trait;
use http::HeaderMap;

use crate::error::SynapseError;

/// The HTTP request the mediator engine asks the gateway to perform.
#[derive(Debug, Clone)]
pub struct UpstreamRequest {
    pub method: String,
    pub url: String,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

/// The backend response returned to the mediation flow. The engine never
/// interprets HTTP status codes as faults: `<call>` passes 4xx/5xx back to the
/// sequence, exactly as Synapse's HTTP transport does.
#[derive(Debug, Clone)]
pub struct UpstreamResponse {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

/// Pluggable transport, so the mediation engine can be tested without a
/// network and the gateway can reuse its pooled reqwest client.
#[async_trait]
pub trait Transport: Send + Sync {
    async fn send(&self, request: UpstreamRequest) -> Result<UpstreamResponse, SynapseError>;
}
