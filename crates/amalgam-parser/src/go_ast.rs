//! Go AST parsing for precise type extraction

use crate::{imports::TypeReference, ParserError};
use amalgam_core::types::{Field, Type};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use reqwest;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

/// Go AST parser that uses go/ast to extract precise type information
pub struct GoASTParser {
    client: reqwest::Client,
    /// Cache of parsed Go types by fully qualified name
    type_cache: HashMap<String, GoTypeInfo>,
    multi_progress: Arc<MultiProgress>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoTypeInfo {
    pub name: String,
    pub package_path: String,
    pub fields: Vec<GoField>,
    pub documentation: Option<String>,
    pub type_kind: GoTypeKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoField {
    pub name: String,
    pub json_name: Option<String>, // From json tags
    pub go_type: String,           // Fully qualified Go type
    pub documentation: Option<String>,
    pub tags: HashMap<String, String>,
    pub is_pointer: bool,
    pub is_optional: bool, // Based on omitempty tag
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GoTypeKind {
    Struct,
    Interface,
    Alias,
    Basic,
}

impl Default for GoASTParser {
    fn default() -> Self {
        Self::new()
    }
}

impl GoASTParser {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .user_agent("amalgam")
                .build()
                .unwrap(),
            type_cache: HashMap::new(),
            multi_progress: Arc::new(MultiProgress::new()),
        }
    }

    /// Fetch and parse Go source files from a repository
    pub async fn fetch_and_parse_repository(
        &mut self,
        repo_url: &str,
        paths: &[&str],
    ) -> Result<(), ParserError> {
        let is_tty = atty::is(atty::Stream::Stdout);

        let main_spinner = if is_tty {
            let pb = self.multi_progress.add(ProgressBar::new_spinner());
            pb.set_style(
                ProgressStyle::default_spinner()
                    .template("{spinner:.cyan} {msg}")
                    .unwrap()
                    .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
            );
            pb.enable_steady_tick(Duration::from_millis(100));
            pb.set_message(format!("Parsing Go repository: {}", repo_url));
            Some(pb)
        } else {
            None
        };

        for path in paths {
            if let Some(ref pb) = main_spinner {
                pb.set_message(format!("Fetching Go files from {}", path));
            }

            let go_files = self.fetch_go_files(repo_url, path).await?;

            if let Some(ref pb) = main_spinner {
                pb.set_message(format!("Parsing {} Go files", go_files.len()));
            }

            self.parse_go_files(&go_files).await?;
        }

        if let Some(pb) = main_spinner {
            pb.finish_with_message(format!("✓ Parsed {} types", self.type_cache.len()));
        }

        Ok(())
    }

    /// Fetch Go files from a specific path in a repository
    async fn fetch_go_files(
        &self,
        repo_url: &str,
        path: &str,
    ) -> Result<Vec<GoSourceFile>, ParserError> {
        // Convert GitHub URL to API format
        let api_url = self.github_url_to_api(repo_url, path)?;

        let response = self
            .client
            .get(&api_url)
            .header("User-Agent", "amalgam")
            .send()
            .await
            .map_err(|e| ParserError::Network(e.to_string()))?;

        if !response.status().is_success() {
            return Err(ParserError::Network(format!(
                "Failed to fetch Go files: {}",
                response.status()
            )));
        }

        let files: Vec<GitHubFile> = response
            .json()
            .await
            .map_err(|e| ParserError::Parse(e.to_string()))?;

        let mut go_files = Vec::new();
        for file in files {
            if file.name.ends_with(".go") && file.file_type == "file" {
                let content = self.fetch_file_content(&file.download_url).await?;
                go_files.push(GoSourceFile {
                    name: file.name,
                    _path: file.path,
                    content,
                });
            }
        }

        Ok(go_files)
    }

    fn github_url_to_api(&self, repo_url: &str, path: &str) -> Result<String, ParserError> {
        // Convert https://github.com/kubernetes/api/tree/master/core/v1
        // to https://api.github.com/repos/kubernetes/api/contents/core/v1

        if let Some(github_part) = repo_url.strip_prefix("https://github.com/") {
            let parts: Vec<&str> = github_part.split("/tree/").collect();
            if parts.len() == 2 {
                let repo = parts[0];
                let branch_and_path = parts[1];
                let path_parts: Vec<&str> = branch_and_path.splitn(2, '/').collect();

                let base_path = if path_parts.len() > 1 {
                    format!("{}/{}", path_parts[1], path)
                } else {
                    path.to_string()
                };

                return Ok(format!(
                    "https://api.github.com/repos/{}/contents/{}",
                    repo, base_path
                ));
            }
        }

        Err(ParserError::Parse(format!(
            "Invalid GitHub URL: {}",
            repo_url
        )))
    }

    async fn fetch_file_content(&self, url: &str) -> Result<String, ParserError> {
        let response = self
            .client
            .get(url)
            .header("User-Agent", "amalgam")
            .send()
            .await
            .map_err(|e| ParserError::Network(e.to_string()))?;

        response
            .text()
            .await
            .map_err(|e| ParserError::Parse(e.to_string()))
    }

    /// Parse Go source files using a Go script
    async fn parse_go_files(&mut self, files: &[GoSourceFile]) -> Result<(), ParserError> {
        // Create a temporary directory with the Go files
        let temp_dir = tempfile::tempdir().map_err(ParserError::Io)?;

        // Write files to temp directory
        for file in files {
            let file_path = temp_dir.path().join(&file.name);
            tokio::fs::write(&file_path, &file.content)
                .await
                .map_err(ParserError::Io)?;
        }

        // Create a Go parser script
        let parser_script = self.create_go_parser_script()?;
        let script_path = temp_dir.path().join("parser.go");
        tokio::fs::write(&script_path, parser_script)
            .await
            .map_err(ParserError::Io)?;

        // Run the Go parser (still synchronous since it's a subprocess)
        let output = tokio::task::spawn_blocking({
            let dir = temp_dir.path().to_path_buf();
            move || {
                Command::new("go")
                    .args(["run", "parser.go"])
                    .current_dir(dir)
                    .output()
            }
        })
        .await
        .map_err(|e| ParserError::Parse(format!("Failed to spawn go parser: {}", e)))?
        .map_err(|e| ParserError::Parse(format!("Failed to run go parser: {}", e)))?;

        if !output.status.success() {
            return Err(ParserError::Parse(format!(
                "Go parser failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        // Parse the JSON output
        let json_output = String::from_utf8_lossy(&output.stdout);
        let type_infos: Vec<GoTypeInfo> = serde_json::from_str(&json_output)
            .map_err(|e| ParserError::Parse(format!("Failed to parse JSON: {}", e)))?;

        // Cache the type information
        for type_info in type_infos {
            let qualified_name = format!("{}.{}", type_info.package_path, type_info.name);
            self.type_cache.insert(qualified_name, type_info);
        }

        Ok(())
    }

    /// Create a Go script that uses go/ast to extract type information
    fn create_go_parser_script(&self) -> Result<String, ParserError> {
        Ok(r#"
package main

import (
    "encoding/json"
    "fmt"
    "go/ast"
    "go/parser"
    "go/token"
    "os"
    "path/filepath"
    "reflect"
    "strings"
)

type GoTypeInfo struct {
    Name        string            `json:"name"`
    PackagePath string            `json:"package_path"`
    Fields      []GoField         `json:"fields"`
    Documentation *string         `json:"documentation"`
    TypeKind    string            `json:"type_kind"`
}

type GoField struct {
    Name         string            `json:"name"`
    JsonName     *string           `json:"json_name"`
    GoType       string            `json:"go_type"`
    Documentation *string          `json:"documentation"`
    Tags         map[string]string `json:"tags"`
    IsPointer    bool              `json:"is_pointer"`
    IsOptional   bool              `json:"is_optional"`
}

func main() {
    fset := token.NewFileSet()
    var allTypes []GoTypeInfo
    
    err := filepath.Walk(".", func(path string, info os.FileInfo, err error) error {
        if err != nil {
            return err
        }
        
        if !strings.HasSuffix(path, ".go") || strings.HasSuffix(path, "parser.go") {
            return nil
        }
        
        node, err := parser.ParseFile(fset, path, nil, parser.ParseComments)
        if err != nil {
            return err
        }
        
        packagePath := node.Name.Name // This would need proper module resolution
        
        ast.Inspect(node, func(n ast.Node) bool {
            switch x := n.(type) {
            case *ast.TypeSpec:
                if structType, ok := x.Type.(*ast.StructType); ok {
                    typeInfo := extractStructInfo(x, structType, packagePath, node)
                    allTypes = append(allTypes, typeInfo)
                }
            }
            return true
        })
        
        return nil
    })
    
    if err != nil {
        fmt.Fprintf(os.Stderr, "Error: %v\n", err)
        os.Exit(1)
    }
    
    jsonData, err := json.MarshalIndent(allTypes, "", "  ")
    if err != nil {
        fmt.Fprintf(os.Stderr, "JSON error: %v\n", err)
        os.Exit(1)
    }
    
    fmt.Print(string(jsonData))
}

func extractStructInfo(typeSpec *ast.TypeSpec, structType *ast.StructType, packagePath string, file *ast.File) GoTypeInfo {
    var fields []GoField
    
    for _, field := range structType.Fields.List {
        for _, name := range field.Names {
            fieldInfo := GoField{
                Name:      name.Name,
                GoType:    typeToString(field.Type),
                Tags:      make(map[string]string),
                IsPointer: isPointerType(field.Type),
            }
            
            // Extract tags
            if field.Tag != nil {
                tagStr := strings.Trim(field.Tag.Value, "`")
                tags := reflect.StructTag(tagStr)
                
                if jsonTag := tags.Get("json"); jsonTag != "" {
                    parts := strings.Split(jsonTag, ",")
                    if len(parts) > 0 && parts[0] != "" && parts[0] != "-" {
                        fieldInfo.JsonName = &parts[0]
                    }
                    
                    // Check for omitempty
                    for _, part := range parts[1:] {
                        if part == "omitempty" {
                            fieldInfo.IsOptional = true
                        }
                    }
                }
                
                fieldInfo.Tags["json"] = tags.Get("json")
                fieldInfo.Tags["yaml"] = tags.Get("yaml")
            }
            
            // Extract documentation
            if field.Doc != nil {
                doc := strings.TrimSpace(field.Doc.Text())
                if doc != "" {
                    fieldInfo.Documentation = &doc
                }
            }
            
            fields = append(fields, fieldInfo)
        }
    }
    
    var doc *string
    if typeSpec.Doc != nil {
        docText := strings.TrimSpace(typeSpec.Doc.Text())
        if docText != "" {
            doc = &docText
        }
    }
    
    return GoTypeInfo{
        Name:          typeSpec.Name.Name,
        PackagePath:   packagePath,
        Fields:        fields,
        Documentation: doc,
        TypeKind:      "Struct",
    }
}

func typeToString(expr ast.Expr) string {
    switch t := expr.(type) {
    case *ast.Ident:
        return t.Name
    case *ast.StarExpr:
        return "*" + typeToString(t.X)
    case *ast.ArrayType:
        return "[]" + typeToString(t.Elt)
    case *ast.MapType:
        return "map[" + typeToString(t.Key) + "]" + typeToString(t.Value)
    case *ast.SelectorExpr:
        return typeToString(t.X) + "." + t.Sel.Name
    case *ast.InterfaceType:
        return "interface{}"
    default:
        return "unknown"
    }
}

func isPointerType(expr ast.Expr) bool {
    _, ok := expr.(*ast.StarExpr)
    return ok
}
"#.to_string())
    }

    /// Get type information for a fully qualified Go type
    pub fn get_type_info(&self, qualified_name: &str) -> Option<&GoTypeInfo> {
        self.type_cache.get(qualified_name)
    }

    /// Convert a Go type to Nickel type using precise AST information
    pub fn go_type_to_nickel(&self, go_type_info: &GoTypeInfo) -> Result<Type, ParserError> {
        let mut fields = BTreeMap::new();

        for field in &go_type_info.fields {
            let field_name = field.json_name.as_ref().unwrap_or(&field.name).to_string();

            let field_type = self.go_type_string_to_nickel(&field.go_type)?;

            // Apply pointer and optional semantics
            let final_type = if field.is_pointer || field.is_optional {
                Type::Optional(Box::new(field_type))
            } else {
                field_type
            };

            fields.insert(
                field_name,
                Field {
                    ty: final_type,
                    required: !field.is_optional && !field.is_pointer,
                    description: field.documentation.clone(),
                    default: None,
                },
            );
        }

        Ok(Type::Record {
            fields,
            open: false, // Go structs are closed by default
        })
    }

    /// Convert a Go type string to Nickel type
    #[allow(clippy::only_used_in_recursion)]
    fn go_type_string_to_nickel(&self, go_type: &str) -> Result<Type, ParserError> {
        match go_type {
            "string" => Ok(Type::String),
            "int" | "int8" | "int16" | "int32" | "int64" | "uint" | "uint8" | "uint16"
            | "uint32" | "uint64" => Ok(Type::Integer),
            "float32" | "float64" => Ok(Type::Number),
            "bool" => Ok(Type::Bool),
            "interface{}" => Ok(Type::Any),
            s if s.starts_with("[]") => {
                let elem_type = &s[2..];
                let elem = self.go_type_string_to_nickel(elem_type)?;
                Ok(Type::Array(Box::new(elem)))
            }
            s if s.starts_with("map[") => {
                // Simple map handling - could be more sophisticated
                Ok(Type::Map {
                    key: Box::new(Type::String), // Most k8s maps are string-keyed
                    value: Box::new(Type::Any),
                })
            }
            s if s.starts_with("*") => {
                // Pointer type - make it optional
                let inner_type = &s[1..];
                let inner = self.go_type_string_to_nickel(inner_type)?;
                Ok(Type::Optional(Box::new(inner)))
            }
            // Handle qualified types (e.g., metav1.ObjectMeta)
            s => Ok(Type::Reference {
                name: s.to_string(),
                module: None,
            }),
        }
    }

    /// Parse specific Kubernetes types
    pub async fn parse_k8s_core_types(
        &mut self,
    ) -> Result<HashMap<String, GoTypeInfo>, ParserError> {
        // Parse core Kubernetes types from k8s.io/api and k8s.io/apimachinery
        let repos_and_paths = vec![
            (
                "https://github.com/kubernetes/api/tree/master",
                vec!["core/v1", "apps/v1", "networking/v1"],
            ),
            (
                "https://github.com/kubernetes/apimachinery/tree/master",
                vec!["pkg/apis/meta/v1", "pkg/util/intstr", "pkg/api/resource"],
            ),
        ];

        for (repo, paths) in repos_and_paths {
            self.fetch_and_parse_repository(repo, &paths).await?;
        }

        Ok(self.type_cache.clone())
    }

    /// Clear progress bars
    pub fn finish(&self) {
        self.multi_progress.clear().ok();
    }
}

#[derive(Debug, Clone, Deserialize)]
struct GitHubFile {
    name: String,
    path: String,
    #[serde(rename = "type")]
    file_type: String,
    download_url: String,
}

#[derive(Debug, Clone)]
struct GoSourceFile {
    name: String,
    _path: String,
    content: String,
}

/// Known Kubernetes type mappings based on Go AST analysis
pub fn create_k8s_type_registry() -> HashMap<String, TypeReference> {
    let mut registry = HashMap::new();

    // Core v1 types
    let core_types = vec![
        ("ObjectMeta", "k8s.io", "v1"),
        ("TypeMeta", "k8s.io", "v1"),
        ("ListMeta", "k8s.io", "v1"),
        ("LabelSelector", "k8s.io", "v1"),
        ("Volume", "k8s.io", "v1"),
        ("VolumeMount", "k8s.io", "v1"),
        ("Container", "k8s.io", "v1"),
        ("PodSpec", "k8s.io", "v1"),
        ("ResourceRequirements", "k8s.io", "v1"),
        ("EnvVar", "k8s.io", "v1"),
        ("ConfigMapKeySelector", "k8s.io", "v1"),
        ("SecretKeySelector", "k8s.io", "v1"),
    ];

    for (kind, group, version) in core_types {
        let go_name = format!("k8s.io/api/core/{}.{}", version, kind);
        let type_ref = TypeReference::new(group.to_string(), version.to_string(), kind.to_string());
        registry.insert(go_name, type_ref);
    }

    // Meta v1 types
    let meta_types = vec![
        ("ObjectMeta", "k8s.io", "v1"),
        ("TypeMeta", "k8s.io", "v1"),
        ("ListMeta", "k8s.io", "v1"),
        ("LabelSelector", "k8s.io", "v1"),
    ];

    for (kind, group, version) in meta_types {
        let go_name = format!("k8s.io/apimachinery/pkg/apis/meta/{}.{}", version, kind);
        let type_ref = TypeReference::new(group.to_string(), version.to_string(), kind.to_string());
        registry.insert(go_name, type_ref);
    }

    registry
}
