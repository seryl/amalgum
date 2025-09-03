//! CRD walker that produces uniform IR

use super::{SchemaWalker, TypeRegistry, DependencyGraph, WalkerError};
use amalgam_core::{
    ir::{Module, TypeDefinition, Import, IR},
    types::{Type, Field},
};
use serde_json::Value;
use std::collections::{HashMap, HashSet, BTreeMap};

pub struct CRDWalker {
    /// Base module name for generated types
    base_module: String,
}

impl CRDWalker {
    pub fn new(base_module: impl Into<String>) -> Self {
        Self {
            base_module: base_module.into(),
        }
    }
    
    /// Convert JSON Schema from CRD to our Type representation
    fn json_schema_to_type(&self, schema: &Value, refs: &mut Vec<String>) -> Result<Type, WalkerError> {
        if let Some(ref_str) = schema.get("$ref").and_then(|v| v.as_str()) {
            // Handle reference
            refs.push(ref_str.to_string());
            
            // Extract type name from reference
            let type_name = ref_str
                .rsplit('/')
                .next()
                .unwrap_or(ref_str);
                
            return Ok(Type::Reference {
                name: type_name.to_string(),
                module: Some(self.base_module.clone()),
            });
        }
        
        let type_str = schema.get("type").and_then(|v| v.as_str());
        
        match type_str {
            Some("string") => Ok(Type::String),
            Some("number") => Ok(Type::Number),
            Some("integer") => Ok(Type::Integer),
            Some("boolean") => Ok(Type::Bool),
            Some("null") => Ok(Type::Null),
            
            Some("array") => {
                let items = schema.get("items");
                let item_type = if let Some(items_schema) = items {
                    self.json_schema_to_type(items_schema, refs)?
                } else {
                    Type::Any
                };
                Ok(Type::Array(Box::new(item_type)))
            }
            
            Some("object") => {
                let mut fields = BTreeMap::new();
                let required = schema
                    .get("required")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str())
                            .map(String::from)
                            .collect::<HashSet<_>>()
                    })
                    .unwrap_or_default();
                
