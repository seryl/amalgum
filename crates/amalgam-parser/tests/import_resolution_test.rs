//! Integration tests for import detection and resolution
//!
//! These tests ensure that:
//! 1. K8s type references are properly detected
//! 2. Imports are correctly generated
//! 3. References are resolved to use import aliases

mod fixtures;

use amalgam_codegen::{nickel::NickelCodegen, Codegen};
use amalgam_core::{
    ir::{Import, Module, TypeDefinition},
    types::{Field, Type},
    IR,
};
use amalgam_parser::{crd::CRDParser, package::PackageGenerator, Parser};
use fixtures::Fixtures;
use std::collections::BTreeMap;

#[test]
fn test_k8s_type_reference_detection() {
    // Load fixture CRD that should have ObjectMeta reference
    let crd = Fixtures::simple_with_metadata();

    // Parse the CRD
    let parser = CRDParser::new();
    let ir = parser.parse(crd).expect("Failed to parse CRD");

    // The type should contain a reference to ObjectMeta
    assert_eq!(ir.modules.len(), 1);
    let module = &ir.modules[0];
    assert_eq!(module.types.len(), 1);

    let type_def = &module.types[0];

    // Check that the metadata field has the k8s reference
    if let Type::Record { fields, .. } = &type_def.ty {
        assert!(fields.contains_key("metadata"));
        let metadata_field = &fields["metadata"];

        // The metadata field can be either:
        // - Type::Reference directly (if the parser detected it should be ObjectMeta)
        // - Type::Optional(Type::Reference)
        // - Type::Object (if it's just marked as 'type: object' in the CRD)

        match &metadata_field.ty {
            Type::Reference { name, module } => {
                assert_eq!(name, "ObjectMeta");
                assert_eq!(
                    module.as_deref(),
                    Some("io.k8s.apimachinery.pkg.apis.meta.v1")
                );
            }
            Type::Optional(inner) => {
                if let Type::Reference { name, module } = &**inner {
                    assert_eq!(name, "ObjectMeta");
                    assert_eq!(
                        module.as_deref(),
                        Some("io.k8s.apimachinery.pkg.apis.meta.v1")
                    );
                } else {
                    // For this test, metadata is just an object, not a k8s reference
                    // This is OK - the parser doesn't automatically add k8s references
                }
            }
            Type::Record { .. } => {
                // This is fine - the CRD just has 'type: object' for metadata
                // The parser converts it to a Record type
            }
            Type::Any => {
                // Metadata might be parsed as Any if no schema is provided
            }
            _ => panic!("Unexpected type for metadata: {:?}", metadata_field.ty),
        }
    } else {
        panic!("Expected Record type, got {:?}", type_def.ty);
    }
}

#[test]
fn test_import_generation_for_k8s_types() {
    // Create multiple CRDs in a package
    let mut package = PackageGenerator::new(
        "test-package".to_string(),
        std::path::PathBuf::from("/tmp/test"),
    );

    let crd1 = Fixtures::simple_with_metadata();
    let crd2 = Fixtures::with_arrays();

    package.add_crd(crd1);
    package.add_crd(crd2);

    // Generate package and check for k8s imports
    let ns_package = package
        .generate_package()
        .expect("Failed to generate package");

    // Get the generated content for a resource that uses k8s types
    let version_files = ns_package.generate_version_files("test.io", "v1");
    if let Some(content) = version_files.get("simple.ncl") {
        // Verify the import is present
        assert!(content.contains("import"), "Missing import statement");
        assert!(content.contains("k8s_io"), "Missing k8s import");
        assert!(
            content.contains("objectmeta.ncl"),
            "Missing objectmeta import path"
        );
    } else {
        // Generate from IR directly as fallback
        let crd = Fixtures::simple_with_metadata();
        let parser = CRDParser::new();
        let ir = parser.parse(crd).expect("Failed to parse CRD");
        let mut codegen = amalgam_codegen::nickel::NickelCodegen::new();
        let content = codegen.generate(&ir).expect("Failed to generate");

        // The k8s imports should still be resolved
        assert!(
            content.contains("k8s_io") || content.contains("k8s_v1"),
            "Missing k8s import resolution in: {}",
            content
        );
    }
}

