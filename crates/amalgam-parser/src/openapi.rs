//! OpenAPI/JSON Schema parser

use crate::{Parser, ParserError};
use amalgam_core::{
    ir::{IRBuilder, IR},
    types::{Field, Type},
};
use openapiv3::{OpenAPI, Schema, SchemaKind, Type as OpenAPIType};
use std::collections::BTreeMap;

pub struct OpenAPIParser;

impl Parser for OpenAPIParser {
    type Input = OpenAPI;

    fn parse(&self, input: Self::Input) -> Result<IR, ParserError> {
        let mut builder = IRBuilder::new().module("openapi");

        // Parse components/schemas
        if let Some(components) = input.components {
            for (name, schema_ref) in components.schemas {
                if let openapiv3::ReferenceOr::Item(schema) = schema_ref {
                    let ty = self.schema_to_type(&schema)?;
                    builder = builder.add_type(name, ty);
                }
            }
        }

        Ok(builder.build())
    }
}

impl OpenAPIParser {
    pub fn new() -> Self {
        Self
    }

    #[allow(clippy::only_used_in_recursion)]
    fn schema_to_type(&self, schema: &Schema) -> Result<Type, ParserError> {
        match &schema.schema_kind {
            SchemaKind::Type(OpenAPIType::String(_)) => Ok(Type::String),
            SchemaKind::Type(OpenAPIType::Number(_)) => Ok(Type::Number),
            SchemaKind::Type(OpenAPIType::Integer(_)) => Ok(Type::Integer),
            SchemaKind::Type(OpenAPIType::Boolean(_)) => Ok(Type::Bool),
            SchemaKind::Type(OpenAPIType::Array(array_type)) => {
                let item_type = array_type
                    .items
                    .as_ref()
                    .and_then(|i| i.as_item())
                    .map(|s| self.schema_to_type(s))
                    .transpose()?
                    .unwrap_or(Type::Any);
                Ok(Type::Array(Box::new(item_type)))
            }
            SchemaKind::Type(OpenAPIType::Object(object_type)) => {
                let mut fields = BTreeMap::new();
                for (field_name, field_schema_ref) in &object_type.properties {
                    if let openapiv3::ReferenceOr::Item(field_schema) = field_schema_ref {
                        let field_type = self.schema_to_type(field_schema)?;
                        let required = object_type.required.contains(field_name);
                        fields.insert(
                            field_name.clone(),
                            Field {
                                ty: field_type,
                                required,
                                description: field_schema.schema_data.description.clone(),
                                default: None,
                            },
                        );
                    }
                }
                Ok(Type::Record {
                    fields,
                    open: object_type.additional_properties.is_some(),
                })
            }
            SchemaKind::OneOf { one_of } => {
                let mut types = Vec::new();
                for schema_ref in one_of {
                    if let openapiv3::ReferenceOr::Item(schema) = schema_ref {
                        types.push(self.schema_to_type(schema)?);
                    }
                }
                Ok(Type::Union {
                    types,
                    coercion_hint: None,
                })
            }
            SchemaKind::AllOf { all_of: _ } => {
                // For now, treat as Any - would need more complex merging
                Ok(Type::Any)
            }
            SchemaKind::AnyOf { any_of } => {
                let mut types = Vec::new();
                for schema_ref in any_of {
                    if let openapiv3::ReferenceOr::Item(schema) = schema_ref {
                        types.push(self.schema_to_type(schema)?);
                    }
                }
                Ok(Type::Union {
                    types,
                    coercion_hint: None,
                })
            }
            SchemaKind::Not { .. } => {
                Err(ParserError::UnsupportedFeature("'not' schema".to_string()))
            }
            SchemaKind::Any(_) => Ok(Type::Any),
        }
    }
}

impl Default for OpenAPIParser {
    fn default() -> Self {
        Self::new()
    }
}
