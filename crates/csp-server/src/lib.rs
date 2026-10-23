//! Embedded HTTP+WS server — telemetry screens, widget dashboards, MCP
//! endpoint (PLAN §2.12). NATIVE ONLY: never in the wasm dependency
//! tree (§8.2 asserts it: `csp-server` is on the wasm ban list).
//!
//! M9c ships the interface tier:
//!
//! - [`serve`] / [`http`] — the axum+tokio runtime: `POST /mcp`
//!   (Streamable HTTP at revision [`mcp::PROTOCOL_REVISION`]), `GET /ws`
//!   (the §2.12.2 subscription stream), `/` + `/api/telemetry` behind
//!   an `Origin` allowlist and a per-start bearer token.
//! - [`mcp`] — the transport-free MCP engine: JSON-RPC 2.0 plus the
//!   `csp.mcp.tools.v1` tool surface (session-class tools declared
//!   `"class": "session"`). The same engine feeds the `csp-mcp`
//!   stdio bin (newline-delimited JSON-RPC over a headless sim).
//! - [`backend`] — [`backend::ControlBackend`], the host seam:
//!   snapshot reads off the `ArcSwap` publish, mutations through the
//!   bounded ingress, session verbs for the save tools.
//!   [`backend::LocalGame`] is the reference headless host `csp-mcp`
//!   and the tests drive.
//! - [`ingress`] — the §2.2.1 bounded queue: ≤32 commands per client
//!   per drain, 64 KiB message cap, bounded depth; refusals are
//!   explicit backpressure errors.
//! - [`telemetry`] — the published `ArcSwap` snapshot + the
//!   field-path subscription projection.

#![forbid(unsafe_code)]

pub mod backend;
pub mod http;
pub mod ingress;
pub mod mcp;
pub mod serve;
pub mod telemetry;

pub use backend::{
    BackendError, ClientRegistry, ControlBackend, LocalGame, SaveInfo, SessionSummary,
};
pub use ingress::{IngressError, IngressQueue};
pub use mcp::{Mcp, PROTOCOL_REVISION, SUPPORTED_REVISIONS, TOOL_SCHEMA_VERSION};
pub use serve::{DEFAULT_PORT, ServeError, Server, ServerConfig, runtime_dir};
pub use telemetry::TelemetrySnapshot;
