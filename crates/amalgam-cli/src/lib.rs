//! Library interface for amalgam CLI components

pub mod manifest;
pub mod validate;
mod vendor;

use amalgam_codegen::nickel::NickelCodegen;
use amalgam_codegen::Codegen;
use amalgam_parser::k8s_types::K8sTypesFetcher;
use anyhow::Result;
use std::fs;
use std::path::Path;
use tracing::info;

fn is_core_k8s_type(name: &str) -> bool {
    matches!(
        name,
        "ObjectMeta"
            | "ListMeta"
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
            | "Condition"
            | "TypeMeta"
            | "APIResource"
            | "APIResourceList"
            | "APIGroup"
            | "APIGroupList"
            | "APIVersions"
            | "GroupVersionForDiscovery"
    )
}

fn is_unversioned_k8s_type(name: &str) -> bool {
    matches!(
        name,
        "RawExtension" | "IntOrString" // runtime.RawExtension and intstr.IntOrString unversioned types
    )
}

fn collect_type_references(
    ty: &amalgam_core::types::Type,
    refs: &mut std::collections::HashSet<String>,
) {
    use amalgam_core::types::Type;

    match ty {
        Type::Reference { name, module } => {
            // Include module info if present
            let full_name = if let Some(module) = module {
                format!("{}.{}", module, name)
            } else {
                name.clone()
            };
            refs.insert(full_name);
        }
        Type::Array(inner) => collect_type_references(inner, refs),
        Type::Optional(inner) => collect_type_references(inner, refs),
        Type::Map { value, .. } => collect_type_references(value, refs),
        Type::Record { fields, .. } => {
            for field in fields.values() {
                collect_type_references(&field.ty, refs);
            }
        }
        Type::Union { types, .. } => {
            for t in types {
                collect_type_references(t, refs);
            }
        }
        Type::TaggedUnion { variants, .. } => {
            for t in variants.values() {
                collect_type_references(t, refs);
            }
        }
        Type::Contract { base, .. } => collect_type_references(base, refs),
        _ => {}
    }
}

fn apply_type_replacements(
    ty: &mut amalgam_core::types::Type,
    replacements: &std::collections::HashMap<String, String>,
) {
    use amalgam_core::types::Type;

    match ty {
        Type::Reference { name, .. } => {
            if let Some(replacement) = replacements.get(name) {
                *name = replacement.clone();
            }
        }
        Type::Array(inner) => apply_type_replacements(inner, replacements),
        Type::Optional(inner) => apply_type_replacements(inner, replacements),
        Type::Map { value, .. } => apply_type_replacements(value, replacements),
        Type::Record { fields, .. } => {
            for field in fields.values_mut() {
                apply_type_replacements(&mut field.ty, replacements);
            }
        }
        Type::Union { types, .. } => {
            for t in types {
                apply_type_replacements(t, replacements);
            }
        }
        Type::TaggedUnion { variants, .. } => {
            for t in variants.values_mut() {
                apply_type_replacements(t, replacements);
            }
        }
        Type::Contract { base, .. } => apply_type_replacements(base, replacements),
        _ => {}
    }
}

