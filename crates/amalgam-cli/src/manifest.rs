//! Manifest-based package generation for CI/CD workflows

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// Main manifest configuration
#[derive(Debug, Deserialize, Serialize)]
pub struct Manifest {
    /// Global configuration
    pub config: ManifestConfig,

    /// List of packages to generate
    pub packages: Vec<PackageDefinition>,
}

/// Global configuration for manifest
#[derive(Debug, Deserialize, Serialize)]
pub struct ManifestConfig {
    /// Base output directory for all packages
    pub output_base: PathBuf,

    /// Enable package mode by default
    #[serde(default = "default_true")]
    pub package_mode: bool,

    /// Base package ID for dependencies (e.g., "github:seryl/nickel-pkgs")
    pub base_package_id: String,

    /// Local package path prefix for development (e.g., "examples/pkgs")
    /// When set, generates Path dependencies instead of Index dependencies
    #[serde(default)]
    pub local_package_prefix: Option<String>,
}

/// Definition of a package to generate
#[derive(Debug, Deserialize, Serialize)]
pub struct PackageDefinition {
    /// Package name
    pub name: String,

    /// Type of source (k8s-core, url, crd, openapi)
    #[serde(rename = "type")]
    pub source_type: SourceType,

    /// Version (for k8s-core and package versioning)
    pub version: Option<String>,

    /// URL (for url type)
    pub url: Option<String>,

    /// Git ref (tag, branch, or commit) for URL sources
    pub git_ref: Option<String>,

    /// File path (for crd/openapi types)
    pub file: Option<PathBuf>,

    /// Output directory name
    pub output: String,

    /// Package description
    pub description: String,

    /// Keywords for package discovery
    pub keywords: Vec<String>,

    /// Dependencies on other packages with version constraints
    #[serde(default)]
    pub dependencies: HashMap<String, DependencySpec>,

    /// Whether this package is enabled
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Dependency specification with version constraints
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(untagged)]
pub enum DependencySpec {
    /// Simple string version (for backwards compatibility)
    Simple(String),
    /// Full specification with version constraints
    Full {
        version: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        min_version: Option<String>,
    },
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum SourceType {
    K8sCore,
    Url,
    Crd,
    OpenApi,
}

impl std::fmt::Display for SourceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SourceType::K8sCore => write!(f, "k8s-core"),
            SourceType::Url => write!(f, "url"),
            SourceType::Crd => write!(f, "crd"),
            SourceType::OpenApi => write!(f, "openapi"),
        }
    }
}

fn default_true() -> bool {
    true
}

impl Manifest {
    /// Load manifest from file
    pub fn from_file(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read manifest file: {}", path.display()))?;

        toml::from_str(&content)
            .with_context(|| format!("Failed to parse manifest file: {}", path.display()))
    }

    /// Generate all packages defined in the manifest
    pub async fn generate_all(&self) -> Result<GenerationReport> {
        let mut report = GenerationReport::default();

        // Create output base directory
        fs::create_dir_all(&self.config.output_base).with_context(|| {
            format!(
                "Failed to create output directory: {}",
                self.config.output_base.display()
            )
        })?;

        for package in &self.packages {
            if !package.enabled {
                info!("Skipping disabled package: {}", package.name);
                report.skipped.push(package.name.clone());
                continue;
            }

            info!("Generating package: {}", package.name);

            match self.generate_package(package).await {
                Ok(output_path) => {
                    info!(
                        "✓ Successfully generated {} at {:?}",
                        package.name, output_path
                    );
                    report.successful.push(package.name.clone());
                }
                Err(e) => {
                    warn!("✗ Failed to generate {}: {}", package.name, e);
                    report.failed.push((package.name.clone(), e.to_string()));
                }
            }
        }

        Ok(report)
    }

