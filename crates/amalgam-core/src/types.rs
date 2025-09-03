//! Unified type system using algebraic data types

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Hint for how to handle union types in target languages
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum UnionCoercion {
    /// Prefer string representation (e.g., for IntOrString)
    PreferString,
    /// Prefer numeric representation
    PreferNumber,
    /// No preference - generate actual union
    NoPreference,
    /// Custom handler
    Custom(String),
}

/// Core type representation - algebraic data types
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Type {
    /// Primitive types
    String,
    Number,
    Integer,
    Bool,
    Null,
    Any,

    /// Compound types
    Array(Box<Type>),
    Map {
        key: Box<Type>,
        value: Box<Type>,
    },
    Optional(Box<Type>),

    /// Product type (struct/record)
    Record {
        fields: BTreeMap<String, Field>,
        open: bool, // Whether additional fields are allowed
    },

    /// Sum type (enum/union) with optional coercion hint
    Union {
        types: Vec<Type>,
        /// Hint for how to handle this union in target languages
        coercion_hint: Option<UnionCoercion>,
    },

    /// Tagged union (discriminated)
    TaggedUnion {
        tag_field: String,
        variants: BTreeMap<String, Type>,
    },

    /// Reference to another type with optional module information
    Reference {
        name: String,
        /// Full module path if this is a cross-module reference
        /// e.g., "io.k8s.api.core.v1" for NodeSelector
        module: Option<String>,
    },

    /// Contract/refinement type
    Contract {
        base: Box<Type>,
        predicate: String, // For now, just store as string
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Field {
    pub ty: Type,
    pub required: bool,
    pub description: Option<String>,
    pub default: Option<serde_json::Value>,
}

/// Type system operations
pub struct TypeSystem {
    types: BTreeMap<String, Type>,
}

impl TypeSystem {
    pub fn new() -> Self {
        Self {
            types: BTreeMap::new(),
        }
    }

    pub fn register(&mut self, name: String, ty: Type) {
        self.types.insert(name, ty);
    }

    pub fn resolve(&self, name: &str) -> Option<&Type> {
        self.types.get(name)
    }

    pub fn is_compatible(&self, source: &Type, target: &Type) -> bool {
        match (source, target) {
            (Type::Any, _) | (_, Type::Any) => true,
            (Type::Null, Type::Optional(_)) => true,
            (s, Type::Optional(t)) => self.is_compatible(s, t),
            (Type::Integer, Type::Number) => true,
            (Type::Reference { name: s, .. }, t) => {
                if let Some(resolved) = self.resolve(s) {
                    self.is_compatible(resolved, t)
                } else {
                    false
                }
            }
            (s, Type::Reference { name: t, .. }) => {
                if let Some(resolved) = self.resolve(t) {
                    self.is_compatible(s, resolved)
                } else {
                    false
                }
            }
            (Type::Array(s), Type::Array(t)) => self.is_compatible(s, t),
            (Type::Union { types, .. }, t) => types.iter().all(|v| self.is_compatible(v, t)),
            (s, Type::Union { types, .. }) => types.iter().any(|v| self.is_compatible(s, v)),
            _ => source == target,
        }
    }
}

impl Default for TypeSystem {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_type_compatibility() {
        let mut ts = TypeSystem::new();

        // Register a custom type
        ts.register("MyString".to_string(), Type::String);

        // Test basic compatibility
        assert!(ts.is_compatible(&Type::String, &Type::String));
        assert!(ts.is_compatible(&Type::Integer, &Type::Number));
        assert!(ts.is_compatible(&Type::Null, &Type::Optional(Box::new(Type::String))));

        // Test reference resolution
        assert!(ts.is_compatible(
            &Type::Reference {
                name: "MyString".to_string(),
                module: None
            },
            &Type::String
        ));

        // Test union types
        let union = Type::Union {
            types: vec![Type::String, Type::Number],
            coercion_hint: None,
        };
        assert!(ts.is_compatible(&Type::String, &union));
        assert!(ts.is_compatible(&Type::Number, &union));
        assert!(!ts.is_compatible(&Type::Bool, &union));
    }
}
