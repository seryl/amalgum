//! Import resolution for cross-package type references


/// Represents a type reference that needs to be imported
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TypeReference {
    /// Group (e.g., "k8s.io", "apiextensions.crossplane.io")
    pub group: String,
    /// Version (e.g., "v1", "v1beta1")
    pub version: String,
    /// Kind (e.g., "ObjectMeta", "Volume")
    pub kind: String,
}

impl TypeReference {
    pub fn new(group: String, version: String, kind: String) -> Self {
        Self {
            group,
            version,
            kind,
        }
    }

    /// Parse a fully qualified type reference like "io.k8s.api.core.v1.ObjectMeta"
    pub fn from_qualified_name(name: &str) -> Option<Self> {
        // Handle various formats:
        // - io.k8s.api.core.v1.ObjectMeta
        // - k8s.io/api/core/v1.ObjectMeta
        // - v1.ObjectMeta (assume k8s.io/api/core)

        if name.starts_with("io.k8s.") {
            // Handle various k8s formats:
            // - io.k8s.api.core.v1.Pod
            // - io.k8s.apimachinery.pkg.apis.meta.v1.ObjectMeta
            let parts: Vec<&str> = name.split('.').collect();

            if name.starts_with("io.k8s.apimachinery.pkg.apis.meta.") && parts.len() >= 8 {
                // Special case for apimachinery types
                let version = parts[parts.len() - 2].to_string();
                let kind = parts[parts.len() - 1].to_string();
                return Some(Self::new("k8s.io".to_string(), version, kind));
            } else if name.starts_with("io.k8s.api.") && parts.len() >= 5 {
                // Standard API types
                let group = if parts[3] == "core" {
                    "k8s.io".to_string()
                } else {
                    format!("{}.k8s.io", parts[3])
                };
                let version = parts[parts.len() - 2].to_string();
                let kind = parts[parts.len() - 1].to_string();
                return Some(Self::new(group, version, kind));
            }
        } else if name.contains('/') {
            // Format: k8s.io/api/core/v1.ObjectMeta
            let parts: Vec<&str> = name.split('/').collect();
            if let Some(last) = parts.last() {
                let type_parts: Vec<&str> = last.split('.').collect();
                if type_parts.len() == 2 {
                    let version = type_parts[0].to_string();
                    let kind = type_parts[1].to_string();
                    let group = parts[0].to_string();
                    return Some(Self::new(group, version, kind));
                }
            }
        } else if name.starts_with("v1.")
            || name.starts_with("v1beta1.")
            || name.starts_with("v1alpha1.")
        {
            // Short format: v1.ObjectMeta (assume core k8s types)
            let parts: Vec<&str> = name.split('.').collect();
            if parts.len() == 2 {
                return Some(Self::new(
                    "k8s.io".to_string(),
                    parts[0].to_string(),
                    parts[1].to_string(),
                ));
            }
        }

        None
    }

    /// Get the import path for this reference relative to a base path
    pub fn import_path(&self, from_group: &str, from_version: &str) -> String {
        // Generic approach: Calculate the relative path between any two files
        // Package layout convention:
        //   vendor_dir/
        //     ├── package_dir/        <- derived from group name
        //     │   └── [group_path]/version/file.ncl
        //     └── other_package/
        //         └── [group_path]/version/file.ncl

        // Helper to derive package directory from group name
        let group_to_package = |group: &str| -> String {
            // Convention:
            // - Replace dots with underscores for filesystem compatibility
            // - If the result would be just an org name (e.g., "crossplane_io"),
            //   try to extract a more meaningful package name
            let sanitized = group.replace('.', "_");

            // If it ends with a common TLD pattern, extract the org name
            if group.contains('.') {
                // For domains like "apiextensions.crossplane.io", we want "crossplane"
                // For domains like "k8s.io", we want "k8s_io"
                let parts: Vec<&str> = group.split('.').collect();
                if parts.len() >= 2
                    && (parts.last() == Some(&"io")
                        || parts.last() == Some(&"com")
                        || parts.last() == Some(&"org"))
                {
                    // If there's a clear org name, use it
                    if parts.len() == 2 {
                        // Simple case like "k8s.io" -> "k8s_io"
                        sanitized
                    } else if parts.len() >= 3 {
                        // Complex case like "apiextensions.crossplane.io"
                        // Take the second-to-last part as the org name
                        parts[parts.len() - 2].to_string()
                    } else {
                        sanitized
                    }
                } else {
                    sanitized
                }
            } else {
                sanitized
            }
        };

        // Helper to determine if a group needs its own subdirectory within the package
        let needs_group_subdir = |group: &str, package: &str| -> bool {
            // If the package name is derived from only part of the group,
            // we need a subdirectory for the full group
            let sanitized = group.replace('.', "_");
            sanitized != package && group.contains('.')
        };

        // Build the from path components
        let from_package = group_to_package(from_group);
        let mut from_components: Vec<String> = Vec::new();
        from_components.push(from_package.clone());

        if needs_group_subdir(from_group, &from_package) {
            from_components.push(from_group.to_string());
        }
        from_components.push(from_version.to_string());

        // Build the target path components
        let target_package = group_to_package(&self.group);
        let mut to_components: Vec<String> = Vec::new();
        to_components.push(target_package.clone());

        if needs_group_subdir(&self.group, &target_package) {
            to_components.push(self.group.clone());
        }
        to_components.push(self.version.clone());
        to_components.push(format!("{}.ncl", self.kind.to_lowercase()));

        // Calculate the relative path
        // From a file at: vendor/package1/group/version/file.ncl
        // We need to go up to vendor/ then down to package2/...
        // The number of ../ equals the depth from the file to the vendor directory
        // which is the number of path components minus the vendor itself
        let up_count = from_components.len();
        let up_dirs = "../".repeat(up_count);
        let down_path = to_components.join("/");

        format!("{}{}", up_dirs, down_path)
    }