    /// Generate a single package
    async fn generate_package(&self, package: &PackageDefinition) -> Result<PathBuf> {
        use amalgam_parser::incremental::{detect_change_type, save_fingerprint, ChangeType};

        let output_path = self.config.output_base.join(&package.output);

        // Check if we need to regenerate using intelligent change detection
        let source = self.create_fingerprint_source(package).await?;
        let change_type = detect_change_type(&output_path, source.as_ref())
            .map_err(|e| anyhow::anyhow!("Failed to detect changes: {}", e))?;

        match change_type {
            ChangeType::NoChange => {
                info!("📦 {} - No changes detected, skipping", package.name);
                return Ok(output_path);
            }
            ChangeType::MetadataOnly => {
                info!(
                    "📦 {} - Only metadata changed, updating manifest",
                    package.name
                );
                // Update manifest with new timestamp but keep existing files
                if self.config.package_mode {
                    self.generate_package_manifest(package, &output_path)?;
                }
                // Save new fingerprint with updated metadata
                save_fingerprint(&output_path, source.as_ref())
                    .map_err(|e| anyhow::anyhow!("Failed to save fingerprint: {}", e))?;
                return Ok(output_path);
            }
            ChangeType::ContentChanged => {
                info!("📦 {} - Content changed, regenerating", package.name);
            }
            ChangeType::FirstGeneration => {
                info!("📦 {} - First generation", package.name);
            }
        }

        // Build the command based on source type
        let result = match package.source_type {
            SourceType::K8sCore => self.generate_k8s_core(package, &output_path).await,
            SourceType::Url => self.generate_from_url(package, &output_path).await,
            SourceType::Crd => self.generate_from_crd(package, &output_path).await,
            SourceType::OpenApi => self.generate_from_openapi(package, &output_path).await,
        };

        // Generate package manifest if successful
        if result.is_ok() && self.config.package_mode {
            self.generate_package_manifest(package, &output_path)?;
            // Save fingerprint after successful generation
            save_fingerprint(&output_path, source.as_ref())
                .map_err(|e| anyhow::anyhow!("Failed to save fingerprint: {}", e))?;
        }

        result
    }

