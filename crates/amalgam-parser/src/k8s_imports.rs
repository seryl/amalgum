//! Special handling for k8s.io internal imports

use amalgam_core::types::Type;
use std::collections::HashSet;

/// Analyze a type to find k8s.io type references that need imports
pub fn find_k8s_type_references(ty: &Type) -> HashSet<(String, Option<String>)> {
    let mut refs = HashSet::new();
    collect_references(ty, &mut refs);
    refs
}

fn collect_references(ty: &Type, refs: &mut HashSet<(String, Option<String>)>) {
    match ty {
        Type::Reference { name, module } => {
            // For k8s types with module info, track both name and module
            if module.as_ref().map_or(false, |m| m.contains("k8s")) {
                refs.insert((name.clone(), module.clone()));
            } else if is_k8s_type(name) {
                // For known k8s types without module info
                refs.insert((name.clone(), None));
            }
        }
        Type::Array(inner) => collect_references(inner, refs),
        Type::Optional(inner) => collect_references(inner, refs),
        Type::Map { value, .. } => collect_references(value, refs),
        Type::Record { fields, .. } => {
            for field in fields.values() {
                collect_references(&field.ty, refs);
            }
        }
        Type::Union { types, .. } => {
            for t in types {
                collect_references(t, refs);
            }
        }
        Type::TaggedUnion { variants, .. } => {
            for t in variants.values() {
                collect_references(t, refs);
            }
        }
        Type::Contract { base, .. } => collect_references(base, refs),
        _ => {}
    }
}

/// Check if a type name is a k8s.io type
fn is_k8s_type(name: &str) -> bool {
    // Common k8s.io types that might be referenced
    matches!(
        name,
        "ListMeta"
            | "ObjectMeta"
            | "LabelSelector"
            | "Time"
            | "MicroTime"
            | "Status"
            | "StatusDetails"
            | "StatusCause"
            | "FieldsV1"
            | "ManagedFieldsEntry"
            | "OwnerReference"
            | "Preconditions"
            | "DeleteOptions"
            | "ListOptions"
            | "GetOptions"
            | "WatchEvent"
            | "NodeSelector"  // Added for cross-version references
            | "NodeSelectorTerm"
            | "NodeSelectorRequirement"
            | "PodAffinityTerm"
            | "ResourceRequirements"
            | "Volume"
            | "VolumeMount"
            | "Container"
            | "ContainerPort"
            | "EnvVar"
            | "Probe"
    )
}

/// Generate import statement for a k8s type
pub fn generate_k8s_import(type_name: &str, current_version: &str) -> Option<String> {
    let (import_path, version) = match type_name {
        "ListMeta" | "ObjectMeta" | "LabelSelector" | "Status" | "StatusDetails"
        | "DeleteOptions" | "ListOptions" | "GetOptions" | "WatchEvent" | "ManagedFieldsEntry"
        | "OwnerReference" | "Preconditions" => {
            // These are in meta/v1
            ("../v1", "v1")
        }
        "Time" | "MicroTime" | "FieldsV1" | "StatusCause" => {
            // These are also typically in v1
            ("../v1", "v1")
        }
        _ => return None,
    };

    // Don't import from the same version we're in
    if version == current_version {
        return None;
    }

    Some(format!(
        "let {} = import \"{}/{}.ncl\" in",
        type_name.to_lowercase(),
        import_path,
        type_name.to_lowercase()
    ))
}

/// Fix missing imports in a k8s.io module
pub fn fix_k8s_imports(
    content: &str,
    type_refs: &HashSet<(String, Option<String>)>,
    current_version: &str,
) -> String {
    tracing::debug!(
        "fix_k8s_imports: current_version={}, type_refs={:?}",
        current_version, type_refs
    );
    
    let mut imports = Vec::new();
    let mut replacements = Vec::new();

    for (type_name, module) in type_refs {
        // Check if this is a cross-version reference
        let needs_import = if let Some(module) = module {
            // Extract version from module path
            let parts: Vec<&str> = module.split('.').collect();
            if let Some(version) = parts.last() {
                *version != current_version
            } else {
                false
            }
        } else {
            // Fall back to old logic for types without module info
            generate_k8s_import(type_name, current_version).is_some()
        };
        
        if needs_import {
            // Generate import based on module info if available
            let import_stmt = if let Some(module) = module {
                let parts: Vec<&str> = module.split('.').collect();
                if let Some(version) = parts.last() {
                    format!(
                        "let {}_{} = import \"../{}/{}.ncl\" in",
                        version,
                        type_name.to_lowercase(),
                        version,
                        type_name.to_lowercase()
                    )
                } else {
                    continue;
                }
            } else if let Some(import) = generate_k8s_import(type_name, current_version) {
                import
            } else {
                continue;
            };
            imports.push(import_stmt.clone());
            
            // Determine the prefix based on how we imported it
            let prefix = if let Some(module) = module {
                let parts: Vec<&str> = module.split('.').collect();
                if let Some(version) = parts.last() {
                    format!("{}_{}", version, type_name.to_lowercase())
                } else {
                    type_name.to_lowercase()
                }
            } else {
                type_name.to_lowercase()
            };
            
            // Replace bare type reference with qualified reference
            replacements.push((
                format!("| {}", type_name),
                format!("| {}.{}", prefix, type_name),
            ));
        }
    }

    if imports.is_empty() {
        return content.to_string();
    }

    // Add imports at the beginning and apply replacements
    let mut result = imports.join("\n") + "\n\n" + content;

    for (from, to) in replacements {
        result = result.replace(&from, &to);
    }

    result
}
