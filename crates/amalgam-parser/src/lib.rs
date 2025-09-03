//! Schema parsers for various formats

pub mod crd;
pub mod dependency_graph;
pub mod error;
pub mod fetch;
pub mod go;
pub mod go_ast;
pub mod imports;
pub mod incremental;
pub mod k8s_authoritative;
// pub mod k8s_imports; // Deprecated - no longer used after unification
pub mod k8s_types;
pub mod openapi;
pub mod package;
pub mod package_walker;
// pub mod version_generator; // Deprecated - functionality moved to walkers
pub mod walkers;

use amalgam_core::IR;

pub use error::ParserError;

/// Common trait for all parsers
pub trait Parser {
    type Input;

    fn parse(&self, input: Self::Input) -> Result<IR, ParserError>;
}
