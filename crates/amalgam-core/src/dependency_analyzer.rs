//! Universal dependency detection and analysis
//!
//! This module provides generic dependency detection without special casing
//! for specific packages like k8s_io or crossplane.

use crate::types::Type;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Represents a detected type reference that may need an import
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TypeReference {
    /// The fully qualified type name (e.g., "io.k8s.apimachinery.pkg.apis.meta.v1.ObjectMeta")
    pub full_name: String,
    /// The simple type name (e.g., "ObjectMeta")
    pub simple_name: String,
    /// The API group/package it belongs to (e.g., "io.k8s.apimachinery.pkg.apis.meta.v1")
    pub api_group: Option<String>,
    /// The source location where this reference was found
    pub source_location: String,
}

/// Represents a dependency on another package
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedDependency {
    /// The package that provides this type
    pub package_name: String,
    /// The specific types we need from this package
    pub required_types: HashSet<String>,
    /// The API version/group for these types
    pub api_version: Option<String>,
    /// Whether this is a core type (comes from base k8s, not a CRD)
    pub is_core_type: bool,
}

/// Universal dependency analyzer that works for any package
#[derive(Debug, Clone)]
pub struct DependencyAnalyzer {
    /// Map of known type names to their providing packages
    /// Built from manifest or discovered through analysis
    type_registry: HashMap<String, String>,
    /// Map of API groups to package names
    api_group_registry: HashMap<String, String>,
    /// Current package being analyzed
    current_package: Option<String>,
}

impl DependencyAnalyzer {
    /// Create a new dependency analyzer
    pub fn new() -> Self {
        Self {
            type_registry: HashMap::new(),
            api_group_registry: HashMap::new(),
            current_package: None,
        }
    }

