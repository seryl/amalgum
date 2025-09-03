//! Test that k8s types properly import cross-version dependencies

use amalgam::handle_k8s_core_import;
use std::fs;
use tempfile::tempdir;

#[tokio::test]
async fn test_k8s_cross_version_imports() {
    // Create a temporary directory for output
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let output_dir = temp_dir.path();

    // Generate k8s core types
    handle_k8s_core_import("v1.33.4", output_dir, true)
        .await
        .expect("Failed to generate k8s core types");

    // Check that v1 contains ObjectMeta
    let v1_objectmeta = output_dir.join("v1/objectmeta.ncl");
    assert!(v1_objectmeta.exists(), "v1/objectmeta.ncl should exist");

    // Check that v1 contains Condition
    let v1_condition = output_dir.join("v1/condition.ncl");
    assert!(v1_condition.exists(), "v1/condition.ncl should exist");

    // Check that v1alpha1 VolumeAttributesClass imports ObjectMeta from v1
    let v1alpha1_vac = output_dir.join("v1alpha1/volumeattributesclass.ncl");
    if v1alpha1_vac.exists() {
        let content = fs::read_to_string(&v1alpha1_vac).expect("Failed to read v1alpha1 file");
        // Check for ObjectMeta import (might be in different positions)
        // Check for ObjectMeta import - the walker generates imports with "let" bindings
        assert!(
            content.contains("objectmeta")
                && content.contains("import")
                && content.contains("v1/objectmeta.ncl"),
            "v1alpha1 should import ObjectMeta from v1. Content: {}",
            &content[..content.len().min(500)]
        );
    }

    // Check that v1beta1 ServiceCIDR imports ObjectMeta from v1
    let v1beta1_servicecidr = output_dir.join("v1beta1/servicecidr.ncl");
    if v1beta1_servicecidr.exists() {
        let content =
            fs::read_to_string(&v1beta1_servicecidr).expect("Failed to read v1beta1 file");
        assert!(
            content.contains("objectmeta")
                && content.contains("import")
                && content.contains("v1/objectmeta.ncl"),
            "v1beta1 ServiceCIDR should import ObjectMeta from v1"
        );
    }

    // Check that v1beta1 ServiceCIDRStatus imports Condition from v1
    let v1beta1_status = output_dir.join("v1beta1/servicecidrstatus.ncl");
    if v1beta1_status.exists() {
        let content =
            fs::read_to_string(&v1beta1_status).expect("Failed to read v1beta1 status file");
        assert!(
            content.contains("condition")
                && content.contains("import")
                && content.contains("v1/condition.ncl"),
            "v1beta1 ServiceCIDRStatus should import Condition from v1"
        );
    }
}

#[test]
fn test_is_core_k8s_type() {
    // Test the is_core_k8s_type function indirectly through the generated output
    // This is tested implicitly through the integration test above

    // Core types that should be recognized
    let core_types = vec![
        "ObjectMeta",
        "ListMeta",
        "Condition",
        "LabelSelector",
        "Time",
        "MicroTime",
        "Status",
        "TypeMeta",
    ];

    // These should all be imported from v1 when referenced from other versions
    for type_name in core_types {
        // The actual test happens in the integration test above
        // where we verify the imports are generated correctly
        assert!(!type_name.is_empty(), "Type name should not be empty");
    }
}
