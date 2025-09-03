//! OpenAPI schema walker that produces uniform IR

use super::{SchemaWalker, TypeRegistry, DependencyGraph, WalkerError};
use amalgam_core::{
    ir::{Module, TypeDefinition, Import, IR},
    types::{Type, Field},
};
use openapiv3::{OpenAPI, Schema, SchemaKind, ReferenceOr, Type as OpenAPIType};
use std::collections::{HashMap, HashSet, BTreeMap};

pub struct OpenAPIWalker {
    /// Base module name for generated types
    base_module: String,
}

impl OpenAPIWalker {
    pub fn new(base_module: impl Into<String>) -> Self {
        Self {
            base_module: base_module.into(),
        }
    }
    
    /// Convert OpenAPI schema to our Type representation
    fn schema_to_type(&self, schema: &Schema, refs: &mut Vec<String>) -> Result<Type, WalkerError> {
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
                    .map(|s| self.schema_to_type(s, refs))
                    .transpose()?
                    .unwrap_or(Type::Any);
                Ok(Type::Array(Box::new(item_type)))
            }
            
            SchemaKind::Type(OpenAPIType::Object(obj)) => {
                let mut fields = BTreeMap::new();
                
                for (name, prop) in &obj.properties {
                    if let ReferenceOr::Item(schema) = prop {
                        let field_type = self.schema_to_type(schema, refs)?;
                        let required = obj.required.contains(name);
                        
                        fields.insert(
                            name.clone(),
                            Field {
                                ty: field_type,
                                required,
                                description: schema.schema_data.description.clone(),
                                default: None,
                            },
                        );
                    } else if let ReferenceOr::Reference { reference } = prop {
                        // Track reference for dependency resolution
                        refs.push(reference.clone());
                        
                        // Extract type name from reference like "#/components/schemas/TypeName"
                        let type_name = reference
                            .rsplit('/')
                            .next()
                            .unwrap_or(reference);
                            
                        fields.insert(
                            name.clone(),
                            Field {
                                ty: Type::Reference {
                                    name: type_name.to_string(),
                                    module: Some(self.base_module.clone()),
                                },
                                required: obj.required.contains(name),
                                description: None,
                                default: None,
                            },
                        );
                    }
                }
                
                Ok(Type::Record {
                    fields,
                    open: obj.additional_properties.is_some(),
                })
            }
            
            SchemaKind::OneOf { one_of } => {
                let types: Result<Vec<_>, _> = one_of
                    .iter()
                    .filter_map(|r| r.as_item())
                    .map(|s| self.schema_to_type(s, refs))
                    .collect();
                    
                Ok(Type::Union {
                    types: types?,
                    coercion_hint: None,
                })
            }
            
            SchemaKind::AllOf { .. } => {
                // TODO: Handle allOf properly
                Ok(Type::Any)
            }
            
            SchemaKind::AnyOf { .. } => {
                // TODO: Handle anyOf properly
                Ok(Type::Any)
            }
            
            SchemaKind::Not { .. } => {
                // Not supported in our type system
                Ok(Type::Any)
            }
            
            SchemaKind::Any(_) => Ok(Type::Any),
        }
    }
    
    /// Extract references from a type recursively
    fn extract_references(&self, ty: &Type, refs: &mut HashSet<String>) {
        match ty {
            Type::Reference { name, module } => {
                let fqn = if let Some(m) = module {
                    format!("{}.{}", m, name)
                } else {
                    name.clone()
                };
                refs.insert(fqn);
            }
            Type::Array(inner) => self.extract_references(inner, refs),
            Type::Optional(inner) => self.extract_references(inner, refs),
            Type::Map { value, .. } => self.extract_references(value, refs),
            Type::Record { fields, .. } => {
                for field in fields.values() {
                    self.extract_references(&field.ty, refs);
                }
            }
            Type::Union { types, .. } => {
                for t in types {
                    self.extract_references(t, refs);
                }
            }
            Type::TaggedUnion { variants, .. } => {
                for t in variants.values() {
                    self.extract_references(t, refs);
                }
            }
            Type::Contract { base, .. } => self.extract_references(base, refs),
            _ => {}
        }
    }
}

impl SchemaWalker for OpenAPIWalker {
    type Input = OpenAPI;
    
