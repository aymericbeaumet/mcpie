//! mcpie connects information sources such as Slack and GitHub and exposes their read-only
//! operations through a CLI, an MCP server, a REST API and a GraphQL endpoint.
//!
//! Everything is generated from one runtime registry of sources and operations; the
//! interfaces are thin projections of that registry. See [`model`] for the registry contract.

pub mod model;

/// The program name used in help output, user agents and configuration paths.
pub const NAME: &str = "mcpie";

/// The crate version, stamped by the release workflow.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
