//! Library interface for amalgam CLI components

pub mod manifest;
pub mod validate;
mod vendor;

use amalgam_parser::k8s_types::K8sTypesFetcher;
use anyhow::Result;
use std::fs;
use std::path::Path;
use tracing::info;

pub async fn handle_k8s_core_import(
    version: &str,
    output_dir: &Path,
    nickel_package: bool,
) -> Result<()> {
    info!(
        "Fetching Kubernetes {} core types using unified pipeline...",
        version
    );

    // Create fetcher
    let fetcher = K8sTypesFetcher::new();

    // Fetch the OpenAPI schema
    let openapi = fetcher.fetch_k8s_openapi(version).await?;

    // Extract core types
    let types_map = fetcher.extract_core_types(&openapi)?;

    let total_types = types_map.len();
    info!("Extracted {} core types", total_types);

    // Create a NamespacedPackage to use the unified pipeline
    let mut package = amalgam_parser::package::NamespacedPackage::new("k8s.io".to_string());

    // Add all types to the package
    for (type_ref, type_def) in types_map {
        package.add_type(
            "k8s.io".to_string(),
            type_ref.version.clone(),
            type_ref.kind.clone(),
            type_def,
        );
    }

    // Get all versions that have types
    let versions = package.versions("k8s.io");

    info!("Processing {} versions", versions.len());

    // Generate files for each version using the unified pipeline
    for version_name in versions {
        let files = package.generate_version_files("k8s.io", &version_name);

        // Write files to disk
        let version_dir = output_dir.join(&version_name);
        fs::create_dir_all(&version_dir)?;

        for (filename, content) in files {
            let file_path = version_dir.join(&filename);
            fs::write(&file_path, content)?;
            info!("Generated {:?}", file_path);
        }
    }

    // Generate main package mod.ncl if requested
    if nickel_package {
        let mut version_imports = Vec::new();
        let versions = package.versions("k8s.io");

        for version in versions {
            version_imports.push(format!("  {} = import \"./{}/mod.ncl\",", version, version));
        }

        let package_content = format!(
            "# Kubernetes Core Types Package\n# Generated with unified IR pipeline\n\n{{\n{}\n}}\n",
            version_imports.join("\n")
        );

        let package_path = output_dir.join("mod.ncl");
        fs::write(&package_path, package_content)?;
        info!("Generated package module {:?}", package_path);
    }

    info!(
        "✅ Successfully generated {} Kubernetes {} types with proper cross-version imports",
        total_types, version
    );

    Ok(())
}