    /// Get the module alias for imports
    pub fn module_alias(&self) -> String {
        format!(
            "{}_{}",
            self.group.replace(['.', '-'], "_"),
            self.version.replace('-', "_")
        )
    }
}


/// Common Kubernetes types that are frequently referenced
pub fn common_k8s_types() -> Vec<TypeReference> {
    vec![
        TypeReference::new(
            "k8s.io".to_string(),
            "v1".to_string(),
            "ObjectMeta".to_string(),
        ),
        TypeReference::new(
            "k8s.io".to_string(),
            "v1".to_string(),
            "ListMeta".to_string(),
        ),
        TypeReference::new(
            "k8s.io".to_string(),
            "v1".to_string(),
            "TypeMeta".to_string(),
        ),
        TypeReference::new(
            "k8s.io".to_string(),
            "v1".to_string(),
            "LabelSelector".to_string(),
        ),
        TypeReference::new("k8s.io".to_string(), "v1".to_string(), "Volume".to_string()),
        TypeReference::new(
            "k8s.io".to_string(),
            "v1".to_string(),
            "VolumeMount".to_string(),
        ),
        TypeReference::new(
            "k8s.io".to_string(),
            "v1".to_string(),
            "Container".to_string(),
        ),
        TypeReference::new(
            "k8s.io".to_string(),
            "v1".to_string(),
            "PodSpec".to_string(),
        ),
        TypeReference::new(
            "k8s.io".to_string(),
            "v1".to_string(),
            "ResourceRequirements".to_string(),
        ),
        TypeReference::new(
            "k8s.io".to_string(),
            "v1".to_string(),
            "Affinity".to_string(),
        ),
        TypeReference::new(
            "k8s.io".to_string(),
            "v1".to_string(),
            "Toleration".to_string(),
        ),
        TypeReference::new("k8s.io".to_string(), "v1".to_string(), "EnvVar".to_string()),
        TypeReference::new(
            "k8s.io".to_string(),
            "v1".to_string(),
            "ConfigMapKeySelector".to_string(),
        ),
        TypeReference::new(
            "k8s.io".to_string(),
            "v1".to_string(),
            "SecretKeySelector".to_string(),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_qualified_name() {
        let ref1 = TypeReference::from_qualified_name("io.k8s.api.core.v1.ObjectMeta");
        assert!(ref1.is_some());
        let ref1 = ref1.unwrap();
        assert_eq!(ref1.group, "k8s.io");
        assert_eq!(ref1.version, "v1");
        assert_eq!(ref1.kind, "ObjectMeta");

        let ref2 = TypeReference::from_qualified_name("v1.Volume");
        assert!(ref2.is_some());
        let ref2 = ref2.unwrap();
        assert_eq!(ref2.group, "k8s.io");
        assert_eq!(ref2.version, "v1");
        assert_eq!(ref2.kind, "Volume");
    }

    #[test]
    fn test_import_path() {
        let type_ref = TypeReference::new(
            "k8s.io".to_string(),
            "v1".to_string(),
            "ObjectMeta".to_string(),
        );

        // Test with a Crossplane group (2+ dots)
        let path = type_ref.import_path("apiextensions.crossplane.io", "v1");
        assert_eq!(path, "../../../k8s_io/v1/objectmeta.ncl");

        // Test with a simple group (1 dot)
        let path2 = type_ref.import_path("example.io", "v1");
        assert_eq!(path2, "../../k8s_io/v1/objectmeta.ncl");
    }
}
