//! Integration tests for amalgam-parser

use amalgam_codegen::Codegen;
use amalgam_parser::{
    crd::{CRDParser, CRD},
    package::PackageGenerator,
    Parser,
};
use tempfile::TempDir;

fn load_test_crd(yaml_content: &str) -> CRD {
    serde_yaml::from_str(yaml_content).expect("Failed to parse test CRD")
}

#[test]
fn test_end_to_end_crd_to_nickel() {
    let crd_yaml = r#"
apiVersion: apiextensions.k8s.io/v1
kind: CustomResourceDefinition
metadata:
  name: compositions.apiextensions.crossplane.io
spec:
  group: apiextensions.crossplane.io
  names:
    kind: Composition
    plural: compositions
    singular: composition
  versions:
  - name: v1
    served: true
    storage: true
    schema:
      openAPIV3Schema:
        type: object
        required:
        - spec
        properties:
          spec:
            type: object
            required:
            - resources
            properties:
              resources:
                type: array
                items:
                  type: object
                  properties:
                    name:
                      type: string
                    base:
                      type: object
              compositeTypeRef:
                type: object
                properties:
                  apiVersion:
                    type: string
                  kind:
                    type: string
"#;

    let crd = load_test_crd(crd_yaml);
    let parser = CRDParser::new();
    let ir = parser.parse(crd.clone()).expect("Failed to parse CRD");

    // Verify IR was generated with one module for the single version
    assert_eq!(
        ir.modules.len(),
        1,
        "Should have 1 module for single version"
    );
    assert!(ir.modules[0].name.contains("Composition"));
    assert!(ir.modules[0].name.contains("v1"));

    // Generate Nickel code
    let mut codegen = amalgam_codegen::nickel::NickelCodegen::new();
    let nickel_code = codegen
        .generate(&ir)
        .expect("Failed to generate Nickel code");

    // Verify generated code contains expected elements
    assert!(nickel_code.contains("Composition"));
    assert!(nickel_code.contains("spec"));
    assert!(nickel_code.contains("resources"));
    assert!(nickel_code.contains("compositeTypeRef"));
}

#[test]
fn test_package_structure_generation() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let output_path = temp_dir.path().to_path_buf();

    let mut generator = PackageGenerator::new("test-package".to_string(), output_path.clone());

    // Add multiple CRDs
    let crd1_yaml = r#"
apiVersion: apiextensions.k8s.io/v1
kind: CustomResourceDefinition
metadata:
  name: widgets.example.io
spec:
  group: example.io
  names:
    kind: Widget
    plural: widgets
    singular: widget
  versions:
  - name: v1
    served: true
    storage: true
    schema:
      openAPIV3Schema:
        type: object
"#;

    let crd2_yaml = r#"
apiVersion: apiextensions.k8s.io/v1
kind: CustomResourceDefinition
metadata:
  name: gadgets.example.io
spec:
  group: example.io
  names:
    kind: Gadget
    plural: gadgets
    singular: gadget
  versions:
  - name: v1
    served: true
    storage: false
    schema:
      openAPIV3Schema:
        type: object
  - name: v2
    served: true
    storage: true
    schema:
      openAPIV3Schema:
        type: object
"#;

    generator.add_crd(load_test_crd(crd1_yaml));
    generator.add_crd(load_test_crd(crd2_yaml));

    let package = generator
        .generate_package()
        .expect("Failed to generate package");

    // Verify package structure
    assert_eq!(package.groups().len(), 1);
    assert!(package.groups().contains(&"example.io".to_string()));

    let versions = package.versions("example.io");
    assert!(versions.contains(&"v1".to_string()));
    assert!(versions.contains(&"v2".to_string()));

    let v1_kinds = package.kinds("example.io", "v1");
    assert!(v1_kinds.contains(&"widget".to_string()));
    assert!(v1_kinds.contains(&"gadget".to_string()));

    let v2_kinds = package.kinds("example.io", "v2");
    assert!(v2_kinds.contains(&"gadget".to_string()));
    assert!(!v2_kinds.contains(&"widget".to_string()));
}

