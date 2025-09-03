//! Vendor package management
#![allow(dead_code)]

use amalgam_parser::fetch::CRDFetcher;
use amalgam_parser::package::PackageGenerator;
use anyhow::Result;
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Subcommand)]
pub enum VendorCommand {
    /// Install dependencies from nickel.toml
    Install,

    /// Add a new dependency to vendor/
    Add {
        /// Package specification (e.g., "crossplane.io@1.14.0")
        package: String,

        /// Source URL for the package
        #[arg(long)]
        source: Option<String>,
    },

    /// Fetch and vendor types from a URL
    Fetch {
        /// URL to fetch CRDs from
        #[arg(long)]
        url: String,

        /// Package name
        #[arg(long)]
        name: Option<String>,

        /// Package version
        #[arg(long)]
        version: Option<String>,
    },

    /// List vendored packages
    List,

    /// Update all vendored packages
    Update,

    /// Clean vendor directory
    Clean,
}

/// Package manifest for vendored packages
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageManifest {
    pub package: PackageInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageInfo {
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub source: PackageSource,
    pub generated: GenerationInfo,
    pub dependencies: Vec<Dependency>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageSource {
    #[serde(rename = "type")]
    pub source_type: String,
    pub url: String,
    #[serde(rename = "ref")]
    pub reference: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationInfo {
    pub tool: String,
    pub version: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dependency {
    pub name: String,
    pub version: String,
}

/// Project manifest (nickel.toml)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectManifest {
    pub project: ProjectInfo,
    #[serde(default)]
    pub dependencies: HashMap<String, DependencySpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DependencySpec {
    Version(String),
    Detailed {
        version: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        source: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
}

pub struct VendorManager {
    project_root: PathBuf,
    vendor_dir: PathBuf,
}

impl VendorManager {
    pub fn new(project_root: PathBuf) -> Self {
        let vendor_dir = project_root.join("vendor");
        Self {
            project_root,
            vendor_dir,
        }
    }

    /// Execute a vendor command
    pub async fn execute(&self, command: VendorCommand) -> Result<()> {
        match command {
            VendorCommand::Install => self.install().await,
            VendorCommand::Add { package, source } => self.add(&package, source.as_deref()).await,
            VendorCommand::Fetch { url, name, version } => {
                self.fetch(&url, name.as_deref(), version.as_deref()).await
            }
            VendorCommand::List => self.list(),
            VendorCommand::Update => self.update().await,
            VendorCommand::Clean => self.clean(),
        }
    }

    /// Install dependencies from nickel.toml
    async fn install(&self) -> Result<()> {
        let manifest_path = self.project_root.join("nickel.toml");
        if !manifest_path.exists() {
            eprintln!("No nickel.toml found. Run 'amalgam init' first.");
            return Ok(());
        }

        let manifest = self.read_project_manifest()?;

        println!("Installing dependencies...");
        for (name, spec) in &manifest.dependencies {
            println!("  Installing {}...", name);
            self.install_dependency(name, spec).await?;
        }

        println!("Done.");
        Ok(())
    }

    /// Add a new dependency
    async fn add(&self, package: &str, source: Option<&str>) -> Result<()> {
        // Parse package specification (name@version)
        let (name, version) = if let Some(at_pos) = package.find('@') {
            (&package[..at_pos], Some(&package[at_pos + 1..]))
        } else {
            (package, None)
        };

        println!("Adding {} to vendor/", name);

        // Determine source
        let source_url = source.unwrap_or_else(|| {
            // Default sources for known packages
            match name {
                "crossplane.io" => {
                    "https://github.com/crossplane/crossplane/tree/master/cluster/crds"
                }
                "k8s.io" => "https://github.com/kubernetes/kubernetes/tree/master/api/openapi-spec",
                _ => panic!("Unknown package '{}'. Please specify --source", name),
            }
        });

        self.fetch_and_vendor(name, source_url, version).await?;

        // Update nickel.toml
        self.update_project_manifest(name, version.unwrap_or("latest"))?;

        println!("Added {} to vendor/", name);
        Ok(())
    }

    /// Fetch and vendor types from a URL
    async fn fetch(&self, url: &str, name: Option<&str>, version: Option<&str>) -> Result<()> {
        let package_name = name.unwrap_or_else(|| {
            // Extract name from URL
            if url.contains("crossplane") {
                "crossplane.io"
            } else if url.contains("kubernetes") {
                "k8s.io"
            } else {
                "custom"
            }
        });

        self.fetch_and_vendor(package_name, url, version).await?;
        Ok(())
    }

    /// List vendored packages
    fn list(&self) -> Result<()> {
        if !self.vendor_dir.exists() {
            println!("No vendor directory found.");
            return Ok(());
        }

        println!("Vendored packages:");
        for entry in fs::read_dir(&self.vendor_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let package_name = entry.file_name();
                let manifest_path = entry.path().join("manifest.ncl");

                if manifest_path.exists() {
                    // Read manifest for version info
                    let manifest_content = fs::read_to_string(&manifest_path)?;
                    // Simple extraction (proper parsing would use Nickel parser)
                    if let Some(version_line) = manifest_content
                        .lines()
                        .find(|line| line.contains("version ="))
                    {
                        if let Some(version) = version_line.split('"').nth(1) {
                            println!("  {} @ {}", package_name.to_string_lossy(), version);
                            continue;
                        }
                    }
                }
                println!("  {}", package_name.to_string_lossy());
            }
        }
        Ok(())
    }

    /// Update all vendored packages
    async fn update(&self) -> Result<()> {
        println!("Updating vendored packages...");

        let manifest = self.read_project_manifest()?;
        for (name, spec) in &manifest.dependencies {
            println!("  Updating {}...", name);
            self.install_dependency(name, spec).await?;
        }

        println!("Done.");
        Ok(())
    }

    /// Clean vendor directory
    fn clean(&self) -> Result<()> {
        if self.vendor_dir.exists() {
            println!("Cleaning vendor directory...");
            fs::remove_dir_all(&self.vendor_dir)?;
            println!("Done.");
        } else {
            println!("No vendor directory to clean.");
        }
        Ok(())
    }

    /// Helper: Fetch and vendor a package
    async fn fetch_and_vendor(&self, name: &str, url: &str, version: Option<&str>) -> Result<()> {
        // Create vendor directory if it doesn't exist
        fs::create_dir_all(&self.vendor_dir)?;

        let package_dir = self.vendor_dir.join(name);

        // Fetch CRDs
        let fetcher = CRDFetcher::new()?;
        let crds = fetcher.fetch_from_url(url).await?;
        fetcher.finish(); // Clear progress bars

        println!("Found {} CRDs", crds.len());

        // Generate package
        let mut generator = PackageGenerator::new(name.to_string(), package_dir.clone());
        generator.add_crds(crds);
        let package = generator.generate_package()?;

        // Write package files
        self.write_package_files(&package_dir, &package)?;

        // Create manifest
        self.create_package_manifest(name, version.unwrap_or("latest"), url)?;

        Ok(())
    }

    /// Helper: Install a dependency
    async fn install_dependency(&self, name: &str, spec: &DependencySpec) -> Result<()> {
        match spec {
            DependencySpec::Version(version) => {
                // Use default source for known packages
                let source = match name {
                    "crossplane.io" => {
                        "https://github.com/crossplane/crossplane/tree/master/cluster/crds"
                    }
                    "k8s.io" => {
                        "https://github.com/kubernetes/kubernetes/tree/master/api/openapi-spec"
                    }
                    _ => return Err(anyhow::anyhow!("Unknown package '{}'", name)),
                };
                self.fetch_and_vendor(name, source, Some(version)).await
            }
            DependencySpec::Detailed {
                version,
                source,
                path,
            } => {
                if let Some(path) = path {
                    // Local dependency
                    self.link_local_dependency(name, path)
                } else if let Some(source) = source {
                    self.fetch_and_vendor(name, source, Some(version)).await
                } else {
                    Err(anyhow::anyhow!("No source specified for {}", name))
                }
            }
        }
    }

    /// Helper: Link a local dependency
    fn link_local_dependency(&self, name: &str, path: &str) -> Result<()> {
        let source_path = self.project_root.join(path);
        let target_path = self.vendor_dir.join(name);

        if !source_path.exists() {
            return Err(anyhow::anyhow!("Local path {} does not exist", path));
        }

        // Create symlink or copy
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink(&source_path, &target_path)?;
        }

        #[cfg(not(unix))]
        {
            // On Windows or other platforms, copy the directory
            copy_dir_all(&source_path, &target_path)?;
        }

        Ok(())
    }

    /// Helper: Write package files
    fn write_package_files(
        &self,
        package_dir: &Path,
        package: &amalgam_parser::package::NamespacedPackage,
    ) -> Result<()> {
        // Create package directory
        fs::create_dir_all(package_dir)?;

        // Write main module file
        let mod_content = package.generate_main_module();
        fs::write(package_dir.join("mod.ncl"), mod_content)?;

        // Write group/version/kind structure
        for group in package.groups() {
            let group_dir = package_dir.join(&group);
            fs::create_dir_all(&group_dir)?;

            // Write group module
            if let Some(group_mod) = package.generate_group_module(&group) {
                fs::write(group_dir.join("mod.ncl"), group_mod)?;
            }

            // Create version directories
            for version in package.versions(&group) {
                let version_dir = group_dir.join(&version);
                fs::create_dir_all(&version_dir)?;

                // Generate all files for this version using batch generation
                // This ensures proper cross-version imports are generated
                let version_files = package.generate_version_files(&group, &version);
                
                // Write all generated files
                for (filename, content) in version_files {
                    fs::write(version_dir.join(&filename), content)?;
                }
            }
        }

        Ok(())
    }

    /// Helper: Create package manifest
    fn create_package_manifest(&self, name: &str, version: &str, source: &str) -> Result<()> {
        let manifest = PackageManifest {
            package: PackageInfo {
                name: name.to_string(),
                version: version.to_string(),
                description: Some(format!("Auto-generated types for {}", name)),
                source: PackageSource {
                    source_type: "url".to_string(),
                    url: source.to_string(),
                    reference: None,
                    path: None,
                },
                generated: GenerationInfo {
                    tool: "amalgam".to_string(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                },
                dependencies: vec![],
            },
        };

        let manifest_path = self.vendor_dir.join(name).join("manifest.ncl");
        let manifest_content = format!(
            r#"# Package manifest for {}
{{
  package = {{
    name = "{}",
    version = "{}",
    description = "{}",
    source = {{
      type = "{}",
      url = "{}",
    }},
    generated = {{
      tool = "{}",
      version = "{}",
      timestamp = "{}",
    }},
    dependencies = [],
  }},
}}
"#,
            name,
            manifest.package.name,
            manifest.package.version,
            manifest
                .package
                .description
                .as_ref()
                .unwrap_or(&String::new()),
            manifest.package.source.source_type,
            manifest.package.source.url,
            manifest.package.generated.tool,
            manifest.package.generated.version,
            manifest.package.generated.timestamp,
        );

        fs::write(manifest_path, manifest_content)?;
        Ok(())
    }

    /// Helper: Read project manifest
    fn read_project_manifest(&self) -> Result<ProjectManifest> {
        let manifest_path = self.project_root.join("nickel.toml");
        let content = fs::read_to_string(manifest_path)?;
        let manifest: ProjectManifest = toml::from_str(&content)?;
        Ok(manifest)
    }

    /// Helper: Update project manifest
    fn update_project_manifest(&self, name: &str, version: &str) -> Result<()> {
        let manifest_path = self.project_root.join("nickel.toml");

        let mut manifest = if manifest_path.exists() {
            self.read_project_manifest()?
        } else {
            // Create new manifest
            ProjectManifest {
                project: ProjectInfo {
                    name: "my-project".to_string(),
                    version: "0.1.0".to_string(),
                },
                dependencies: HashMap::new(),
            }
        };

        // Add or update dependency
        manifest.dependencies.insert(
            name.to_string(),
            DependencySpec::Version(version.to_string()),
        );

        // Write back
        let content = toml::to_string_pretty(&manifest)?;
        fs::write(manifest_path, content)?;

        Ok(())
    }
}

#[cfg(not(unix))]
fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &dst.join(entry.file_name()))?;
        } else {
            fs::copy(entry.path(), dst.join(entry.file_name()))?;
        }
    }
    Ok(())
}
