//! Generic walkers that produce uniform IR from different input sources
//!
//! This module implements the walker pattern for different schema sources.
//! Each walker produces the same IR structure, ensuring uniform handling
//! regardless of input format.

use amalgam_core::{
    ir::{TypeDefinition, IR},
};
use std::collections::{HashMap, HashSet};

pub mod openapi;
pub mod crd;
// pub mod go_ast; // TODO: Implement Go AST walker

pub use openapi::OpenAPIWalker;
pub use crd::CRDWalker;

/// Trait for walking different schema sources and producing uniform IR
pub trait SchemaWalker {
    /// The input type this walker processes
    type Input;
    
    /// Walk the input and produce a complete IR with all dependencies resolved
    fn walk(&self, input: Self::Input) -> Result<IR, WalkerError>;
    
    /// Extract all types from the input into a type registry
    fn extract_types(&self, input: &Self::Input) -> Result<TypeRegistry, WalkerError>;
    
    /// Build dependency graph from the type registry
    fn build_dependencies(&self, registry: &TypeRegistry) -> DependencyGraph;
    
    /// Generate complete IR with imports from registry and dependencies
    fn generate_ir(&self, registry: TypeRegistry, deps: DependencyGraph) -> Result<IR, WalkerError>;
}

/// Registry of all types with their full qualified names
#[derive(Debug, Clone)]
pub struct TypeRegistry {
    /// Map from fully qualified name to type definition
    /// e.g., "io.k8s.api.core.v1.Pod" -> TypeDefinition
    pub types: HashMap<String, TypeDefinition>,
    
    /// Grouped by module for easier processing
    /// e.g., "io.k8s.api.core.v1" -> ["Pod", "Service", ...]
    pub modules: HashMap<String, Vec<String>>,
}

impl TypeRegistry {
    pub fn new() -> Self {
        Self {
            types: HashMap::new(),
            modules: HashMap::new(),
        }
    }
    
    pub fn add_type(&mut self, fqn: &str, type_def: TypeDefinition) {
        self.types.insert(fqn.to_string(), type_def);
        
        // Extract module from FQN
        if let Some(last_dot) = fqn.rfind('.') {
            let module = &fqn[..last_dot];
            let type_name = &fqn[last_dot + 1..];
            self.modules
                .entry(module.to_string())
                .or_default()
                .push(type_name.to_string());
        }
    }
    
    pub fn get_type(&self, fqn: &str) -> Option<&TypeDefinition> {
        self.types.get(fqn)
    }
}

/// Dependency graph showing which types reference which other types
#[derive(Debug, Clone)]
pub struct DependencyGraph {
    /// Map from type FQN to set of types it depends on
    pub dependencies: HashMap<String, HashSet<String>>,
    
    /// Reverse map: type FQN to set of types that depend on it
    pub dependents: HashMap<String, HashSet<String>>,
}

impl DependencyGraph {
    pub fn new() -> Self {
        Self {
            dependencies: HashMap::new(),
            dependents: HashMap::new(),
        }
    }
    
    pub fn add_dependency(&mut self, from: &str, to: &str) {
        self.dependencies
            .entry(from.to_string())
            .or_default()
            .insert(to.to_string());
            
        self.dependents
            .entry(to.to_string())
            .or_default()
            .insert(from.to_string());
    }
    
    /// Get all cross-module dependencies for a type
    pub fn get_cross_module_deps(&self, fqn: &str) -> Vec<String> {
        let module = Self::extract_module(fqn);
        
        self.dependencies
            .get(fqn)
            .map(|deps| {
                deps.iter()
                    .filter(|dep| Self::extract_module(dep) != module)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
    
    fn extract_module(fqn: &str) -> &str {
        fqn.rfind('.')
            .map(|i| &fqn[..i])
            .unwrap_or(fqn)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WalkerError {
    #[error("Failed to parse schema: {0}")]
    ParseError(String),
    
    #[error("Invalid type reference: {0}")]
    InvalidReference(String),
    
    #[error("Circular dependency detected: {0}")]
    CircularDependency(String),
    
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}