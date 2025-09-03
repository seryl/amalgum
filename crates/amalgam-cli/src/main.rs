use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::fs;
use std::path::PathBuf;
use tracing::info;

use amalgam_codegen::{go::GoCodegen, nickel::NickelCodegen, Codegen};
use amalgam_parser::{
    crd::{CRDParser, CRD},
    openapi::OpenAPIParser,
    walkers::SchemaWalker,
    Parser as SchemaParser,
};

mod manifest;
mod validate;
mod vendor;

#[derive(Parser)]
#[command(name = "amalgam")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = "Generate type-safe Nickel configurations from any schema source", long_about = None)]
struct Cli {
    /// Enable verbose output
    #[arg(short, long)]
    verbose: bool,

    /// Enable debug output
    #[arg(short, long)]
    debug: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Import types from various sources
    Import {
        #[command(subcommand)]
        source: ImportSource,
    },

    /// Generate code from IR
    Generate {
        /// Input IR file (JSON format)
        #[arg(short, long)]
        input: PathBuf,

        /// Output file path
        #[arg(short, long)]
        output: PathBuf,

        /// Target language
        #[arg(short, long, default_value = "nickel")]
        target: String,
    },

    /// Convert from one format to another
    Convert {
        /// Input file path
        #[arg(short, long)]
        input: PathBuf,

        /// Input format (crd, openapi, go)
        #[arg(short = 'f', long)]
        from: String,

        /// Output file path
        #[arg(short, long)]
        output: PathBuf,

        /// Output format (nickel, go, ir)
        #[arg(short, long)]
        to: String,
    },

    /// Vendor package management
    Vendor {
        #[command(subcommand)]
        command: vendor::VendorCommand,
    },

    /// Validate a Nickel package
    Validate {
        /// Path to the Nickel package or file to validate
        #[arg(short, long)]
        path: PathBuf,

        /// Package path prefix for dependency resolution (e.g., examples/pkgs)
        #[arg(long)]
        package_path: Option<PathBuf>,

        /// Enable verbose output
        #[arg(short, long)]
        verbose: bool,
    },