pub async fn handle_k8s_core_import(
    version: &str,
    output_dir: &Path,
    nickel_package: bool,
) -> Result<()> {
    info!("Fetching Kubernetes {} core types...", version);

    // Create fetcher
    let fetcher = K8sTypesFetcher::new();

    // Fetch the OpenAPI schema
    let openapi = fetcher.fetch_k8s_openapi(version).await?;

    // Extract core types
    let types = fetcher.extract_core_types(&openapi)?;

    let total_types = types.len();
    info!("Extracted {} core types", total_types);

    // Group types by version
    let mut types_by_version: std::collections::HashMap<
        String,
        Vec<(
            amalgam_parser::imports::TypeReference,
            amalgam_core::ir::TypeDefinition,
        )>,
    > = std::collections::HashMap::new();

    for (type_ref, type_def) in types {
        types_by_version
            .entry(type_ref.version.clone())
            .or_default()
            .push((type_ref, type_def));
    }

    // Generate files for each version
    for (version, version_types) in &types_by_version {
        let version_dir = output_dir.join(version);
        fs::create_dir_all(&version_dir)?;

        let mut mod_imports = Vec::new();

        // Generate each type in its own file
        for (type_ref, type_def) in version_types {
            // Check if this type references other types in the same version
            let mut imports = Vec::new();
            let mut type_replacements = std::collections::HashMap::new();

            // Collect any references to other types in the same module
            let mut referenced_types = std::collections::HashSet::new();
            collect_type_references(&type_def.ty, &mut referenced_types);
            
            if type_ref.kind == "VolumeAttributesClass" {
                tracing::info!(
                    "VolumeAttributesClass references: {:?}",
                    referenced_types
                );
            }

            // For each referenced type, check if it exists in the same version
            for referenced in &referenced_types {
                // Extract the type name from the full path (e.g., "io.k8s.api.core.v1.ObjectMeta" -> "ObjectMeta")
                let type_name = if referenced.contains('.') {
                    referenced.split('.').last().unwrap_or(referenced.as_str())
                } else {
                    referenced.as_str()
                };
                
                if type_name != &type_ref.kind {
                    // Check if this type exists in the same version
                    if version_types.iter().any(|(tr, _)| tr.kind == type_name) {
                        // Add import for the type in the same directory
                        let alias = type_name.to_lowercase();
                        imports.push(amalgam_core::ir::Import {
                            path: format!("./{}.ncl", alias),
                            alias: Some(alias.clone()),
                            items: vec![type_name.to_string()],
                        });

                        // Store replacement: ManagedFieldsEntry -> managedfieldsentry.ManagedFieldsEntry
                        type_replacements
                            .insert(type_name.to_string(), format!("{}.{}", alias, type_name));
                    } else if is_core_k8s_type(type_name) {
                        // Check if this is a core k8s type that should be imported from v1
                        // Common core types are usually in v1 even when referenced from other versions
                        let source_version = "v1";
                        if version != source_version {
                            // Import from v1 directory
                            let alias = type_name; // Use the actual type name as alias
                            imports.push(amalgam_core::ir::Import {
                                path: format!(
                                    "../{}/{}.ncl",
                                    source_version,
                                    type_name.to_lowercase()
                                ),
                                alias: Some(alias.to_string()),
                                items: vec![],
                            });

                            // Store replacement: Type remains as Type (e.g., ObjectMeta remains as ObjectMeta)
                            // No need to qualify since we're importing with the same name
                        }
                    } else if is_unversioned_k8s_type(type_name) {
                        // Check if this is an unversioned k8s type (like RawExtension)
                        // These types are placed in v0 directory
                        let source_version = "v0";
                        if version != source_version {
                            // Import from v0 directory
                            let alias = type_name; // Use the actual type name as alias
                            imports.push(amalgam_core::ir::Import {
                                path: format!(
                                    "../{}/{}.ncl",
                                    source_version,
                                    type_name.to_lowercase()
                                ),
                                alias: Some(alias.to_string()),
                                items: vec![],
                            });
                        }
                    }
                }
            }

            // Apply type replacements to the type definition
            let mut updated_type_def = type_def.clone();
            apply_type_replacements(&mut updated_type_def.ty, &type_replacements);

            // Create a module with the type and its imports
            let module = amalgam_core::ir::Module {
                name: format!(
                    "k8s.io.{}.{}",
                    type_ref.version,
                    type_ref.kind.to_lowercase()
                ),
                imports,
                types: vec![updated_type_def],
                constants: vec![],
                metadata: Default::default(),
            };

            // Create IR with the module
            let mut ir = amalgam_core::IR::new();
            ir.add_module(module);

            // Generate Nickel code
            let mut codegen = NickelCodegen::new();
            let code = codegen.generate(&ir)?;

            // Write to file
            let filename = format!("{}.ncl", type_ref.kind.to_lowercase());
            let file_path = version_dir.join(&filename);
            fs::write(&file_path, code)?;

            info!("Generated {:?}", file_path);

            // Add to module imports
            mod_imports.push(format!(
                "  {} = (import \"./{}\").{},",
                type_ref.kind, filename, type_ref.kind
            ));
        }

        // Generate mod.ncl for this version
        let mod_content = format!(
            "# Kubernetes core {} types\n{{\n{}\n}}\n",
            version,
            mod_imports.join("\n")
        );
        fs::write(version_dir.join("mod.ncl"), mod_content)?;
    }

    // Generate top-level mod.ncl with all versions
    let mut version_imports = Vec::new();
    for version in types_by_version.keys() {
        version_imports.push(format!("  {} = import \"./{}/mod.ncl\",", version, version));
    }

    let root_mod_content = format!(
        "# Kubernetes core types\n{{\n{}\n}}\n",
        version_imports.join("\n")
    );
    fs::write(output_dir.join("mod.ncl"), root_mod_content)?;

    // Generate Nickel package manifest if requested
    if nickel_package {
        info!("Generating Nickel package manifest (experimental)");

        use amalgam_codegen::nickel_package::{NickelPackageConfig, NickelPackageGenerator};

        let config = NickelPackageConfig {
            name: "k8s-io".to_string(),
            version: "0.1.0".to_string(),
            minimal_nickel_version: "1.9.0".to_string(),
            description: format!("Kubernetes {} core type definitions for Nickel", version),
            authors: vec!["amalgam".to_string()],
            license: "Apache-2.0".to_string(),
            keywords: vec![
                "kubernetes".to_string(),
                "k8s".to_string(),
                "types".to_string(),
            ],
        };

        let generator = NickelPackageGenerator::new(config);

        // Convert types to modules for manifest generation
        let modules: Vec<amalgam_core::ir::Module> = types_by_version
            .keys()
            .map(|ver| amalgam_core::ir::Module {
                name: ver.clone(),
                imports: Vec::new(),
                types: Vec::new(),
                constants: Vec::new(),
                metadata: Default::default(),
            })
            .collect();

        let manifest = generator
            .generate_manifest(&modules, std::collections::HashMap::new())
            .unwrap_or_else(|e| format!("# Error generating manifest: {}\n", e));

        fs::write(output_dir.join("Nickel-pkg.ncl"), manifest)?;
        info!("✓ Generated Nickel-pkg.ncl");
    }

    info!(
        "Successfully generated {} k8s core types in {:?}",
        total_types, output_dir
    );
    if nickel_package {
        info!("  with Nickel package manifest");
    }
    Ok(())
}
