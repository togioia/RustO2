//! Small shared building blocks used by both RustO2 services.

pub mod env;
pub mod telemetry;

pub use telemetry::{
    init_tracing, install_metrics, request_id_middleware, shutdown_signal, RequestId,
    REQUEST_ID_HEADER,
};