    /// Generate packages from a manifest file
    GenerateFromManifest {
        /// Path to the manifest file (TOML format)
        #[arg(short, long, default_value = ".amalgam-manifest.toml")]
        manifest: PathBuf,

        /// Only generate specific packages (by name)
        #[arg(short, long)]
        packages: Vec<String>,

        /// Dry run - show what would be generated without doing it
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum ImportSource {
    /// Import from Kubernetes CRD
    Crd {
        /// CRD file path (YAML or JSON)
        #[arg(short, long)]
        file: PathBuf,

        /// Output file path
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Generate as submittable package (with package imports)
        #[arg(long)]
        package_mode: bool,
    },

    /// Import CRDs from URL (GitHub repo, directory, or direct file)
    Url {
        /// URL to fetch CRDs from
        #[arg(short, long)]
        url: String,

        /// Output directory for package
        #[arg(short, long)]
        output: PathBuf,

        /// Package name (defaults to last part of URL)
        #[arg(short, long)]
        package: Option<String>,

        /// Generate Nickel package manifest (experimental)
        #[arg(long)]
        nickel_package: bool,

        /// Base directory for package resolution (defaults to current directory)
        #[arg(long, env = "AMALGAM_PACKAGE_BASE")]
        package_base: Option<PathBuf>,
    },

    /// Import from OpenAPI specification
    OpenApi {
        /// OpenAPI spec file path (YAML or JSON)
        #[arg(short, long)]
        file: PathBuf,

        /// Output file path
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Import core Kubernetes types from upstream OpenAPI
    K8sCore {
        /// Kubernetes version (e.g., "v1.31.0", "master")
        #[arg(short, long, default_value = "v1.33.4")]
        version: String,

        /// Output directory for generated types
        #[arg(short, long, default_value = "k8s_io")]
        output: PathBuf,

        /// Specific types to import (if empty, imports common types)
        #[arg(short, long)]
        types: Vec<String>,

        /// Generate Nickel package manifest (experimental)
        #[arg(long)]
        nickel_package: bool,

        /// Base directory for package resolution (defaults to current directory)
        #[arg(long, env = "AMALGAM_PACKAGE_BASE")]
        package_base: Option<PathBuf>,
    },

    /// Import from Kubernetes cluster (not implemented)
    K8s {
        /// Kubernetes context to use
        #[arg(short, long)]
        context: Option<String>,

        /// CRD group to import
        #[arg(short, long)]
        group: Option<String>,

        /// Output directory
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize tracing
    let level = if cli.debug {
        tracing::Level::TRACE
    } else if cli.verbose {
        tracing::Level::DEBUG
    } else {
        tracing::Level::INFO
    };

    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_target(cli.debug) // Show target module in debug mode
        .init();

    match cli.command {
        Some(Commands::Import { source }) => handle_import(source).await,
        Some(Commands::Generate {
            input,
            output,
            target,
        }) => handle_generate(input, output, &target),
        Some(Commands::Convert {
            input,
            from,
            output,
            to,
        }) => handle_convert(input, &from, output, &to),
        Some(Commands::Vendor { command }) => {
            let project_root = std::env::current_dir()?;
            let manager = vendor::VendorManager::new(project_root);
            manager.execute(command).await
        }
        Some(Commands::Validate {
            path,
            package_path,
            verbose: _,
        }) => validate::run_validation_with_package_path(&path, package_path.as_deref()),
        Some(Commands::GenerateFromManifest {
            manifest,
            packages,
            dry_run,
        }) => handle_manifest_generation(manifest, packages, dry_run).await,
        None => {
            // No command provided, show help
            use clap::CommandFactory;
            Cli::command().print_help()?;
            Ok(())
        }
    }
}

async fn handle_import(source: ImportSource) -> Result<()> {
    match source {
        ImportSource::Url {
            url,
            output,
            package,
            nickel_package,
            package_base: _,
        } => {
            info!("Fetching CRDs from URL: {}", url);

            // Determine package name
            let package_name = package.unwrap_or_else(|| {
                url.split('/')
                    .next_back()
                    .unwrap_or("generated")
                    .trim_end_matches(".yaml")
                    .trim_end_matches(".yml")
                    .to_string()
            });

            // Fetch CRDs
            let fetcher = amalgam_parser::fetch::CRDFetcher::new()?;
            let crds = fetcher.fetch_from_url(&url).await?;
            fetcher.finish(); // Clear progress bars when done

            info!("Found {} CRDs", crds.len());

            // Use unified pipeline with NamespacedPackage
            // Parse all CRDs and organize by group
            let mut packages_by_group: std::collections::HashMap<String, amalgam_parser::package::NamespacedPackage> = std::collections::HashMap::new();
            
            for crd in crds {
                let group = crd.spec.group.clone();
                
                // Get or create package for this group
                let package = packages_by_group.entry(group.clone())
                    .or_insert_with(|| amalgam_parser::package::NamespacedPackage::new(group.clone()));
                
                // Parse CRD to get types
                let parser = CRDParser::new();
                let temp_ir = parser.parse(crd.clone())?;
                
                // Add types from the parsed IR to the package
                for module in &temp_ir.modules {
                    for type_def in &module.types {
                        // Extract version from module name
                        let parts: Vec<&str> = module.name.split('.').collect();
                        let version = if parts.len() > 2 {
                            parts[parts.len() - 2]
                        } else {
                            "v1"
                        };
                        
                        package.add_type(
                            group.clone(),
                            version.to_string(),
                            type_def.name.clone(),
                            type_def.clone(),
                        );
                    }
                }
            }
            
            // Create output directory structure
            fs::create_dir_all(&output)?;
            
            // Generate files for each group using unified pipeline
            let mut all_groups = Vec::new();
            for (group, package) in packages_by_group {
                all_groups.push(group.clone());
                let group_dir = output.join(&group);
                fs::create_dir_all(&group_dir)?;
                
                // Get all versions for this group
                let versions = package.versions(&group);
                
                // Generate version directories and files
                let mut version_modules = Vec::new();
                for version in versions {
                    let version_dir = group_dir.join(&version);
                    fs::create_dir_all(&version_dir)?;
                    
                    // Generate all files for this version using unified pipeline
                    let version_files = package.generate_version_files(&group, &version);
                    
                    // Write all generated files
                    for (filename, content) in version_files {
                        fs::write(version_dir.join(&filename), content)?;
                    }
                    
                    version_modules.push(format!("  {} = import \"./{}/mod.ncl\",", version, version));
                }
                
                // Write group module
                if !version_modules.is_empty() {
                    let group_mod = format!(
                        "# Module: {}\n# Generated with unified pipeline\n\n{{\n{}\n}}\n",
                        group,
                        version_modules.join("\n")
                    );
                    fs::write(group_dir.join("mod.ncl"), group_mod)?;
                }
            }
            
            // Write main module file
            let group_imports: Vec<String> = all_groups.iter()
                .map(|g| {
                    let sanitized = g.replace(['.', '-'], "_");
                    format!("  {} = import \"./{}/mod.ncl\",", sanitized, g)
                })
                .collect();
                
            let main_module = format!(
                "# Package: {}\n# Generated with unified pipeline\n\n{{\n{}\n}}\n",
                package_name,
                group_imports.join("\n")
            );
            fs::write(output.join("mod.ncl"), main_module)?;
            
            // Generate Nickel package manifest if requested
            if nickel_package {
                info!("Generating Nickel package manifest (experimental)");
                // TODO: Implement Nickel manifest generation with unified pipeline
                let manifest = format!(
                    "# Nickel package manifest for {}\n# Generated with unified pipeline\n\n{{\n  name = \"{}\",\n  version = \"0.1.0\",\n}}\n",
                    package_name, package_name
                );
                fs::write(output.join("Nickel-pkg.ncl"), manifest)?;
                info!("✓ Generated Nickel-pkg.ncl");
            }
            
            info!("Generated package '{}' in {:?} using unified pipeline", package_name, output);
            info!("Package structure:");
            for group in &all_groups {
                info!("  {}/", group);
            }
            if nickel_package {
                info!("  Nickel-pkg.ncl (package manifest)");
            }

            Ok(())
        }

        ImportSource::Crd {
            file,
            output,
            package_mode,
        } => {
            info!("Importing CRD from {:?}", file);

            let content = fs::read_to_string(&file)
                .with_context(|| format!("Failed to read CRD file: {:?}", file))?;

            let crd: CRD = if file.extension().is_some_and(|ext| ext == "json") {
                serde_json::from_str(&content)?
            } else {
                serde_yaml::from_str(&content)?
            };

            // Use the unified pipeline through NamespacedPackage
            let mut package =
                amalgam_parser::package::NamespacedPackage::new(crd.spec.group.clone());

            // Parse CRD to get type definition
            let parser = CRDParser::new();
            let temp_ir = parser.parse(crd.clone())?;

            // Add types from the parsed IR to the package
            for module in &temp_ir.modules {
                for type_def in &module.types {
                    // Extract version from module name
                    let parts: Vec<&str> = module.name.split('.').collect();
                    let version = if parts.len() > 1 {
                        parts[parts.len() - 2]
                    } else {
                        "v1"
                    };

                    package.add_type(
                        crd.spec.group.clone(),
                        version.to_string(),
                        type_def.name.clone(),
                        type_def.clone(),
                    );
                }
            }

            // Generate using unified pipeline
            let version = crd
                .spec
                .versions
                .first()
                .map(|v| v.name.clone())
                .unwrap_or_else(|| "v1".to_string());

            let files = package.generate_version_files(&crd.spec.group, &version);

            // For single file output, just get the first generated file
            let code = files
                .values()
                .next()
                .cloned()
                .unwrap_or_else(|| "# No types generated\n".to_string());

            // Apply package mode transformation if requested
            let final_code = if package_mode {
                // Transform relative imports to package imports
                // This is a post-processing step on the generated code
                transform_imports_to_package_mode(&code, &crd.spec.group)
            } else {
                code.clone()
            };

            if let Some(output_path) = output {
                fs::write(&output_path, &final_code)
                    .with_context(|| format!("Failed to write output: {:?}", output_path))?;
                info!("Generated Nickel code written to {:?}", output_path);
            } else {
                println!("{}", final_code);
            }

            Ok(())
        }

        ImportSource::OpenApi { file, output } => {
            info!("Importing OpenAPI spec from {:?}", file);

            let content = fs::read_to_string(&file)
                .with_context(|| format!("Failed to read OpenAPI file: {:?}", file))?;

            let spec: openapiv3::OpenAPI = if file.extension().is_some_and(|ext| ext == "json") {
                serde_json::from_str(&content)?
            } else {
                serde_yaml::from_str(&content)?
            };

            // Use the unified pipeline through NamespacedPackage
            // Extract namespace from filename or use default
            let namespace = file
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("openapi")
                .to_string();

            let mut package = amalgam_parser::package::NamespacedPackage::new(namespace.clone());

            // Parse using walker pattern
            let walker = amalgam_parser::walkers::openapi::OpenAPIWalker::new(&namespace);
            let ir = walker.walk(spec)?;

            // Add all types to the package from the generated IR
            for module in &ir.modules {
                for type_def in &module.types {
                    // Extract version from module name if present
                    let parts: Vec<&str> = module.name.split('.').collect();
                    let version = if parts.len() > 1 {
                        parts.last().unwrap().to_string()
                    } else {
                        "v1".to_string() // Default version
                    };

                    package.add_type(
                        namespace.clone(),
                        version.clone(),
                        type_def.name.clone(),
                        type_def.clone(),
                    );
                }
            }

            // Generate files using the unified pipeline
            let files = package.generate_version_files(&namespace, "v1");
            let code = files.values().next().unwrap_or(&String::new()).clone();

            if let Some(output_path) = output {
                fs::write(&output_path, &code)
                    .with_context(|| format!("Failed to write output: {:?}", output_path))?;
                info!("Generated Nickel code written to {:?}", output_path);
            } else {
                println!("{}", code);
            }

            Ok(())
        }

        ImportSource::K8sCore {
            version,
            output,
            types: _,
            nickel_package,
            package_base: _,
        } => {
            handle_k8s_core_import(&version, &output, nickel_package).await?;
            Ok(())
        }

        ImportSource::K8s { .. } => {
            anyhow::bail!("Kubernetes import not yet implemented. Build with --features kubernetes to enable.")
        }
    }
}

// Moved to lib.rs to avoid duplication
use amalgam::handle_k8s_core_import;

async fn handle_manifest_generation(
    manifest_path: PathBuf,
    packages: Vec<String>,
    dry_run: bool,
) -> Result<()> {
    use crate::manifest::Manifest;

    info!("Loading manifest from {:?}", manifest_path);
    let mut manifest = Manifest::from_file(&manifest_path)?;

    // Filter packages if specific ones were requested
    if !packages.is_empty() {
        manifest.packages.retain(|p| packages.contains(&p.name));
        if manifest.packages.is_empty() {
            anyhow::bail!("No matching packages found for: {:?}", packages);
        }
    }

    if dry_run {
        info!("Dry run mode - showing what would be generated:");
        for package in &manifest.packages {
            if package.enabled {
                info!("  - {} -> {}", package.name, package.output);
            }
        }
        return Ok(());
    }

    // Generate all packages
    let report = manifest.generate_all().await?;
    report.print_summary();

    if !report.failed.is_empty() {
        anyhow::bail!("Some packages failed to generate");
    }

    Ok(())
}

fn handle_generate(input: PathBuf, output: PathBuf, target: &str) -> Result<()> {
    info!("Generating {} code from {:?}", target, input);

    let ir_content = fs::read_to_string(&input)
        .with_context(|| format!("Failed to read IR file: {:?}", input))?;

    let ir: amalgam_core::IR =
        serde_json::from_str(&ir_content).with_context(|| "Failed to parse IR JSON")?;

    let code = match target {
        "nickel" => {
            let mut codegen = NickelCodegen::new();
            codegen.generate(&ir)?
        }
        "go" => {
            let mut codegen = GoCodegen::new();
            codegen.generate(&ir)?
        }
        _ => {
            anyhow::bail!("Unsupported target language: {}", target);
        }
    };

    fs::write(&output, code).with_context(|| format!("Failed to write output: {:?}", output))?;

    info!("Generated code written to {:?}", output);
    Ok(())
}

fn handle_convert(input: PathBuf, from: &str, output: PathBuf, to: &str) -> Result<()> {
    info!("Converting from {} to {}", from, to);

    let content = fs::read_to_string(&input)
        .with_context(|| format!("Failed to read input file: {:?}", input))?;

    // Parse input to IR
    let ir = match from {
        "crd" => {
            let crd: CRD = if input.extension().is_some_and(|ext| ext == "json") {
                serde_json::from_str(&content)?
            } else {
                serde_yaml::from_str(&content)?
            };
            CRDParser::new().parse(crd)?
        }
        "openapi" => {
            let spec: openapiv3::OpenAPI = if input.extension().is_some_and(|ext| ext == "json") {
                serde_json::from_str(&content)?
            } else {
                serde_yaml::from_str(&content)?
            };
            OpenAPIParser::new().parse(spec)?
        }
        _ => {
            anyhow::bail!("Unsupported input format: {}", from);
        }
    };

    // Generate output
    let output_content = match to {
        "nickel" => {
            let mut codegen = NickelCodegen::new();
            codegen.generate(&ir)?
        }
        "go" => {
            let mut codegen = GoCodegen::new();
            codegen.generate(&ir)?
        }
        "ir" => serde_json::to_string_pretty(&ir)?,
        _ => {
            anyhow::bail!("Unsupported output format: {}", to);
        }
    };

    fs::write(&output, output_content)
        .with_context(|| format!("Failed to write output: {:?}", output))?;

    info!("Conversion complete. Output written to {:?}", output);
    Ok(())
}

/// Transform relative imports in generated code to package imports
/// This is used when --package-mode is enabled
fn transform_imports_to_package_mode(code: &str, group: &str) -> String {
    // Determine the base package ID based on the group
    let package_id = if group.starts_with("k8s.io") || group.contains("k8s.io") {
        "github:seryl/nickel-pkgs/k8s-io"
    } else if group.contains("crossplane") {
        "github:seryl/nickel-pkgs/crossplane"
    } else {
        // For unknown groups, keep relative imports
        return code.to_string();
    };
    
    // Transform import statements from relative to package imports
    let mut result = String::new();
    for line in code.lines() {
        if line.contains("import") && line.contains("../") {
            // Extract the module path from the import
            if let Some(start) = line.find('"') {
                if let Some(end) = line.rfind('"') {
                    let import_path = &line[start + 1..end];
                    // Count the number of ../ to determine depth
                    let depth = import_path.matches("../").count();
                    
                    // Extract the module name (last part of the path)
                    let module_parts: Vec<&str> = import_path.split('/').collect();
                    let module_name = module_parts.last()
                        .and_then(|s| s.strip_suffix(".ncl"))
                        .unwrap_or("");
                    
                    // Construct package import
                    if depth >= 2 && module_name != "mod" {
                        // This looks like a cross-version import
                        let new_line = format!(
                            "{}import \"{}#/{}\".{}",
                            &line[..start],
                            package_id,
                            module_name,
                            &line[end + 1..]
                        );
                        result.push_str(&new_line);
                        result.push('\n');
                        continue;
                    }
                }
            }
        }
        result.push_str(line);
        result.push('\n');
    }
    
    // Remove trailing newline if original didn't have one
    if !code.ends_with('\n') && result.ends_with('\n') {
        result.pop();
    }
    
    result
}
