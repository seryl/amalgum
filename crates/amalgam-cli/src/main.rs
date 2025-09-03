use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::fs;
use std::path::PathBuf;
use tracing::info;

use amalgam_codegen::{go::GoCodegen, nickel::NickelCodegen, Codegen};
use amalgam_parser::{
    crd::{CRDParser, CRD},
    openapi::OpenAPIParser,
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
            package_base,
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

            // Generate package structure
            let mut generator = amalgam_parser::package::PackageGenerator::new(
                package_name.clone(),
                output.clone(),
            );
            generator.add_crds(crds);

            let package_structure = generator.generate_package()?;

            // Create output directory structure
            fs::create_dir_all(&output)?;

            // Write main module file
            let main_module = package_structure.generate_main_module();
            fs::write(output.join("mod.ncl"), main_module)?;

            // Create group/version/kind structure
            for group in package_structure.groups() {
                let group_dir = output.join(&group);
                fs::create_dir_all(&group_dir)?;

                // Write group module
                if let Some(group_mod) = package_structure.generate_group_module(&group) {
                    fs::write(group_dir.join("mod.ncl"), group_mod)?;
                }

                // Create version directories
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

            // Generate Nickel package manifest if requested
            if nickel_package {
                info!("Generating Nickel package manifest (experimental)");
                let manifest = package_structure.generate_nickel_manifest(None);
                fs::write(output.join("Nickel-pkg.ncl"), manifest)?;
                info!("✓ Generated Nickel-pkg.ncl");
            }

            info!("Generated package '{}' in {:?}", package_name, output);
            info!("Package structure:");
            for group in package_structure.groups() {
                info!("  {}/", group);
                for version in package_structure.versions(&group) {
                    let kinds = package_structure.kinds(&group, &version);
                    info!("    {}/: {} types", version, kinds.len());
                }
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

            let parser = CRDParser::new();
            let mut ir = parser.parse(crd.clone())?;

            // Note: Import resolution is now handled by the walker pattern
            // The CRDParser already adds necessary imports during IR generation


            // Generate Nickel code with package mode support
            let mut codegen = if package_mode {
                use amalgam_codegen::package_mode::PackageMode;
                use std::path::PathBuf;

                // Look for manifest in current directory first, then fallback locations
                let manifest_path = if PathBuf::from(".amalgam-manifest.toml").exists() {
                    PathBuf::from(".amalgam-manifest.toml")
                } else if PathBuf::from("amalgam-manifest.toml").exists() {
                    PathBuf::from("amalgam-manifest.toml")
                } else {
                    PathBuf::from("does-not-exist")
                };

                let manifest = if manifest_path.exists() {
                    Some(&manifest_path)
                } else {
                    None
                };

                // Create analyzer-based package mode
                let mut package_mode = PackageMode::new_with_analyzer(manifest);

                // Analyze the IR to detect dependencies automatically
                // Extract the package name from the CRD group
                let package_name = crd.spec.group.split('.').next().unwrap_or("unknown");
                let mut all_types: Vec<amalgam_core::types::Type> = Vec::new();
                for module in &ir.modules {
                    for type_def in &module.types {
                        all_types.push(type_def.ty.clone());
                    }
                }
                package_mode.analyze_and_update_dependencies(&all_types, package_name);

                NickelCodegen::new().with_package_mode(package_mode)
            } else {
                NickelCodegen::new()
            };
            let code = codegen.generate(&ir)?;

            if let Some(output_path) = output {
                fs::write(&output_path, code)
                    .with_context(|| format!("Failed to write output: {:?}", output_path))?;
                info!("Generated Nickel code written to {:?}", output_path);
            } else {
                println!("{}", code);
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

            let parser = OpenAPIParser::new();
            let mut ir = parser.parse(spec)?;

            // Note: Import resolution is now handled by the walker pattern
            // The CRDParser already adds necessary imports during IR generation


            // Generate Nickel code by default
            let mut codegen = NickelCodegen::new();
            let code = codegen.generate(&ir)?;

            if let Some(output_path) = output {
                fs::write(&output_path, code)
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
            package_base,
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