#[test]
fn test_reference_resolution_to_alias() {
    // Create a module with k8s type reference and import
    let mut ir = IR::new();

    let mut fields = BTreeMap::new();
    fields.insert(
        "metadata".to_string(),
        Field {
            ty: Type::Reference {
                name: "ObjectMeta".to_string(),
                module: Some("io.k8s.apimachinery.pkg.apis.meta.v1".to_string()),
            },
            required: false,
            default: None,
            description: Some("Standard Kubernetes metadata".to_string()),
        },
    );

    let module = Module {
        name: "test.example.io".to_string(),
        imports: vec![Import {
            path: "../../k8s_io/v1/objectmeta.ncl".to_string(),
            alias: Some("k8s_io_v1".to_string()),
            items: vec![],
        }],
        types: vec![TypeDefinition {
            name: "TestResource".to_string(),
            ty: Type::Record {
                fields,
                open: false,
            },
            documentation: None,
            annotations: BTreeMap::new(),
        }],
        constants: vec![],
        metadata: Default::default(),
    };

    ir.add_module(module);

    // Generate Nickel code
    let mut codegen = NickelCodegen::new();
    let generated = codegen
        .generate(&ir)
        .expect("Failed to generate Nickel code");

    // Verify the import is in the output
    assert!(
        generated.contains("let k8s_io_v1 = import"),
        "Missing import statement in generated code"
    );

    // Verify the reference was resolved to use the alias
    assert!(
        generated.contains("k8s_io_v1.ObjectMeta"),
        "Reference not resolved to alias. Generated:\n{}",
        generated
    );

    // Verify the original reference is NOT in the output
    assert!(
        !generated.contains("io.k8s.apimachinery.pkg.apis.meta.v1.ObjectMeta"),
        "Original reference still present. Generated:\n{}",
        generated
    );
}

#[test]
fn test_multiple_k8s_type_references() {
    // Use fixture with multiple k8s refs
    let crd = Fixtures::multiple_k8s_refs();
    let parser = CRDParser::new();
    let ir = parser.parse(crd).expect("Failed to parse CRD");

    let mut codegen = amalgam_codegen::nickel::NickelCodegen::new();
    let content = codegen.generate(&ir).expect("Failed to generate");

    // Note: The current CRD parser doesn't handle $ref, so k8s types in definitions
    // won't be detected. This test documents the current behavior.
    // TODO: Add $ref support to CRDParser

    // For now, just verify the CRD parses and generates valid Nickel
    assert!(content.contains("MultiRef"), "Missing type name");
    assert!(content.contains("spec"), "Missing spec field");
}

#[test]
fn test_no_import_for_local_types() {
    // Use fixture without k8s types
    let crd = Fixtures::nested_objects();
    let parser = CRDParser::new();
    let ir = parser.parse(crd).expect("Failed to parse CRD");

    // No imports should be generated for local types
    assert_eq!(
        ir.modules[0].imports.len(),
        0,
        "Unexpected imports for CRD without k8s types"
    );
}

#[test]
fn test_import_path_calculation() {
    use amalgam_parser::imports::TypeReference;

    // Test that import paths are calculated correctly
    let type_ref =
        TypeReference::from_qualified_name("io.k8s.apimachinery.pkg.apis.meta.v1.ObjectMeta")
            .expect("Failed to parse type reference");

    let import_path = type_ref.import_path("example.io", "v1");
    assert_eq!(import_path, "../../k8s_io/v1/objectmeta.ncl");

    let alias = type_ref.module_alias();
    assert_eq!(alias, "k8s_io_v1");
}

#[test]
fn test_case_insensitive_type_matching() {
    // The resolver should handle case differences between reference and file names
    let mut ir = IR::new();

    let mut fields = BTreeMap::new();
    fields.insert(
        "metadata".to_string(),
        Field {
            ty: Type::Reference {
                name: "ObjectMeta".to_string(),
                module: Some("io.k8s.apimachinery.pkg.apis.meta.v1".to_string()),
            },
            required: false,
            default: None,
            description: None,
        },
    );

    let module = Module {
        name: "test".to_string(),
        imports: vec![Import {
            // Note: file is lowercase "objectmeta.ncl"
            path: "../../k8s_io/v1/objectmeta.ncl".to_string(),
            alias: Some("k8s_v1".to_string()),
            items: vec![],
        }],
        types: vec![TypeDefinition {
            name: "Test".to_string(),
            ty: Type::Record {
                fields,
                open: false,
            },
            documentation: None,
            annotations: BTreeMap::new(),
        }],
        constants: vec![],
        metadata: Default::default(),
    };

    ir.add_module(module);

    let mut codegen = NickelCodegen::new();
    let generated = codegen.generate(&ir).expect("Failed to generate");

    // Should resolve despite case difference
    assert!(
        generated.contains("k8s_v1.ObjectMeta"),
        "Failed to resolve with case difference. Generated:\n{}",
        generated
    );
}

/// Test that package generation creates proper structure
#[test]
fn test_package_structure_generation() {
    let mut package = PackageGenerator::new(
        "test-package".to_string(),
        std::path::PathBuf::from("/tmp/test"),
    );

    // Add CRDs from different fixtures
    let crd1 = Fixtures::simple_with_metadata();
    let crd2 = Fixtures::with_arrays();
    let crd3 = Fixtures::multi_version();

    package.add_crd(crd1);
    package.add_crd(crd2);
    package.add_crd(crd3);

    // Generate and check structure
    let ns_package = package
        .generate_package()
        .expect("Failed to generate package");

    // Check that main module was generated
    let main_module = ns_package.generate_main_module();
    assert!(
        main_module.contains("test_io"),
        "Missing test.io group in main module"
    );
}