                if let Some(properties) = schema.get("properties").and_then(|v| v.as_object()) {
                    for (name, prop_schema) in properties {
                        let field_type = self.json_schema_to_type(prop_schema, refs)?;
                        let is_required = required.contains(name);
                        let description = prop_schema
                            .get("description")
                            .and_then(|v| v.as_str())
                            .map(String::from);
                        
                        fields.insert(
                            name.clone(),
                            Field {
                                ty: field_type,
                                required: is_required,
                                description,
                                default: None,
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
            
            None => {
                // Check for oneOf, anyOf, allOf
                if let Some(one_of) = schema.get("oneOf").and_then(|v| v.as_array()) {
                    let types: Result<Vec<_>, _> = one_of
                        .iter()
                        .map(|s| self.json_schema_to_type(s, refs))
                        .collect();
                        
                    Ok(Type::Union {
                        types: types?,
                        coercion_hint: None,
                    })
                } else if let Some(any_of) = schema.get("anyOf").and_then(|v| v.as_array()) {
                    let types: Result<Vec<_>, _> = any_of
                        .iter()
                        .map(|s| self.json_schema_to_type(s, refs))
                        .collect();
                        
                    Ok(Type::Union {
                        types: types?,
                        coercion_hint: None,
                    })
                } else {
                    Ok(Type::Any)
                }
            }
            
            _ => Ok(Type::Any),
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

/// CRD input format - simplified for now
#[derive(Debug, Clone)]
pub struct CRDInput {
    pub group: String,
    pub versions: Vec<CRDVersion>,
}

#[derive(Debug, Clone)]
pub struct CRDVersion {
    pub name: String,
    pub schema: Value,
}

impl SchemaWalker for CRDWalker {
    type Input = CRDInput;
    
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
        
        for version in &input.versions {
            let module_name = format!("{}.{}", input.group, version.name);
            
            // Extract spec schema
            if let Some(spec) = version.schema
                .get("openAPIV3Schema")
                .and_then(|s| s.get("properties"))
                .and_then(|p| p.get("spec")) 
            {
                let mut refs = Vec::new();
                let ty = self.json_schema_to_type(spec, &mut refs)?;
                
                let fqn = format!("{}.Spec", module_name);
                let type_def = TypeDefinition {
                    name: "Spec".to_string(),
                    ty,
                    documentation: spec
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    annotations: Default::default(),
                };
                
                registry.add_type(&fqn, type_def);
            }
            
            // Extract status schema if present
            if let Some(status) = version.schema
                .get("openAPIV3Schema")
                .and_then(|s| s.get("properties"))
                .and_then(|p| p.get("status")) 
            {
                let mut refs = Vec::new();
                let ty = self.json_schema_to_type(status, &mut refs)?;
                
                let fqn = format!("{}.Status", module_name);
                let type_def = TypeDefinition {
                    name: "Status".to_string(),
                    ty,
                    documentation: status
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    annotations: Default::default(),
                };
                
                registry.add_type(&fqn, type_def);
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
                // Check if this is a k8s core type reference
                if ref_fqn.starts_with("io.k8s.") {
                    // Add as external dependency
                    graph.add_dependency(fqn, &ref_fqn);
                } else if registry.types.contains_key(&ref_fqn) {
                    // Internal dependency
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
                        // Handle k8s core type imports specially
                        if dep_fqn.starts_with("io.k8s.") {
                            // Map to our k8s package structure
                            let import_path = self.map_k8s_import_path(&dep_fqn);
                            let type_name = dep_fqn.rsplit('.').next().unwrap_or(&dep_fqn);
                            
                            imports_map
                                .entry(import_path)
                                .or_default()
                                .insert(type_name.to_string());
                        } else if let Some(last_dot) = dep_fqn.rfind('.') {
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
                let import_path = if import_module.starts_with("../") {
                    // Already a path
                    import_module.clone()
                } else {
                    self.calculate_import_path(&module_name, &import_module)
                };
                
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

impl CRDWalker {
    /// Map k8s core type references to import paths
    fn map_k8s_import_path(&self, fqn: &str) -> String {
        // Extract version and type from FQN like "io.k8s.api.core.v1.ObjectMeta"
        if fqn.starts_with("io.k8s.apimachinery.pkg.apis.meta.") {
            // Meta types are in v1
            "../../../k8s_io/v1".to_string()
        } else if fqn.starts_with("io.k8s.api.core.") {
            // Core types - extract version
            let parts: Vec<&str> = fqn.split('.').collect();
            if let Some(version) = parts.get(4) {
                format!("../../../k8s_io/{}", version)
            } else {
                "../../../k8s_io/v1".to_string()
            }
        } else {
            // Default to v1
            "../../../k8s_io/v1".to_string()
        }
    }
    
    /// Calculate relative import path between modules
    fn calculate_import_path(&self, from_module: &str, to_module: &str) -> String {
        // Simple relative path calculation
        let from_parts: Vec<&str> = from_module.split('.').collect();
        let to_parts: Vec<&str> = to_module.split('.').collect();
        
        // Find common prefix
        let common_len = from_parts.iter()
            .zip(to_parts.iter())
            .take_while(|(a, b)| a == b)
            .count();
        
        // Build relative path
        let ups = from_parts.len() - common_len;
        let mut path = vec![".."; ups];
        path.extend(&to_parts[common_len..]);
        
        format!("{}/mod.ncl", path.join("/"))
    }
    
    /// Generate an alias for an imported module
    fn generate_alias(&self, module: &str) -> String {
        if module.starts_with("../") {
            // Extract meaningful part from path
            module
                .rsplit('/')
                .find(|s| !s.is_empty() && *s != "..")
                .unwrap_or("import")
                .to_string()
        } else {
            // Use last part of module path as alias
            module.split('.').last().unwrap_or(module).to_string()
        }
    }
}