//! The registry model shared by every interface.
//!
//! A [`Source`] owns its state and answers named operations described by an
//! [`OperationSpec`]. The [`Registry`] holds every source, applies the read-only policy and the
//! configured filters, and is the single entry point the CLI, MCP, REST and GraphQL facades call.

pub mod cursor;
pub mod error;
pub mod names;
pub mod normalized;
pub mod projection;
pub mod registry;
pub mod source;
pub mod spec;

pub use error::SourceError;
pub use normalized::{
    Item, ItemKind, OperationRef, SearchProvider, SearchQuery, SearchResult, SourceFailure,
};
pub use registry::{Exposed, Registry, RegistryError, Selection, SourceOptions};
pub use source::{CallContext, Source, Status, typed};
pub use spec::{OperationKind, OperationSpec, Page, schema_of};
