//! Snapshot tests for generated Nickel code
//!
//! These tests ensure that the generated output remains consistent
//! and catch any unintended changes to the code generation

mod fixtures;

use amalgam_codegen::{nickel::NickelCodegen, Codegen};
use amalgam_parser::{crd::CRDParser, package::PackageGenerator, Parser};
use fixtures::Fixtures;
use insta::assert_snapshot;

#[test]
fn test_snapshot_simple_crd() {
    let crd = Fixtures::simple_with_metadata();
    let parser = CRDParser::new();
    let ir = parser.parse(crd).expect("Failed to parse CRD");

    // Generate Nickel code
    let mut codegen = NickelCodegen::new();
    let generated = codegen
        .generate(&ir)
        .expect("Failed to generate Nickel code");

    // Snapshot the generated code
    assert_snapshot!("simple_crd_nickel", generated);
}

#[test]
fn test_snapshot_crd_with_k8s_imports() {
    let crd = Fixtures::simple_with_metadata();
    let parser = CRDParser::new();
    let ir = parser.parse(crd.clone()).expect("Failed to parse CRD");

    // Use PackageGenerator to handle imports
    let mut package = PackageGenerator::new(
        "test-package".to_string(),
        std::path::PathBuf::from("/tmp/test"),
    );
    package.add_crd(crd);

    let generated_package = package
        .generate_package()
        .expect("Failed to generate package");

    // Get the generated content using the new batch generation
    let version_files = generated_package.generate_version_files("test.io", "v1");
    let content = version_files
        .get("simple.ncl")
        .cloned()
        .unwrap_or_else(|| {
            // If no file found, generate from IR directly
            let mut codegen = NickelCodegen::new();
            codegen.generate(&ir).expect("Failed to generate")
        });

    // Snapshot should include imports and resolved references
    assert_snapshot!("simple_with_k8s_imports", content);
}

#[test]
fn test_snapshot_multiple_k8s_refs() {
    let crd = Fixtures::multiple_k8s_refs();
    let parser = CRDParser::new();
    let ir = parser.parse(crd).expect("Failed to parse CRD");

    let mut codegen = NickelCodegen::new();
    let content = codegen.generate(&ir).expect("Failed to generate");

    assert_snapshot!("multiple_k8s_refs_nickel", content);
}

#[test]
fn test_snapshot_nested_objects() {
    let crd = Fixtures::nested_objects();
    let parser = CRDParser::new();
    let ir = parser.parse(crd).expect("Failed to parse CRD");

    let mut codegen = NickelCodegen::new();
    let generated = codegen.generate(&ir).expect("Failed to generate");

    assert_snapshot!("nested_objects_nickel", generated);
}

#[test]
fn test_snapshot_arrays() {
    let crd = Fixtures::with_arrays();
    let parser = CRDParser::new();
    let ir = parser.parse(crd).expect("Failed to parse CRD");

    let mut codegen = NickelCodegen::new();
    let content = codegen.generate(&ir).expect("Failed to generate");

    assert_snapshot!("arrays_nickel", content);
}

#[test]
fn test_snapshot_validation() {
    let crd = Fixtures::with_validation();
    let parser = CRDParser::new();
    let ir = parser.parse(crd).expect("Failed to parse CRD");

    let mut codegen = NickelCodegen::new();
    let generated = codegen.generate(&ir).expect("Failed to generate");

    assert_snapshot!("validation_nickel", generated);
}

#[test]
fn test_snapshot_multi_version() {
    let crd = Fixtures::multi_version();
    let parser = CRDParser::new();

    // Parse all versions
    let ir = parser.parse(crd).expect("Failed to parse CRD");

    // The IR should have modules for each version
    let mut codegen = NickelCodegen::new();
    let all_versions = codegen.generate(&ir).expect("Failed to generate");

    // Snapshot the full multi-version output
    assert_snapshot!("multi_version_all", all_versions);
}

#[test]
fn test_snapshot_ir_structure() {
    // Also snapshot the IR structure to catch changes in parsing
    let crd = Fixtures::simple_with_metadata();
    let parser = CRDParser::new();
    let ir = parser.parse(crd).expect("Failed to parse CRD");

    assert_snapshot!("simple_crd_ir", format!("{:#?}", ir));
}

#[test]
fn test_snapshot_package_structure() {
    let mut package = PackageGenerator::new(
        "test-package".to_string(),
        std::path::PathBuf::from("/tmp/test"),
    );

    // Add multiple CRDs
    package.add_crd(Fixtures::simple_with_metadata());
    package.add_crd(Fixtures::with_arrays());
    package.add_crd(Fixtures::multi_version());

    // Generate the package
    let ns_package = package
        .generate_package()
        .expect("Failed to generate package");

    // Get the main module to see structure
    let main_module = ns_package.generate_main_module();

    assert_snapshot!("package_structure_main", main_module);
}
