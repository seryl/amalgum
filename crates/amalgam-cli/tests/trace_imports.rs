//! Test with comprehensive tracing to visualize the full call graph

use amalgam::handle_k8s_core_import;
use std::fs;
use tempfile::tempdir;
use tracing_subscriber::prelude::*;

#[tokio::test]
async fn trace_k8s_imports() {
    // Use tracing-forest for better async/tokio visualization
    let forest_layer = tracing_forest::ForestLayer::default();

    // Configure subscriber
    let subscriber = tracing_subscriber::registry().with(forest_layer).with(
        tracing_subscriber::EnvFilter::from_default_env()
            .add_directive("amalgam=trace".parse().unwrap())
            .add_directive("amalgam_parser=trace".parse().unwrap())
            .add_directive("amalgam_core=trace".parse().unwrap())
            .add_directive("amalgam_codegen=trace".parse().unwrap()),
    );

    // Try to set the global subscriber, ignore if already set
    let _ = tracing::subscriber::set_global_default(subscriber);

    // Create a temporary directory for output
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let output_dir = temp_dir.path();

    // Generate k8s core types with tracing
    tracing::info!("Starting k8s core import generation");

    let span = tracing::info_span!("k8s_import", version = "v1.33.4");
    let _enter = span.enter();

    handle_k8s_core_import("v1.33.4", output_dir, true)
        .await
        .expect("Failed to generate k8s core types");

    // Check specific files to trigger import resolution code paths
    let v1alpha1_vac = output_dir.join("v1alpha1/volumeattributesclass.ncl");
    if v1alpha1_vac.exists() {
        let content = fs::read_to_string(&v1alpha1_vac).expect("Failed to read v1alpha1 file");

        // Print the actual content so we can see what's generated
        println!("\n===== v1alpha1/volumeattributesclass.ncl content =====");
        println!("{}", content);
        println!("===== end of content =====\n");

        tracing::info!(
            has_import = content.contains("import"),
            has_objectmeta = content.contains("ObjectMeta"),
            file = "v1alpha1/volumeattributesclass.ncl",
            "Checking for imports"
        );
    }
}
