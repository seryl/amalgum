//! Go source code parser

use crate::{Parser, ParserError};
use amalgam_core::{
    ir::{IRBuilder, IR},
    types::{Field, Type},
};
use std::collections::BTreeMap;

/// Simplified Go AST representation
#[derive(Debug)]
pub struct GoFile {
    pub package: String,
    pub imports: Vec<String>,
    pub types: Vec<GoTypeDecl>,
}

#[derive(Debug)]
pub struct GoTypeDecl {
    pub name: String,
    pub ty: GoType,
}

#[derive(Debug)]
pub enum GoType {
    Struct {
        fields: Vec<GoField>,
    },
    Interface {
        methods: Vec<GoMethod>,
    },
    Alias(Box<GoType>),
    Basic(String),
    Array(Box<GoType>),
    Slice(Box<GoType>),
    Map {
        key: Box<GoType>,
        value: Box<GoType>,
    },
    Pointer(Box<GoType>),
}

#[derive(Debug)]
pub struct GoField {
    pub name: String,
    pub ty: GoType,
    pub tag: Option<String>,
}

#[derive(Debug)]
pub struct GoMethod {
    pub name: String,
    pub params: Vec<GoType>,
    pub returns: Vec<GoType>,
}

pub struct GoParser;

impl Parser for GoParser {
    type Input = GoFile;

    fn parse(&self, input: Self::Input) -> Result<IR, ParserError> {
        let mut builder = IRBuilder::new().module(&input.package);

        // Add imports
        for import in input.imports {
            builder = builder.add_import(import);
        }

        // Convert types
        for type_decl in input.types {
            let ty = self.go_type_to_type(&type_decl.ty)?;
            builder = builder.add_type(type_decl.name, ty);
        }

        Ok(builder.build())
    }
}

impl GoParser {
    pub fn new() -> Self {
        Self
    }

    fn go_type_to_type(&self, go_type: &GoType) -> Result<Type, ParserError> {
        match go_type {
            GoType::Basic(name) => match name.as_str() {
                "string" => Ok(Type::String),
                "int" | "int8" | "int16" | "int32" | "int64" | "uint" | "uint8" | "uint16"
                | "uint32" | "uint64" => Ok(Type::Integer),
                "float32" | "float64" => Ok(Type::Number),
                "bool" => Ok(Type::Bool),
                "byte" => Ok(Type::Integer), // byte is alias for uint8
                "rune" => Ok(Type::Integer), // rune is alias for int32
                "interface{}" | "any" => Ok(Type::Any),
                _ => Ok(Type::Reference {
                    name: name.clone(),
                    module: None,
                }),
            },
            GoType::Struct { fields } => {
                let mut record_fields = BTreeMap::new();
                for field in fields {
                    let field_type = self.go_type_to_type(&field.ty)?;
                    let (name, required) = self.parse_field_tag(&field.name, &field.tag);
                    record_fields.insert(
                        name,
                        Field {
                            ty: field_type,
                            required,
                            description: None,
                            default: None,
                        },
                    );
                }
                Ok(Type::Record {
                    fields: record_fields,
                    open: false,
                })
            }
            GoType::Interface { methods: _ } => {
                // For now, interfaces become contracts
                Ok(Type::Contract {
                    base: Box::new(Type::Any),
                    predicate: "interface".to_string(),
                })
            }
            GoType::Alias(inner) => self.go_type_to_type(inner),
            GoType::Array(elem) | GoType::Slice(elem) => {
                let elem_type = self.go_type_to_type(elem)?;
                Ok(Type::Array(Box::new(elem_type)))
            }
            GoType::Map { key, value } => {
                let key_type = self.go_type_to_type(key)?;
                let value_type = self.go_type_to_type(value)?;
                Ok(Type::Map {
                    key: Box::new(key_type),
                    value: Box::new(value_type),
                })
            }
            GoType::Pointer(inner) => {
                let inner_type = self.go_type_to_type(inner)?;
                Ok(Type::Optional(Box::new(inner_type)))
            }
        }
    }

    fn parse_field_tag(&self, field_name: &str, tag: &Option<String>) -> (String, bool) {
        if let Some(tag_str) = tag {
            // Parse JSON tag if present
            if let Some(json_tag) = self.extract_json_tag(tag_str) {
                let parts: Vec<&str> = json_tag.split(',').collect();
                if let Some(name) = parts.first() {
                    if *name != "-" {
                        let required = !parts.contains(&"omitempty");
                        return (name.to_string(), required);
                    }
                }
            }
        }
        (field_name.to_string(), true)
    }

    fn extract_json_tag(&self, tag: &str) -> Option<String> {
        // Simple JSON tag extraction - in real implementation would use proper parsing
        if let Some(start) = tag.find("json:\"") {
            let tag_content = &tag[start + 6..];
            if let Some(end) = tag_content.find('"') {
                return Some(tag_content[..end].to_string());
            }
        }
        None
    }
}

impl Default for GoParser {
    fn default() -> Self {
        Self::new()
    }
}