#[test]
fn test_complex_schema_parsing() {
    let crd_yaml = r#"
apiVersion: apiextensions.k8s.io/v1
kind: CustomResourceDefinition
metadata:
  name: complex.test.io
spec:
  group: test.io
  names:
    kind: Complex
    plural: complexes
    singular: complex
  versions:
  - name: v1
    served: true
    storage: true
    schema:
      openAPIV3Schema:
        type: object
        properties:
          spec:
            type: object
            properties:
              stringField:
                type: string
                default: "default-value"
              intField:
                type: integer
                minimum: 0
                maximum: 100
              arrayField:
                type: array
                items:
                  type: string
              mapField:
                type: object
                additionalProperties:
                  type: number
              nestedObject:
                type: object
                properties:
                  innerString:
                    type: string
                  innerBool:
                    type: boolean
              enumField:
                type: string
                enum:
                - value1
                - value2
                - value3
              unionField:
                oneOf:
                - type: string
                - type: number
              optionalField:
                type: string
                nullable: true
"#;

    let crd = load_test_crd(crd_yaml);
    let parser = CRDParser::new();
    let ir = parser.parse(crd).expect("Failed to parse complex CRD");

    // Find the Complex type in the IR
    let complex_module = ir
        .modules
        .iter()
        .find(|m| m.name.contains("Complex"))
        .expect("Complex module not found");

    let complex_type = complex_module
        .types
        .iter()
        .find(|t| t.name == "Complex")
        .expect("Complex type not found");

    // Verify the type structure
    match &complex_type.ty {
        amalgam_core::types::Type::Record { fields, .. } => {
            assert!(fields.contains_key("spec"));
            // Further nested validation could be done here
        }
        _ => panic!("Expected Complex to be a Record type"),
    }
}

#[test]
fn test_multi_version_crd() {
    let crd_yaml = r#"
apiVersion: apiextensions.k8s.io/v1
kind: CustomResourceDefinition
metadata:
  name: multiversion.test.io
spec:
  group: test.io
  names:
    kind: MultiVersion
    plural: multiversions
    singular: multiversion
  versions:
  - name: v1alpha1
    served: true
    storage: false
    schema:
      openAPIV3Schema:
        type: object
        properties:
          spec:
            type: object
            properties:
              alphaField:
                type: string
  - name: v1beta1
    served: true
    storage: false
    schema:
      openAPIV3Schema:
        type: object
        properties:
          spec:
            type: object
            properties:
              alphaField:
                type: string
              betaField:
                type: integer
  - name: v1
    served: true
    storage: true
    schema:
      openAPIV3Schema:
        type: object
        properties:
          spec:
            type: object
            properties:
              alphaField:
                type: string
              betaField:
                type: integer
              stableField:
                type: boolean
"#;

    let crd = load_test_crd(crd_yaml);
    let parser = CRDParser::new();
    let ir = parser
        .parse(crd.clone())
        .expect("Failed to parse multi-version CRD");

    // Parser should create separate modules for each version
    assert_eq!(ir.modules.len(), 3, "Should have 3 modules for 3 versions");

    // Check that each version has its own module
    let module_names: Vec<String> = ir.modules.iter().map(|m| m.name.clone()).collect();

    assert!(
        module_names.iter().any(|n| n.contains("v1alpha1")),
        "Should have v1alpha1 module"
    );
    assert!(
        module_names.iter().any(|n| n.contains("v1beta1")),
        "Should have v1beta1 module"
    );
    assert!(
        module_names.iter().any(|n| n.contains(".v1.")),
        "Should have v1 module"
    );

    // Each module should have the MultiVersion type
    for module in &ir.modules {
        assert_eq!(module.types.len(), 1, "Each module should have one type");
        assert_eq!(module.types[0].name, "MultiVersion");
    }
}