    /// Create a fingerprint source for change detection
    async fn create_fingerprint_source(
        &self,
        package: &PackageDefinition,
    ) -> Result<Box<dyn amalgam_core::fingerprint::Fingerprintable>> {
        use amalgam_parser::incremental::*;

        match package.source_type {
            SourceType::K8sCore => {
                let version = package.version.as_deref().unwrap_or("v1.31.0");
                // For k8s core, we would fetch the OpenAPI spec and hash it
                let spec_url = format!(
                    "https://dl.k8s.io/{}/api/openapi-spec/swagger.json",
                    version
                );
                let source = K8sCoreSource {
                    version: version.to_string(),
                    openapi_spec: "".to_string(), // Would be fetched in real implementation
                    spec_url,
                };
                Ok(Box::new(source))
            }
            SourceType::Url => {
                let url = package
                    .url
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("URL required for url type package"))?;

                // Include git ref and version in the fingerprint URL
                let fingerprint_url = if let Some(ref git_ref) = package.git_ref {
                    format!("{}@{}", url, git_ref)
                } else if let Some(ref version) = package.version {
                    format!("{}@{}", url, version)
                } else {
                    url.clone()
                };

                // For URL sources, we would fetch all the URLs and hash their content
                let source = UrlSource {
                    base_url: fingerprint_url.clone(),
                    urls: vec![fingerprint_url], // Simplified - would list all files
                    contents: vec!["".to_string()], // Would be actual content
                };
                Ok(Box::new(source))
            }
            SourceType::Crd | SourceType::OpenApi => {
                // For file-based sources
                let file = package.file.as_ref().ok_or_else(|| {
                    anyhow::anyhow!(
                        "File path required for {:?} type package",
                        package.source_type
                    )
                })?;

                let content = if std::path::Path::new(file).exists() {
                    std::fs::read_to_string(file).unwrap_or_default()
                } else {
                    String::new()
                };

                let source = LocalFilesSource {
                    paths: vec![file.to_string_lossy().to_string()],
                    contents: vec![content],
                };
                Ok(Box::new(source))
            }
        }
    }

    async fn generate_k8s_core(
        &self,
        package: &PackageDefinition,
        output: &Path,
    ) -> Result<PathBuf> {
        use crate::handle_k8s_core_import;

        let version = package.version.as_deref().unwrap_or("v1.31.0");

        info!("Fetching Kubernetes {} core types...", version);
        handle_k8s_core_import(version, output, true).await?;

        Ok(output.to_path_buf())
    }

    async fn generate_from_url(
        &self,
        package: &PackageDefinition,
        output: &Path,
    ) -> Result<PathBuf> {
        let url = package
            .url
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("URL required for url type package"))?;

        // Build URL with git ref if specified
        let fetch_url = if let Some(ref git_ref) = package.git_ref {
            // Replace /tree/main or /tree/master with the specified ref
            if url.contains("/tree/") {
                let parts: Vec<&str> = url.split("/tree/").collect();
                if parts.len() == 2 {
                    let base = parts[0];
                    let path_parts: Vec<&str> = parts[1].split('/').collect();
                    if path_parts.len() > 1 {
                        // Reconstruct with new ref
                        format!("{}/tree/{}/{}", base, git_ref, path_parts[1..].join("/"))
                    } else {
                        format!("{}/tree/{}", base, git_ref)
                    }
                } else {
                    url.clone()
                }
            } else {
                // Append ref if no /tree/ found
                format!("{}/tree/{}", url.trim_end_matches('/'), git_ref)
            }
        } else {
            url.clone()
        };

        info!("Fetching CRDs from URL: {}", fetch_url);
        if package.git_ref.is_some() {
            info!("Using git ref: {}", package.git_ref.as_ref().unwrap());
        }

        // Use the existing URL import functionality
        use amalgam_parser::fetch::CRDFetcher;
        use amalgam_parser::package::PackageGenerator;

        let fetcher = CRDFetcher::new()?;
        let crds = fetcher.fetch_from_url(&fetch_url).await?;
        fetcher.finish();

        info!("Found {} CRDs", crds.len());

        // Generate package structure
        let mut generator = PackageGenerator::new(package.name.clone(), output.to_path_buf());
        generator.add_crds(crds);

        let package_structure = generator.generate_package()?;

        // Create output directory structure
        fs::create_dir_all(output)?;

        // Write main module file
        let main_module = package_structure.generate_main_module();
        fs::write(output.join("mod.ncl"), main_module)?;

        // Generate group/version/kind structure
        for group in package_structure.groups() {
            let group_dir = output.join(&group);
            fs::create_dir_all(&group_dir)?;

            if let Some(group_mod) = package_structure.generate_group_module(&group) {
                fs::write(group_dir.join("mod.ncl"), group_mod)?;
            }

            for version in package_structure.versions(&group) {
                let version_dir = group_dir.join(&version);
                fs::create_dir_all(&version_dir)?;

                // Generate all files for this version using batch generation
                // This ensures proper cross-version imports are generated
                let version_files = package_structure.generate_version_files(&group, &version);
                
                // Write all generated files
                for (filename, content) in version_files {
                    fs::write(version_dir.join(&filename), content)?;
                }
            }
        }

        Ok(output.to_path_buf())
    }

    async fn generate_from_crd(
        &self,
        package: &PackageDefinition,
        output: &Path,
    ) -> Result<PathBuf> {
        let file = package
            .file
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("File path required for crd type package"))?;

        info!("Importing CRD from {:?}", file);

        // TODO: Implement CRD file import
        // This would use the existing CRD import functionality

        Ok(output.to_path_buf())
    }

    async fn generate_from_openapi(
        &self,
        package: &PackageDefinition,
        output: &Path,
    ) -> Result<PathBuf> {
        let file = package
            .file
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("File path required for openapi type package"))?;

        info!("Importing OpenAPI spec from {:?}", file);

        // TODO: Implement OpenAPI import
        // This would use the existing OpenAPI import functionality

        Ok(output.to_path_buf())
    }

    fn generate_package_manifest(&self, package: &PackageDefinition, output: &Path) -> Result<()> {
        use amalgam_codegen::package_mode::PackageMode;
        use chrono::Utc;
        use std::collections::{HashMap, HashSet};
        use std::path::PathBuf;

        // Use the current manifest file for type registry
        let manifest_path = PathBuf::from(".amalgam-manifest.toml");
        let manifest = if manifest_path.exists() {
            Some(&manifest_path)
        } else {
            None
        };
        let _package_mode = PackageMode::new_with_analyzer(manifest);

        // Build a map of package names to their outputs for dependency resolution
        let package_map: HashMap<String, String> = self
            .packages
            .iter()
            .map(|p| (p.output.clone(), p.name.clone()))
            .collect();

        // Scan generated files for dependencies
        let mut detected_deps = HashSet::new();
        if output.exists() {
            // Walk through all generated .ncl files and look for imports
            for entry in walkdir::WalkDir::new(output)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().is_some_and(|ext| ext == "ncl"))
            {
                if let Ok(content) = fs::read_to_string(entry.path()) {
                    // Look for imports - could be any package name from our manifest
                    for line in content.lines() {
                        // Check for imports of any known package
                        for pkg_output in package_map.keys() {
                            let import_pattern = format!("import \"{}\"", pkg_output);
                            if line.contains(&import_pattern) {
                                detected_deps.insert(pkg_output.clone());
                            }
                        }
                    }
                }
            }
        }

        // Format dependencies for the manifest
        // Check if package has explicit dependency constraints
        let deps_str = if detected_deps.is_empty() && package.dependencies.is_empty() {
            "{}".to_string()
        } else {
            let mut dep_entries: Vec<String> = Vec::new();

            // Add detected dependencies with constraints from manifest if available
            for dep_output in &detected_deps {
                // Find the package definition for this dependency
                let dep_package = self.packages.iter().find(|p| &p.output == dep_output);

                let dep_entry = if let Some(dep_pkg) = dep_package {
                    // For production, use Index dependency with version constraints
                    let version = if let Some(ref constraint) =
                        package.dependencies.get(dep_output.as_str())
                    {
                        match constraint {
                            DependencySpec::Simple(v) => v.clone(),
                            DependencySpec::Full { version, .. } => version.clone(),
                        }
                    } else if let Some(ref dep_version) = dep_pkg.version {
                        // Use the package's own version as default
                        dep_version
                            .strip_prefix('v')
                            .unwrap_or(dep_version)
                            .to_string()
                    } else {
                        "*".to_string()
                    };

                    // Build package ID from base_package_id and package name
                    let package_id = format!(
                        "{}/{}",
                        self.config.base_package_id.trim_end_matches('/'),
                        dep_pkg.name
                    );

                    format!(
                        "    {} = 'Index {{ package = \"{}\", version = \"{}\" }}",
                        dep_output, package_id, version
                    )
                } else {
                    // Fallback for unknown packages - still use Index
                    let package_id = format!(
                        "{}/{}",
                        self.config.base_package_id.trim_end_matches('/'),
                        dep_output
                    );
                    format!(
                        "    {} = 'Index {{ package = \"{}\", version = \"*\" }}",
                        dep_output, package_id
                    )
                };
                dep_entries.push(dep_entry);
            }

            // Add any explicit dependencies not auto-detected
            for (dep_name, dep_spec) in &package.dependencies {
                if !detected_deps.contains(dep_name.as_str()) {
                    // Try to find the package in our manifest
                    let dep_package = self
                        .packages
                        .iter()
                        .find(|p| p.output == *dep_name || p.name == *dep_name);

                    // Always use Index dependencies - packages should reference upstream
                    let version = match dep_spec {
                        DependencySpec::Simple(v) => v.clone(),
                        DependencySpec::Full { version, .. } => version.clone(),
                    };

                    // Build package ID based on manifest or fallback
                    let package_id = if let Some(dep_pkg) = dep_package {
                        format!(
                            "{}/{}",
                            self.config.base_package_id.trim_end_matches('/'),
                            dep_pkg.name
                        )
                    } else {
                        // If not in manifest, assume it's an external package
                        format!(
                            "{}/{}",
                            self.config.base_package_id.trim_end_matches('/'),
                            dep_name
                        )
                    };

                    let dep_entry = format!(
                        "    {} = 'Index {{ package = \"{}\", version = \"{}\" }}",
                        dep_name, package_id, version
                    );
                    dep_entries.push(dep_entry);
                }
            }

            format!("{{\n{}\n  }}", dep_entries.join(",\n"))
        };

        // Fix version format - remove 'v' prefix for Nickel packages
        let version = package.version.as_deref().unwrap_or("0.1.0");
        let clean_version = version.strip_prefix('v').unwrap_or(version);

        // Create enhanced manifest with proper metadata
        let now = Utc::now();

        // Build header comments with metadata
        let header = format!(
            r#"# Amalgam Package Manifest
# Generated: {}
# Generator: amalgam v{}
# Source: {}{}
"#,
            now.to_rfc3339(),
            env!("CARGO_PKG_VERSION"),
            package
                .url
                .as_deref()
                .unwrap_or(&format!("{} (local)", package.source_type)),
            if let Some(ref git_ref) = package.git_ref {
                format!("\n# Git ref: {}", git_ref)
            } else {
                String::new()
            }
        );

        let manifest_content = format!(
            r#"{}{{
  # Package identity
  name = "{}",
  version = "{}",
  
  # Package information
  description = "{}",
  authors = ["amalgam"],
  keywords = [{}],
  license = "Apache-2.0",
  
  # Dependencies
  dependencies = {},
  
  # Nickel version requirement
  minimal_nickel_version = "1.9.0",
}} | std.package.Manifest
"#,
            header,
            package.name,
            clean_version,
            package.description,
            package
                .keywords
                .iter()
                .map(|k| format!("\"{}\"", k))
                .collect::<Vec<_>>()
                .join(", "),
            deps_str
        );

        // Write manifest file
        let manifest_path = output.join("Nickel-pkg.ncl");
        fs::write(manifest_path, manifest_content)?;

        Ok(())
    }
}

/// Report of package generation results
#[derive(Debug, Default)]
pub struct GenerationReport {
    pub successful: Vec<String>,
    pub failed: Vec<(String, String)>,
    pub skipped: Vec<String>,
}

impl GenerationReport {
    /// Print a summary of the generation results
    pub fn print_summary(&self) {
        println!("\n=== Package Generation Summary ===");

        if !self.successful.is_empty() {
            println!(
                "\n✓ Successfully generated {} packages:",
                self.successful.len()
            );
            for name in &self.successful {
                println!("  - {}", name);
            }
        }

        if !self.failed.is_empty() {
            println!("\n✗ Failed to generate {} packages:", self.failed.len());
            for (name, error) in &self.failed {
                println!("  - {}: {}", name, error);
            }
        }

        if !self.skipped.is_empty() {
            println!("\n⊘ Skipped {} disabled packages:", self.skipped.len());
            for name in &self.skipped {
                println!("  - {}", name);
            }
        }

        let total = self.successful.len() + self.failed.len() + self.skipped.len();
        println!("\nTotal: {} packages processed", total);
    }
}
