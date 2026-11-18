//! Ontology documents extracted from a read-only source dictionary.

mod diagram;
mod extract;
mod graph;
mod layout;
mod load;
mod merge;
mod mermaid;
mod render;
mod schema;
mod svg;

pub(crate) use diagram::{
    default_name, diagram, parse_patterns, DiagramReport, DiagramRequest, EdgeCounts,
};
pub use extract::pull;
pub(crate) use extract::split_table;
pub(crate) use graph::Selection;
pub use render::render;
pub use schema::validate;
