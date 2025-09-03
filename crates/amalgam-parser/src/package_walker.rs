//! Package walker adapter that bridges Package types to the walker infrastructure

use crate::walkers::{DependencyGraph, TypeRegistry, WalkerError};
use amalgam_core::{
    ir::{Import, Module, TypeDefinition, IR},
    types::Type,
};
use std::collections::{HashMap, HashSet};
use tracing::{debug, instrument};

/// Adapter to convert Package's internal type storage to walker-compatible format
pub struct PackageWalkerAdapter;

impl PackageWalkerAdapter {
    /// Convert Package types for a version into TypeRegistry
    pub fn build_registry(
        types: &HashMap<String, TypeDefinition>,
        group: &str,
        version: &str,
    ) -> Result<TypeRegistry, WalkerError> {
        let mut registry = TypeRegistry::new();

        for (kind, type_def) in types {
            let fqn = format!("{}.{}.{}", group, version, kind.to_lowercase());
            registry.add_type(&fqn, type_def.clone());
        }

        Ok(registry)
    }

    /// Build dependency graph from type registry
    pub fn build_dependencies(registry: &TypeRegistry) -> DependencyGraph {
        let mut graph = DependencyGraph::new();

        for (fqn, type_def) in &registry.types {
            let refs = Self::extract_references(&type_def.ty);

            for ref_info in refs {
                // Build the full qualified name of the dependency
                let dep_fqn = if let Some(module) = ref_info.module {
                    // If module info is present, use it to build FQN
                    format!("{}.{}", module, ref_info.name.to_lowercase())
                } else {
                    // Try to find the type in the same module first
                    let self_module = fqn.rsplit_once('.').map(|(m, _)| m).unwrap_or("");
                    format!("{}.{}", self_module, ref_info.name.to_lowercase())
                };

                // Add dependency if it exists in registry or is a k8s type
                if registry.types.contains_key(&dep_fqn) || dep_fqn.starts_with("io.k8s.") {
                    graph.add_dependency(fqn, &dep_fqn);
                }
            }
        }

        graph
    }

    /// Generate IR with imports from registry and dependencies
    #[instrument(skip(registry, deps), fields(group = %group, version = %version), level = "debug")]
    pub fn generate_ir(
        registry: TypeRegistry,
        deps: DependencyGraph,
        group: &str,
        version: &str,
    ) -> Result<IR, WalkerError> {
        debug!("Generating IR for {}.{}", group, version);
        let mut ir = IR::new();

        // Create a module for each type
        for (fqn, type_def) in registry.types {
            let mut module = Module {
                name: fqn.clone(),
                imports: Vec::new(),
                types: vec![type_def],
                constants: Vec::new(),
                metadata: Default::default(),
            };

            // Get cross-module dependencies and add imports
            let cross_deps = deps.get_cross_module_deps(&fqn);
            let mut imports_map: HashMap<String, HashSet<String>> = HashMap::new();

            for dep_fqn in cross_deps {
                let (import_path, type_name) =
                    Self::calculate_import(&fqn, &dep_fqn, group, version);

                imports_map
                    .entry(import_path)
                    .or_default()
                    .insert(type_name);
            }

            // Convert imports map to Import structs
            for (import_path, import_types) in imports_map {
                let alias = Self::generate_alias(&import_path);

                module.imports.push(Import {
                    path: import_path,
                    alias: Some(alias),
                    items: import_types.into_iter().collect(),
                });
            }

            ir.add_module(module);
        }

        Ok(ir)
    }

    /// Extract type references from a Type
    fn extract_references(ty: &Type) -> Vec<ReferenceInfo> {
        let mut refs = Vec::new();
        Self::collect_references(ty, &mut refs);
        refs
    }

    fn collect_references(ty: &Type, refs: &mut Vec<ReferenceInfo>) {
        match ty {
            Type::Reference { name, module } => {
                refs.push(ReferenceInfo {
                    name: name.clone(),
                    module: module.clone(),
                });
            }
            Type::Array(inner) => Self::collect_references(inner, refs),
            Type::Optional(inner) => Self::collect_references(inner, refs),
            Type::Map { value, .. } => Self::collect_references(value, refs),
            Type::Record { fields, .. } => {
                for field in fields.values() {
                    Self::collect_references(&field.ty, refs);
                }
            }
            Type::Union { types, .. } => {
                for t in types {
                    Self::collect_references(t, refs);
                }
            }
            Type::TaggedUnion { variants, .. } => {
                for t in variants.values() {
                    Self::collect_references(t, refs);
                }
            }
            Type::Contract { base, .. } => Self::collect_references(base, refs),
            _ => {}
        }
    }

    /// Calculate import path and type name for a dependency
    fn calculate_import(
        _from_fqn: &str,
        to_fqn: &str,
        _group: &str,
        version: &str,
    ) -> (String, String) {
        // Extract type name from dependency FQN
        let type_name = to_fqn.rsplit('.').next().unwrap_or(to_fqn).to_string();

        // Handle k8s core types specially
        if to_fqn.starts_with("io.k8s.") {
            // Map to our k8s package structure
            let import_path = if to_fqn.contains(".v1.") || to_fqn.contains(".meta.v1.") {
                "../../../k8s_io/v1".to_string()
            } else if to_fqn.contains(".v1alpha1.") {
                "../../../k8s_io/v1alpha1".to_string()
            } else if to_fqn.contains(".v1alpha3.") {
                "../../../k8s_io/v1alpha3".to_string()
            } else if to_fqn.contains(".v1beta1.") {
                "../../../k8s_io/v1beta1".to_string()
            } else if to_fqn.contains(".v2.") {
                "../../../k8s_io/v2".to_string()
            } else {
                "../../../k8s_io/v1".to_string()
            };

            (format!("{}/{}.ncl", import_path, type_name), type_name)
        } else {
            // Internal cross-version reference
            let to_parts: Vec<&str> = to_fqn.split('.').collect();
            if to_parts.len() >= 2 {
                let to_version = to_parts[to_parts.len() - 2];
                if to_version != version {
                    // Cross-version import
                    (format!("../{}/{}.ncl", to_version, type_name), type_name)
                } else {
                    // Same version import
                    (format!("./{}.ncl", type_name), type_name)
                }
            } else {
                // Default to same directory
                (format!("./{}.ncl", type_name), type_name)
            }
        }
    }

    /// Generate an alias for an import path
    fn generate_alias(import_path: &str) -> String {
        // Extract meaningful part from path
        import_path
            .trim_end_matches(".ncl")
            .rsplit('/')
            .next()
            .unwrap_or("import")
            .to_string()
    }
}

#[derive(Debug, Clone)]
struct ReferenceInfo {
    name: String,
    module: Option<String>,
}