    fn walk(&self, input: Self::Input) -> Result<IR, WalkerError> {
        // Step 1: Extract all types
        let registry = self.extract_types(&input)?;
        
        // Step 2: Build dependency graph
        let deps = self.build_dependencies(&registry);
        
        // Step 3: Generate IR with imports
        self.generate_ir(registry, deps)
    }
    
    fn extract_types(&self, input: &Self::Input) -> Result<TypeRegistry, WalkerError> {
        let mut registry = TypeRegistry::new();
        
        if let Some(components) = &input.components {
            for (name, schema_ref) in &components.schemas {
                if let ReferenceOr::Item(schema) = schema_ref {
                    let mut refs = Vec::new();
                    let ty = self.schema_to_type(schema, &mut refs)?;
                    
                    let fqn = format!("{}.{}", self.base_module, name);
                    let type_def = TypeDefinition {
                        name: name.clone(),
                        ty,
                        documentation: schema.schema_data.description.clone(),
                        annotations: Default::default(),
                    };
                    
                    registry.add_type(&fqn, type_def);
                }
            }
        }
        
        Ok(registry)
    }
    
    fn build_dependencies(&self, registry: &TypeRegistry) -> DependencyGraph {
        let mut graph = DependencyGraph::new();
        
        for (fqn, type_def) in &registry.types {
            let mut refs = HashSet::new();
            self.extract_references(&type_def.ty, &mut refs);
            
            for ref_fqn in refs {
                // Only add if the referenced type exists in our registry
                if registry.types.contains_key(&ref_fqn) {
                    graph.add_dependency(fqn, &ref_fqn);
                }
            }
        }
        
        graph
    }
    
    fn generate_ir(&self, registry: TypeRegistry, deps: DependencyGraph) -> Result<IR, WalkerError> {
        let mut ir = IR::new();
        
        // Group types by module
        for (module_name, type_names) in registry.modules {
            let mut module = Module {
                name: module_name.clone(),
                imports: Vec::new(),
                types: Vec::new(),
                constants: Vec::new(),
                metadata: Default::default(),
            };
            
            // Collect all imports needed for this module
            let mut imports_map: HashMap<String, HashSet<String>> = HashMap::new();
            
            for type_name in &type_names {
                let fqn = format!("{}.{}", module_name, type_name);
                
                if let Some(type_def) = registry.types.get(&fqn) {
                    module.types.push(type_def.clone());
                    
                    // Get cross-module dependencies
                    for dep_fqn in deps.get_cross_module_deps(&fqn) {
                        // Extract module and type from dependency FQN
                        if let Some(last_dot) = dep_fqn.rfind('.') {
                            let dep_module = &dep_fqn[..last_dot];
                            let dep_type = &dep_fqn[last_dot + 1..];
                            
                            imports_map
                                .entry(dep_module.to_string())
                                .or_default()
                                .insert(dep_type.to_string());
                        }
                    }
                }
            }
            
            // Convert imports map to Import structs
            for (import_module, import_types) in imports_map {
                let import_path = self.calculate_import_path(&module_name, &import_module);
                
                module.imports.push(Import {
                    path: import_path,
                    alias: Some(self.generate_alias(&import_module)),
                    items: import_types.into_iter().collect(),
                });
            }
            
            ir.add_module(module);
        }
        
        Ok(ir)
    }
}

impl OpenAPIWalker {
    /// Calculate relative import path between modules
    fn calculate_import_path(&self, from_module: &str, to_module: &str) -> String {
        // Simple relative path calculation
        // This would need to be more sophisticated for complex module structures
        let from_parts: Vec<&str> = from_module.split('.').collect();
        let to_parts: Vec<&str> = to_module.split('.').collect();
        
        // Find common prefix
        let common_len = from_parts.iter()
            .zip(to_parts.iter())
            .take_while(|(a, b)| a == b)
            .count();
        
        // Build relative path
        let ups = from_parts.len() - common_len - 1;
        let mut path = vec![".."; ups];
        path.extend(&to_parts[common_len..]);
        
        format!("{}/mod.ncl", path.join("/"))
    }
    
    /// Generate an alias for an imported module
    fn generate_alias(&self, module: &str) -> String {
        // Use last part of module path as alias
        module.split('.').last().unwrap_or(module).to_string()
    }
}