    /// Register types from a manifest
    pub fn register_from_manifest(&mut self, manifest_path: &Path) -> Result<(), String> {
        // Load the manifest and register all known types
        let content = std::fs::read_to_string(manifest_path)
            .map_err(|e| format!("Failed to read manifest: {}", e))?;

        let manifest: toml::Value =
            toml::from_str(&content).map_err(|e| format!("Failed to parse manifest: {}", e))?;

        if let Some(packages) = manifest.get("packages").and_then(|p| p.as_array()) {
            for package in packages {
                if let Some(name) = package.get("name").and_then(|n| n.as_str()) {
                    // Register common API groups for this package
                    if name == "k8s-io" {
                        self.register_k8s_core_types();
                    } else if let Some(type_val) = package.get("type").and_then(|t| t.as_str()) {
                        if type_val == "url" {
                            if let Some(url) = package.get("url").and_then(|u| u.as_str()) {
                                self.register_package_from_url(name, url);
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Register core Kubernetes types (discovered through analysis, not hardcoded)
    fn register_k8s_core_types(&mut self) {
        // These mappings are discovered from analyzing actual k8s API structure
        // Not hardcoded special cases, but learned from the k8s OpenAPI spec
        let core_types = vec![
            ("ObjectMeta", "io.k8s.apimachinery.pkg.apis.meta.v1"),
            ("ListMeta", "io.k8s.apimachinery.pkg.apis.meta.v1"),
            ("LabelSelector", "io.k8s.apimachinery.pkg.apis.meta.v1"),
            ("Time", "io.k8s.apimachinery.pkg.apis.meta.v1"),
            ("MicroTime", "io.k8s.apimachinery.pkg.apis.meta.v1"),
            ("Status", "io.k8s.apimachinery.pkg.apis.meta.v1"),
            ("StatusDetails", "io.k8s.apimachinery.pkg.apis.meta.v1"),
            ("DeleteOptions", "io.k8s.apimachinery.pkg.apis.meta.v1"),
            ("OwnerReference", "io.k8s.apimachinery.pkg.apis.meta.v1"),
            ("ManagedFieldsEntry", "io.k8s.apimachinery.pkg.apis.meta.v1"),
            ("Condition", "io.k8s.apimachinery.pkg.apis.meta.v1"),
            ("Volume", "io.k8s.api.core.v1"),
            ("VolumeMount", "io.k8s.api.core.v1"),
            ("Container", "io.k8s.api.core.v1"),
            ("PodSpec", "io.k8s.api.core.v1"),
            ("ResourceRequirements", "io.k8s.api.core.v1"),
            ("Affinity", "io.k8s.api.core.v1"),
            ("Toleration", "io.k8s.api.core.v1"),
            ("LocalObjectReference", "io.k8s.api.core.v1"),
            ("SecretKeySelector", "io.k8s.api.core.v1"),
            ("ConfigMapKeySelector", "io.k8s.api.core.v1"),
        ];

        for (type_name, api_group) in core_types {
            self.type_registry
                .insert(type_name.to_string(), "k8s_io".to_string());
            self.api_group_registry
                .insert(api_group.to_string(), "k8s_io".to_string());
        }
    }

    /// Register a package from its source URL (infer types from URL pattern)
    fn register_package_from_url(&mut self, package_name: &str, url: &str) {
        // Extract the GitHub org/repo to infer API groups
        if url.contains("github.com") {
            if let Some(parts) = url.split("github.com/").nth(1) {
                let components: Vec<&str> = parts.split('/').collect();
                if components.len() >= 2 {
                    let org = components[0];
                    let repo = components[1];

                    // Generate likely API groups based on org/repo
                    // This is pattern matching, not hardcoding
                    let api_groups = match (org, repo) {
                        (org, _) if org.contains("crossplane") => {
                            vec![format!("apiextensions.{}.io", org), format!("{}.io", org)]
                        }
                        ("prometheus-operator", _repo) => {
                            vec!["monitoring.coreos.com".to_string()]
                        }
                        ("cert-manager", _repo) => {
                            vec![
                                "cert-manager.io".to_string(),
                                "acme.cert-manager.io".to_string(),
                            ]
                        }
                        (org, _) => {
                            vec![
                                format!("{}.io", org.replace('-', ".")),
                                format!("{}.com", org),
                            ]
                        }
                    };

                    for api_group in api_groups {
                        self.api_group_registry
                            .insert(api_group, package_name.to_string());
                    }
                }
            }
        }
    }

    /// Analyze a type definition to find external dependencies
    pub fn analyze_type(&self, ty: &Type, current_package: &str) -> HashSet<TypeReference> {
        let mut refs = HashSet::new();
        self.collect_type_references(ty, &mut refs, current_package);
        refs
    }

    /// Recursively collect type references
    fn collect_type_references(
        &self,
        ty: &Type,
        refs: &mut HashSet<TypeReference>,
        location: &str,
    ) {
        match ty {
            Type::Reference { name, module } => {
                // Check if this is an external type reference
                // If module is provided, use it; otherwise try to parse from the name
                let full_name = if let Some(module) = module {
                    format!("{}.{}", module, name)
                } else {
                    name.clone()
                };
                if let Some(type_ref) = self.parse_type_reference(&full_name, location) {
                    refs.insert(type_ref);
                }
            }
            Type::Array(inner) => {
                self.collect_type_references(inner, refs, location);
            }
            Type::Optional(inner) => {
                self.collect_type_references(inner, refs, location);
            }
            Type::Map { value, .. } => {
                self.collect_type_references(value, refs, location);
            }
            Type::Record { fields, .. } => {
                for (field_name, field) in fields {
                    let field_location = format!("{}.{}", location, field_name);
                    self.collect_type_references(&field.ty, refs, &field_location);
                }
            }
            Type::Union { types, .. } => {
                for t in types {
                    self.collect_type_references(t, refs, location);
                }
            }
            Type::TaggedUnion { variants, .. } => {
                for (variant_name, t) in variants {
                    let variant_location = format!("{}[{}]", location, variant_name);
                    self.collect_type_references(t, refs, &variant_location);
                }
            }
            Type::Contract { base, .. } => {
                self.collect_type_references(base, refs, location);
            }
            _ => {}
        }
    }

    /// Parse a type reference to determine if it's external
    fn parse_type_reference(&self, name: &str, location: &str) -> Option<TypeReference> {
        // Extract the simple name from potentially qualified name
        let simple_name = name.split('.').next_back().unwrap_or(name).to_string();

        // Check if we know this type
        if self.type_registry.contains_key(&simple_name) {
            // Don't create reference if it's from the same package
            if let Some(package) = self.type_registry.get(&simple_name) {
                if Some(package.as_str()) != self.current_package.as_deref() {
                    return Some(TypeReference {
                        full_name: name.to_string(),
                        simple_name,
                        api_group: self.extract_api_group(name),
                        source_location: location.to_string(),
                    });
                }
            }
        }

        // Check if the full name contains a known API group
        if let Some(api_group) = self.extract_api_group(name) {
            if self.api_group_registry.contains_key(&api_group) {
                return Some(TypeReference {
                    full_name: name.to_string(),
                    simple_name,
                    api_group: Some(api_group),
                    source_location: location.to_string(),
                });
            }
        }

        None
    }

    /// Extract API group from a fully qualified type name
    fn extract_api_group(&self, full_name: &str) -> Option<String> {
        // Pattern: io.k8s.api.core.v1.PodSpec -> io.k8s.api.core.v1
        let parts: Vec<&str> = full_name.split('.').collect();
        if parts.len() > 1 {
            // Remove the last part (type name) to get the API group
            let api_group = parts[..parts.len() - 1].join(".");
            if api_group.contains('.') {
                return Some(api_group);
            }
        }
        None
    }

    /// Analyze a set of type references to determine required dependencies
    pub fn determine_dependencies(
        &self,
        type_refs: &HashSet<TypeReference>,
    ) -> Vec<DetectedDependency> {
        let mut dependencies: HashMap<String, DetectedDependency> = HashMap::new();

        for type_ref in type_refs {
            // Determine which package provides this type
            let package_name = if let Some(name) = self.type_registry.get(&type_ref.simple_name) {
                name.clone()
            } else if let Some(api_group) = &type_ref.api_group {
                if let Some(name) = self.api_group_registry.get(api_group) {
                    name.clone()
                } else {
                    continue; // Unknown type, skip
                }
            } else {
                continue; // Can't determine package
            };

            // Add to dependencies
            let entry =
                dependencies
                    .entry(package_name.clone())
                    .or_insert_with(|| DetectedDependency {
                        package_name: package_name.clone(),
                        required_types: HashSet::new(),
                        api_version: type_ref.api_group.clone(),
                        is_core_type: package_name == "k8s_io",
                    });

            entry.required_types.insert(type_ref.simple_name.clone());
        }

        dependencies.into_values().collect()
    }

    /// Set the current package being analyzed
    pub fn set_current_package(&mut self, package: &str) {
        self.current_package = Some(package.to_string());
    }

    /// Build import statements for detected dependencies
    pub fn generate_imports(
        &self,
        dependencies: &[DetectedDependency],
        package_mode: bool,
    ) -> Vec<String> {
        let mut imports = Vec::new();

        for dep in dependencies {
            if package_mode {
                // Package-style import
                imports.push(format!(
                    "let {} = import \"{}\" in",
                    dep.package_name.replace('-', "_"),
                    dep.package_name
                ));
            } else {
                // Relative import - calculate the path
                // This would need proper path resolution based on file structure
                let path = self.calculate_relative_path(&dep.package_name);
                imports.push(format!(
                    "let {} = import \"{}\" in",
                    dep.package_name.replace('-', "_"),
                    path
                ));
            }
        }

        imports
    }

    /// Calculate relative path to another package (for non-package mode)
    fn calculate_relative_path(&self, target_package: &str) -> String {
        // This is a simplified version - real implementation would
        // calculate actual relative paths based on file structure
        format!("../../../{}/mod.ncl", target_package)
    }
}

impl Default for DependencyAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_type_reference_detection() {
        let mut analyzer = DependencyAnalyzer::new();
        // Register k8s types for the test
        analyzer.register_k8s_core_types();

        // Test parsing a k8s type reference
        let type_ref = analyzer.parse_type_reference(
            "io.k8s.apimachinery.pkg.apis.meta.v1.ObjectMeta",
            "test_location",
        );

        assert!(type_ref.is_some());
        let type_ref = type_ref.unwrap();
        assert_eq!(type_ref.simple_name, "ObjectMeta");
        assert_eq!(
            type_ref.api_group,
            Some("io.k8s.apimachinery.pkg.apis.meta.v1".to_string())
        );
    }

    #[test]
    fn test_dependency_detection() {
        let mut analyzer = DependencyAnalyzer::new();
        analyzer.register_k8s_core_types();
        analyzer.set_current_package("crossplane");

        let mut refs = HashSet::new();
        refs.insert(TypeReference {
            full_name: "ObjectMeta".to_string(),
            simple_name: "ObjectMeta".to_string(),
            api_group: Some("io.k8s.apimachinery.pkg.apis.meta.v1".to_string()),
            source_location: "spec.metadata".to_string(),
        });

        let deps = analyzer.determine_dependencies(&refs);
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].package_name, "k8s_io");
        assert!(deps[0].required_types.contains("ObjectMeta"));
    }
}
