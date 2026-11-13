//! Ontology documents extracted from a read-only source dictionary.

mod extract;
mod merge;
mod render;
mod schema;

pub use extract::pull;
pub(crate) use extract::split_table;
pub use render::render;
pub use schema::validate;
