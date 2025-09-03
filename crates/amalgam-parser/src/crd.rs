//! Kubernetes CRD parser

use crate::{k8s_authoritative::K8sTypePatterns, Parser, ParserError};
use amalgam_core::{
    ir::{IRBuilder, IR},
    types::Type,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Kubernetes CustomResourceDefinition (simplified)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CRD {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub kind: String,
    pub metadata: CRDMetadata,
    pub spec: CRDSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CRDMetadata {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CRDSpec {
    pub group: String,
    pub versions: Vec<CRDVersion>,
    pub names: CRDNames,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CRDVersion {
    pub name: String,
    pub served: bool,
    pub storage: bool,
    pub schema: Option<CRDSchema>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CRDSchema {
    #[serde(rename = "openAPIV3Schema")]
    pub openapi_v3_schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CRDNames {
    pub plural: String,
    pub singular: String,
    pub kind: String,
}

pub struct CRDParser {
    k8s_patterns: K8sTypePatterns,
}

impl Parser for CRDParser {
    type Input = CRD;

    fn parse(&self, input: Self::Input) -> Result<IR, ParserError> {
        let mut ir = IR::new();

        // Create a separate module for each version
        for version in input.spec.versions {
            if let Some(schema) = version.schema {
                let module_name = format!(
                    "{}.{}.{}",
                    input.spec.names.kind, version.name, input.spec.group
                );
                let mut builder = IRBuilder::new().module(module_name);

                let type_name = input.spec.names.kind.clone();
                let ty = self.json_schema_to_type(&schema.openapi_v3_schema)?;

                // Enhance the type with proper k8s fields
                let enhanced_ty = self.enhance_kubernetes_type(ty)?;

                builder = builder.add_type(type_name, enhanced_ty);

                // Add this version's module to the IR
                let version_ir = builder.build();
                for module in version_ir.modules {
                    ir.add_module(module);
                }
            }
        }

        // If no versions had schemas, create an empty module
        if ir.modules.is_empty() {
            let module_name = format!("{}.{}", input.spec.names.kind, input.spec.group);
            let builder = IRBuilder::new().module(module_name);
            ir = builder.build();
        }

        Ok(ir)
    }
}

impl CRDParser {
    pub fn new() -> Self {
        Self {
            k8s_patterns: K8sTypePatterns::new(),
        }
    }

    /// Parse a specific version of a CRD
    pub fn parse_version(&self, crd: &CRD, version_name: &str) -> Result<IR, ParserError> {
        // Find the specific version
        let version = crd
            .spec
            .versions
            .iter()
            .find(|v| v.name == version_name)
            .ok_or_else(|| {
                ParserError::Parse(format!("Version {} not found in CRD", version_name))
            })?;

        if let Some(schema) = &version.schema {
            let module_name = format!(
                "{}.{}.{}",
                crd.spec.names.kind, version.name, crd.spec.group
            );
            let mut builder = IRBuilder::new().module(module_name);

            let type_name = crd.spec.names.kind.clone();
            let ty = self.json_schema_to_type(&schema.openapi_v3_schema)?;

            // Enhance the type with proper k8s fields
            let enhanced_ty = self.enhance_kubernetes_type(ty)?;

            builder = builder.add_type(type_name, enhanced_ty);
            Ok(builder.build())
        } else {
            Err(ParserError::Parse(format!(
                "Version {} has no schema",
                version_name
            )))
        }
    }

    /// Enhance a Kubernetes resource type with proper field references
    fn enhance_kubernetes_type(&self, ty: Type) -> Result<Type, ParserError> {
        if let Type::Record { mut fields, open } = ty {
            // Check for standard Kubernetes fields

            // Replace empty metadata with ObjectMeta reference
            if let Some(metadata_field) = fields.get_mut("metadata") {
                if matches!(metadata_field.ty, Type::Record { ref fields, .. } if fields.is_empty())
                {
                    metadata_field.ty = Type::Reference {
                        name: "ObjectMeta".to_string(),
                        module: Some("io.k8s.apimachinery.pkg.apis.meta.v1".to_string()),
                    };
                }
            }

            // Check for status field that might need enhancement
            if let Some(status_field) = fields.get_mut("status") {
                // Status fields often reference common condition types
                if let Type::Record {
                    fields: ref mut status_fields,
                    ..
                } = &mut status_field.ty
                {
                    if let Some(conditions_field) = status_fields.get_mut("conditions") {
                        // Conditions are often arrays of metav1.Condition
                        if matches!(conditions_field.ty, Type::Array(_)) {
                            // Could enhance with proper Condition type reference
                        }
                    }
                }
            }

            // Recursively enhance nested record types
            for field in fields.values_mut() {
                field.ty = self.enhance_field_type(field.ty.clone())?;
            }

            Ok(Type::Record { fields, open })
        } else {
            Ok(ty)
        }
    }

    /// Enhance field types using authoritative Kubernetes type patterns
    fn enhance_field_type(&self, ty: Type) -> Result<Type, ParserError> {
        self.enhance_field_type_with_context(ty, &[])
    }

    /// Enhance field types with context path for more precise matching
    fn enhance_field_type_with_context(
        &self,
        ty: Type,
        context: &[&str],
    ) -> Result<Type, ParserError> {
        match ty {
            Type::Record { fields, open } => {
                let mut enhanced_fields = fields;

                // Check for fields using authoritative patterns
                for (field_name, field) in enhanced_fields.iter_mut() {
                    // Check if we have an authoritative type for this field
                    if let Some(go_type) =
                        self.k8s_patterns.get_contextual_type(field_name, context)
                    {
                        // Convert Go type string to appropriate Nickel type
                        let replacement_type = self.go_type_string_to_nickel_type(go_type)?;

                        // Only replace if the current type matches expected pattern
                        let should_replace =
                            match (field_name.as_str(), &field.ty, go_type.as_str()) {
                                // Metadata should be ObjectMeta if it's currently empty
                                ("metadata", Type::Record { fields, .. }, _)
                                    if fields.is_empty() =>
                                {
                                    true
                                }

                                // Arrays should be replaced if currently generic
                                (_, Type::Array(_), go_type) if go_type.starts_with("[]") => true,

                                // Records should be replaced if they're empty or generic
                                (_, Type::Record { fields, .. }, _) if fields.is_empty() => true,

                                // Maps for specific patterns
                                ("nodeSelector", Type::Map { .. }, _) => false, // Keep as map

                                _ => false,
                            };

                        if should_replace {
                            field.ty = replacement_type;
                            continue;
                        }
                    }

                    // Recursively enhance nested types with updated context
                    let mut new_context = context.to_vec();
                    new_context.push(field_name);
                    field.ty =
                        self.enhance_field_type_with_context(field.ty.clone(), &new_context)?;
                }

                Ok(Type::Record {
                    fields: enhanced_fields,
                    open,
                })
            }
            Type::Array(inner) => Ok(Type::Array(Box::new(
                self.enhance_field_type_with_context(*inner, context)?,
            ))),
            Type::Optional(inner) => Ok(Type::Optional(Box::new(
                self.enhance_field_type_with_context(*inner, context)?,
            ))),
            _ => Ok(ty),
        }
    }

    /// Convert Go type string to Nickel Type
    #[allow(clippy::only_used_in_recursion)]
    fn go_type_string_to_nickel_type(&self, go_type: &str) -> Result<Type, ParserError> {
        if let Some(elem_type) = go_type.strip_prefix("[]") {
            // Array type
            let elem = self.go_type_string_to_nickel_type(elem_type)?;
            Ok(Type::Array(Box::new(elem)))
        } else if go_type.starts_with("map[") {
            // For now, keep as generic map - could be more sophisticated
            Ok(Type::Map {
                key: Box::new(Type::String),
                value: Box::new(Type::String),
            })
        } else if go_type.contains("/") {
            // Qualified type name - create reference
            Ok(Type::Reference { name: go_type.to_string(), module: None })
        } else {
            // Basic types or unqualified names
            match go_type {
                "string" => Ok(Type::String),
                "int" | "int32" | "int64" => Ok(Type::Integer),
                "float32" | "float64" => Ok(Type::Number),
                "bool" => Ok(Type::Bool),
                _ => Ok(Type::Reference { name: go_type.to_string(), module: None }),
            }
        }
    }

    #[allow(clippy::only_used_in_recursion)]
    fn json_schema_to_type(&self, schema: &serde_json::Value) -> Result<Type, ParserError> {
        use serde_json::Value;

        let schema_type = schema.get("type").and_then(|v| v.as_str());

        match schema_type {
            Some("string") => Ok(Type::String),
            Some("number") => Ok(Type::Number),
            Some("integer") => Ok(Type::Integer),
            Some("boolean") => Ok(Type::Bool),
            Some("null") => Ok(Type::Null),
            Some("array") => {
                let items = schema
                    .get("items")
                    .map(|i| self.json_schema_to_type(i))
                    .transpose()?
                    .unwrap_or(Type::Any);
                Ok(Type::Array(Box::new(items)))
            }
            Some("object") => {
                let mut fields = BTreeMap::new();
                if let Some(Value::Object(props)) = schema.get("properties") {
                    let required = schema
                        .get("required")
                        .and_then(|r| r.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str())
                                .map(String::from)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();

                    for (name, prop_schema) in props {
                        let ty = self.json_schema_to_type(prop_schema)?;
                        fields.insert(
                            name.clone(),
                            amalgam_core::types::Field {
                                ty,
                                required: required.contains(name),
                                description: prop_schema
                                    .get("description")
                                    .and_then(|d| d.as_str())
                                    .map(String::from),
                                default: prop_schema.get("default").cloned(),
                            },
                        );
                    }
                }

                let open = schema
                    .get("additionalProperties")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                Ok(Type::Record { fields, open })
            }
            _ => {
                // Check for oneOf, anyOf, allOf
                if let Some(Value::Array(schemas)) = schema.get("oneOf") {
                    let types = schemas
                        .iter()
                        .map(|s| self.json_schema_to_type(s))
                        .collect::<Result<Vec<_>, _>>()?;
                    return Ok(Type::Union {
                        types,
                        coercion_hint: None,
                    });
                }

                if let Some(Value::Array(schemas)) = schema.get("anyOf") {
                    let types = schemas
                        .iter()
                        .map(|s| self.json_schema_to_type(s))
                        .collect::<Result<Vec<_>, _>>()?;
                    return Ok(Type::Union {
                        types,
                        coercion_hint: None,
                    });
                }

                Ok(Type::Any)
            }
        }
    }
}

impl Default for CRDParser {
    fn default() -> Self {
        Self::new()
    }
}