#[test]
fn test_multi_version_package_generation() {
    let crd_yaml = r#"
apiVersion: apiextensions.k8s.io/v1
kind: CustomResourceDefinition
metadata:
  name: evolving.test.io
spec:
  group: test.io
  names:
    kind: Evolving
    plural: evolvings
    singular: evolving
  versions:
  - name: v1alpha1
    served: true
    storage: false
    schema:
      openAPIV3Schema:
        type: object
        properties:
          spec:
            type: object
            properties:
              alphaField:
                type: string
  - name: v1beta1
    served: true
    storage: false
    schema:
      openAPIV3Schema:
        type: object
        properties:
          spec:
            type: object
            properties:
              alphaField:
                type: string
              betaField:
                type: integer
  - name: v1
    served: true
    storage: true
    schema:
      openAPIV3Schema:
        type: object
        properties:
          spec:
            type: object
            properties:
              alphaField:
                type: string
              betaField:
                type: integer
              stableField:
                type: boolean
"#;

    let temp_dir = tempfile::TempDir::new().unwrap();
    let mut generator =
        PackageGenerator::new("evolution-test".to_string(), temp_dir.path().to_path_buf());

    generator.add_crd(load_test_crd(crd_yaml));

    let package = generator
        .generate_package()
        .expect("Failed to generate package");

    // Verify all versions are present
    let versions = package.versions("test.io");
    assert_eq!(versions.len(), 3, "Should have 3 versions");
    assert!(versions.contains(&"v1alpha1".to_string()));
    assert!(versions.contains(&"v1beta1".to_string()));
    assert!(versions.contains(&"v1".to_string()));

    // Each version should have the evolving kind
    for version in &["v1alpha1", "v1beta1", "v1"] {
        let kinds = package.kinds("test.io", version);
        assert_eq!(kinds.len(), 1, "Each version should have 1 kind");
        assert!(kinds.contains(&"evolving".to_string()));
    }

    // Verify we can generate files for each version
    let v1alpha1_files = package.generate_version_files("test.io", "v1alpha1");
    assert!(v1alpha1_files.contains_key("evolving.ncl"));
    
    let v1beta1_files = package.generate_version_files("test.io", "v1beta1");
    assert!(v1beta1_files.contains_key("evolving.ncl"));
    
    let v1_files = package.generate_version_files("test.io", "v1");
    assert!(v1_files.contains_key("evolving.ncl"));
}

#[test]
fn test_crd_with_validation_rules() {
    let crd_yaml = r#"
apiVersion: apiextensions.k8s.io/v1
kind: CustomResourceDefinition
metadata:
  name: validated.test.io
spec:
  group: test.io
  names:
    kind: Validated
    plural: validateds
    singular: validated
  versions:
  - name: v1
    served: true
    storage: true
    schema:
      openAPIV3Schema:
        type: object
        required:
        - spec
        properties:
          spec:
            type: object
            required:
            - requiredField
            properties:
              requiredField:
                type: string
                minLength: 3
                maxLength: 10
                pattern: "^[a-z]+$"
              numberWithBounds:
                type: number
                minimum: 0.0
                maximum: 100.0
                exclusiveMinimum: true
              arrayWithLimits:
                type: array
                minItems: 1
                maxItems: 5
                uniqueItems: true
                items:
                  type: string
"#;

    let crd = load_test_crd(crd_yaml);
    let parser = CRDParser::new();
    let ir = parser.parse(crd).expect("Failed to parse validated CRD");

    // Generate code and verify validation constraints are preserved
    let mut codegen = amalgam_codegen::nickel::NickelCodegen::new();
    let nickel_code = codegen
        .generate(&ir)
        .expect("Failed to generate Nickel code");

    // Check that required fields are marked
    assert!(nickel_code.contains("requiredField"));
    // Note: Actual validation constraints would need to be implemented
    // in the code generator to be properly tested here
}
