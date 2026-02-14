//! Code analyzer for extracting API information from Rust codebase.
//!
//! This module provides functionality to analyze Rust workspace crates and extract
//! public API signatures, documentation, and metadata for validation against
//! documentation files.

use crate::error::{AuditError, Result};
use crate::parser::{ApiItemType, ApiReference};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use syn::spanned::Spanned;
use syn::{
    Attribute, Expr, Item, ItemConst, ItemEnum, ItemFn, ItemImpl, ItemStatic, ItemStruct,
    ItemTrait, ItemType, Lit, Meta, Visibility,
};
use tracing::{debug, info, instrument, warn};
use walkdir::WalkDir;

/// Registry of all crates in the workspace with their public APIs.
#[derive(Debug, Clone)]
pub struct CrateRegistry {
    /// Map of crate name to crate information.
    pub crates: HashMap<String, CrateInfo>,
}

/// Information about a single crate in the workspace.
#[derive(Debug, Clone)]
pub struct CrateInfo {
    /// Name of the crate.
    pub name: String,
    /// Version of the crate.
    pub version: String,
    /// Path to the crate directory.
    pub path: PathBuf,
    /// All public APIs exposed by this crate.
    pub public_apis: Vec<PublicApi>,
    /// Feature flags defined in Cargo.toml.
    pub feature_flags: Vec<String>,
    /// Dependencies listed in Cargo.toml.
    pub dependencies: Vec<Dependency>,
    /// Rust version requirement.
    pub rust_version: Option<String>,
}

/// Information about a public API item.
#[derive(Debug, Clone)]
pub struct PublicApi {
    /// Full path to the API item (e.g., "my_crate::module::function").
    pub path: String,
    /// String representation of the signature.
    pub signature: String,
    /// Type of API item.
    pub item_type: ApiItemType,
    /// Documentation comment if present.
    pub documentation: Option<String>,
    /// Whether the API is marked as deprecated.
    pub deprecated: bool,
    /// Source file where this API is defined.
    pub source_file: PathBuf,
    /// Line number in the source file.
    pub line_number: usize,
}

/// Information about a dependency.
#[derive(Debug, Clone)]
pub struct Dependency {
    /// Name of the dependency.
    pub name: String,
    /// Version requirement.
    pub version: String,
    /// Whether it's optional.
    pub optional: bool,
    /// Features enabled for this dependency.
    pub features: Vec<String>,
}

/// Result of validating an API reference.
#[derive(Debug, Clone)]
pub struct ValidationResult {
    /// Whether the validation was successful.
    pub success: bool,
    /// Error messages if validation failed.
    pub errors: Vec<String>,
    /// Warning messages.
    pub warnings: Vec<String>,
    /// Suggestions for fixing issues.
    pub suggestions: Vec<String>,
    /// The actual API that was found (if any).
    pub found_api: Option<PublicApi>,
}

/// Code analyzer for extracting API information from Rust workspace.
pub struct CodeAnalyzer {
    /// Path to the workspace root.
    workspace_path: PathBuf,
    /// Cached crate registry.
    crate_registry: Option<CrateRegistry>,
}

impl CodeAnalyzer {
    /// Create a new code analyzer for the given workspace.
    pub fn new(workspace_path: PathBuf) -> Self {
        Self { workspace_path, crate_registry: None }
    }

    /// Analyze the entire workspace and build a registry of all crates and their APIs.
    #[instrument(skip(self))]
    pub async fn analyze_workspace(&mut self) -> Result<&CrateRegistry> {
        info!("Starting workspace analysis at: {}", self.workspace_path.display());

        let mut crates = HashMap::new();

        // Find all Cargo.toml files in the workspace
        let cargo_files = self.find_cargo_files()?;
        info!("Found {} Cargo.toml files", cargo_files.len());

        for cargo_path in cargo_files {
            if let Some(crate_info) = self.analyze_crate(&cargo_path).await? {
                debug!("Analyzed crate: {}", crate_info.name);
                crates.insert(crate_info.name.clone(), crate_info);
            }
        }

        let registry = CrateRegistry { crates };
        self.crate_registry = Some(registry);

        info!(
            "Workspace analysis complete. Found {} crates",
            self.crate_registry.as_ref().unwrap().crates.len()
        );
        Ok(self.crate_registry.as_ref().unwrap())
    }

    /// Get the cached crate registry, analyzing the workspace if not already done.
    pub async fn get_registry(&mut self) -> Result<&CrateRegistry> {
        if self.crate_registry.is_none() {
            self.analyze_workspace().await?;
        }
        Ok(self.crate_registry.as_ref().unwrap())
    }

    /// Validate an API reference against the analyzed crates.
    #[instrument(skip(self))]
    pub async fn validate_api_reference(
        &mut self,
        api_ref: &ApiReference,
    ) -> Result<ValidationResult> {
        // Get registry first
        let registry = self.get_registry().await?;

        debug!("Validating API reference: {}::{}", api_ref.crate_name, api_ref.item_path);

        // Check if the crate exists
        let crate_info = match registry.crates.get(&api_ref.crate_name) {
            Some(info) => info,
            None => {
                // Create suggestion using helper method
                let suggestion = Self::suggest_similar_crate_names(
                    &api_ref.crate_name,
                    registry,
                );
                return Ok(ValidationResult {
                    success: false,
                    errors: vec![format!("Crate '{}' not found in workspace", api_ref.crate_name)],
                    warnings: vec![],
                    suggestions: vec![suggestion],
                    found_api: None,
                });
            }
        };

        // Look for the specific API in the crate
        let matching_apis: Vec<&PublicApi> = crate_info
            .public_apis
            .iter()
            .filter(|api| {
                api.path.ends_with(&api_ref.item_path) && api.item_type == api_ref.item_type
            })
            .collect();

        match matching_apis.len() {
            0 => {
                let suggestion = Self::suggest_similar_api_names(
                    &api_ref.item_path,
                    crate_info,
                );
                Ok(ValidationResult {
                    success: false,
                    errors: vec![format!(
                        "API '{}' of type '{:?}' not found in crate '{}'",
                        api_ref.item_path, api_ref.item_type, api_ref.crate_name
                    )],
                    warnings: vec![],
                    suggestions: vec![suggestion],
                    found_api: None,
                })
            }
            1 => {
                let found_api = matching_apis[0].clone();
                let mut warnings = vec![];

                // Check if the API is deprecated
                if found_api.deprecated {
                    warnings.push(format!("API '{}' is deprecated", api_ref.item_path));
                }

                Ok(ValidationResult {
                    success: true,
                    errors: vec![],
                    warnings,
                    suggestions: vec![],
                    found_api: Some(found_api),
                })
            }
            _ => Ok(ValidationResult {
                success: false,
                errors: vec![format!(
                    "Multiple APIs matching '{}' found in crate '{}'. Please be more specific.",
                    api_ref.item_path, api_ref.crate_name
                )],
                warnings: vec![],
                suggestions: matching_apis.iter().map(|api| api.path.clone()).collect(),
                found_api: None,
            }),
        }
    }

    /// Find APIs that exist in the codebase but are not documented.
    #[instrument(skip(self, documented_apis))]
    pub async fn find_undocumented_apis(
        &mut self,
        documented_apis: &[ApiReference],
    ) -> Result<Vec<PublicApi>> {
        let registry = self.get_registry().await?;
        let mut undocumented = Vec::new();

        // Create a set of documented API paths for quick lookup
        let documented_paths: std::collections::HashSet<String> = documented_apis
            .iter()
            .map(|api| format!("{}::{}", api.crate_name, api.item_path))
            .collect();

        // Check each API in each crate
        for crate_info in registry.crates.values() {
            for api in &crate_info.public_apis {
                let full_path = format!("{}::{}", crate_info.name, api.path);
                if !documented_paths.contains(&full_path) {
                    undocumented.push(api.clone());
                }
            }
        }

        info!("Found {} undocumented APIs", undocumented.len());
        Ok(undocumented)
    }

    /// Validate that a function signature matches the documented signature.
    #[instrument(skip(self))]
    pub async fn validate_function_signature(
        &mut self,
        api_ref: &ApiReference,
        expected_signature: &str,
    ) -> Result<ValidationResult> {
        let validation_result = self.validate_api_reference(api_ref).await?;

        if !validation_result.success {
            return Ok(validation_result);
        }

        if let Some(found_api) = &validation_result.found_api {
            // Compare signatures (simplified comparison)
            let normalized_expected = self.normalize_signature(expected_signature);
            let normalized_found = self.normalize_signature(&found_api.signature);

            if normalized_expected == normalized_found {
                Ok(validation_result)
            } else {
                Ok(ValidationResult {
                    success: false,
                    errors: vec![format!(
                        "Function signature mismatch for '{}'. Expected: '{}', Found: '{}'",
                        api_ref.item_path, expected_signature, found_api.signature
                    )],
                    warnings: validation_result.warnings,
                    suggestions: vec![format!(
                        "Update documentation to use: {}",
                        found_api.signature
                    )],
                    found_api: validation_result.found_api,
                })
            }
        } else {
            Ok(validation_result)
        }
    }

    /// Validate that struct fields mentioned in documentation exist.
    #[instrument(skip(self))]
    pub async fn validate_struct_fields(
        &mut self,
        api_ref: &ApiReference,
        expected_fields: &[String],
    ) -> Result<ValidationResult> {
        let validation_result = self.validate_api_reference(api_ref).await?;

        if !validation_result.success {
            return Ok(validation_result);
        }

        if let Some(found_api) = &validation_result.found_api {
            // Extract field names from the struct signature
            let actual_fields = self.extract_struct_fields(&found_api.signature);
            let missing_fields: Vec<&String> =
                expected_fields.iter().filter(|field| !actual_fields.contains(field)).collect();

            if missing_fields.is_empty() {
                Ok(validation_result)
            } else {
                Ok(ValidationResult {
                    success: false,
                    errors: vec![format!(
                        "Struct '{}' is missing fields: {}",
                        api_ref.item_path,
                        missing_fields.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
                    )],
                    warnings: validation_result.warnings,
                    suggestions: vec![format!("Available fields: {}", actual_fields.join(", "))],
                    found_api: validation_result.found_api,
                })
            }
        } else {
            Ok(validation_result)
        }
    }

    /// Validate that import statements are valid for the current crate structure.
    #[instrument(skip(self))]
    pub async fn validate_import_statement(
        &mut self,
        import_path: &str,
    ) -> Result<ValidationResult> {
        let registry = self.get_registry().await?;

        debug!("Validating import statement: {}", import_path);

        // Parse the import path (e.g., "adk_core::Llm" or "crate::module::Type")
        let parts: Vec<&str> = import_path.split("::").collect();
        if parts.is_empty() {
            return Ok(ValidationResult {
                success: false,
                errors: vec!["Invalid import path format".to_string()],
                warnings: vec![],
                suggestions: vec![],
                found_api: None,
            });
        }

        let crate_name = parts[0].replace('_', "-"); // Convert snake_case to kebab-case for crate names

        // Check if the crate exists
        let crate_info = match registry.crates.get(&crate_name) {
            Some(info) => info,
            None => {
                // Try with the original name in case it's already kebab-case
                match registry.crates.get(parts[0]) {
                    Some(info) => info,
                    None => {
                        let suggestion =
                            Self::suggest_similar_crate_names(parts[0], registry);
                        return Ok(ValidationResult {
                            success: false,
                            errors: vec![format!("Crate '{}' not found in workspace", parts[0])],
                            warnings: vec![],
                            suggestions: vec![suggestion],
                            found_api: None,
                        });
                    }
                }
            }
        };

        // If it's just a crate import, that's valid
        if parts.len() == 1 {
            return Ok(ValidationResult {
                success: true,
                errors: vec![],
                warnings: vec![],
                suggestions: vec![],
                found_api: None,
            });
        }

        // Check if the specific item exists in the crate
        let item_path = parts[1..].join("::");
        let matching_apis: Vec<&PublicApi> =
            crate_info.public_apis.iter().filter(|api| api.path.ends_with(&item_path)).collect();

        if matching_apis.is_empty() {
            let suggestion =
                Self::suggest_similar_api_names(&item_path, crate_info);
            Ok(ValidationResult {
                success: false,
                errors: vec![format!("Item '{}' not found in crate '{}'", item_path, crate_name)],
                warnings: vec![],
                suggestions: vec![suggestion],
                found_api: None,
            })
        } else {
            Ok(ValidationResult {
                success: true,
                errors: vec![],
                warnings: vec![],
                suggestions: vec![],
                found_api: Some(matching_apis[0].clone()),
            })
        }
    }

    /// Validate that method names mentioned in documentation exist on the specified type.
    #[instrument(skip(self))]
    pub async fn validate_method_exists(
        &mut self,
        type_ref: &ApiReference,
        method_name: &str,
    ) -> Result<ValidationResult> {
        debug!(
            "Validating method '{}' exists on type '{}::{}'",
            method_name, type_ref.crate_name, type_ref.item_path
        );

        // First validate that the type exists
        let type_validation = self.validate_api_reference(type_ref).await?;
        if !type_validation.success {
            return Ok(type_validation);
        }

        // Get registry again to avoid borrow checker issues
        let registry = self.get_registry().await?;

        // Look for methods on this type
        let crate_info = registry.crates.get(&type_ref.crate_name).unwrap();
        let type_name = type_ref.item_path.split("::").last().unwrap_or(&type_ref.item_path);

        let method_path = format!("{}::{}", type_ref.item_path, method_name);
        let matching_methods: Vec<&PublicApi> = crate_info
            .public_apis
            .iter()
            .filter(|api| api.item_type == ApiItemType::Method && api.path.ends_with(&method_path))
            .collect();

        if matching_methods.is_empty() {
            // Look for similar method names on this type
            let type_methods: Vec<&PublicApi> = crate_info
                .public_apis
                .iter()
                .filter(|api| {
                    api.item_type == ApiItemType::Method
                        && api.path.contains(&format!("{}::", type_name))
                })
                .collect();

            let suggestions: Vec<String> = type_methods
                .iter()
                .map(|api| api.path.split("::").last().unwrap_or(&api.path).to_string())
                .collect();

            Ok(ValidationResult {
                success: false,
                errors: vec![format!(
                    "Method '{}' not found on type '{}'",
                    method_name, type_ref.item_path
                )],
                warnings: vec![],
                suggestions: if suggestions.is_empty() {
                    vec!["No methods found on this type".to_string()]
                } else {
                    vec![format!("Available methods: {}", suggestions.join(", "))]
                },
                found_api: None,
            })
        } else {
            Ok(ValidationResult {
                success: true,
                errors: vec![],
                warnings: vec![],
                suggestions: vec![],
                found_api: Some(matching_methods[0].clone()),
            })
        }
    }

    /// Find all Cargo.toml files in the workspace.
    fn find_cargo_files(&self) -> Result<Vec<PathBuf>> {
        let mut cargo_files = Vec::new();

        for entry in
            WalkDir::new(&self.workspace_path).follow_links(true).into_iter().filter_map(|e| e.ok())
        {
            if entry.file_name() == "Cargo.toml" {
                // Skip target directories and other build artifacts
                let path_str = entry.path().to_string_lossy();
                if !path_str.contains("/target/") && !path_str.contains("\\target\\") {
                    cargo_files.push(entry.path().to_path_buf());
                }
            }
        }

        Ok(cargo_files)
    }

    /// Analyze a single crate from its Cargo.toml file.
    #[instrument(skip(self))]
    async fn analyze_crate(&self, cargo_path: &Path) -> Result<Option<CrateInfo>> {
        debug!("Analyzing crate at: {}", cargo_path.display());

        // Parse Cargo.toml
        let cargo_content = std::fs::read_to_string(cargo_path).map_err(|e| {
            AuditError::IoError { path: cargo_path.to_path_buf(), details: e.to_string() }
        })?;

        let cargo_toml: toml::Value = toml::from_str(&cargo_content).map_err(|e| {
            AuditError::TomlError { file_path: cargo_path.to_path_buf(), details: e.to_string() }
        })?;

        // Extract package information
        let package = match cargo_toml.get("package") {
            Some(pkg) => pkg,
            None => {
                // This might be a workspace root Cargo.toml without a package
                debug!("No package section found in {}, skipping", cargo_path.display());
                return Ok(None);
            }
        };

        let name = package
            .get("name")
            .and_then(|n| n.as_str())
            .ok_or_else(|| AuditError::TomlError {
                file_path: cargo_path.to_path_buf(),
                details: "Missing package name".to_string(),
            })?
            .to_string();

        let version =
            package.get("version").and_then(|v| v.as_str()).unwrap_or("0.0.0").to_string();

        let rust_version =
            package.get("rust-version").and_then(|v| v.as_str()).map(|s| s.to_string());

        // Extract feature flags
        let feature_flags = self.extract_feature_flags(&cargo_toml);

        // Extract dependencies
        let dependencies = self.extract_dependencies(&cargo_toml);

        // Find and analyze source files
        let crate_dir = cargo_path.parent().unwrap();
        let src_dir = crate_dir.join("src");

        let public_apis = if src_dir.exists() {
            self.analyze_source_files(&src_dir, &name).await?
        } else {
            warn!("No src directory found for crate: {}", name);
            Vec::new()
        };

        Ok(Some(CrateInfo {
            name,
            version,
            path: crate_dir.to_path_buf(),
            public_apis,
            feature_flags,
            dependencies,
            rust_version,
        }))
    }

    /// Extract feature flags from Cargo.toml.
    fn extract_feature_flags(&self, cargo_toml: &toml::Value) -> Vec<String> {
        cargo_toml
            .get("features")
            .and_then(|f| f.as_table())
            .map(|table| table.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Extract dependencies from Cargo.toml.
    fn extract_dependencies(&self, cargo_toml: &toml::Value) -> Vec<Dependency> {
        let mut dependencies = Vec::new();

        // Process regular dependencies
        if let Some(deps) = cargo_toml.get("dependencies").and_then(|d| d.as_table()) {
            for (name, spec) in deps {
                dependencies.push(self.parse_dependency(name, spec));
            }
        }

        // Process dev-dependencies
        if let Some(deps) = cargo_toml.get("dev-dependencies").and_then(|d| d.as_table()) {
            for (name, spec) in deps {
                dependencies.push(self.parse_dependency(name, spec));
            }
        }

        dependencies
    }

    /// Parse a single dependency specification.
    fn parse_dependency(&self, name: &str, spec: &toml::Value) -> Dependency {
        match spec {
            toml::Value::String(version) => Dependency {
                name: name.to_string(),
                version: version.clone(),
                optional: false,
                features: Vec::new(),
            },
            toml::Value::Table(table) => {
                let version =
                    table.get("version").and_then(|v| v.as_str()).unwrap_or("*").to_string();

                let optional = table.get("optional").and_then(|o| o.as_bool()).unwrap_or(false);

                let features = table
                    .get("features")
                    .and_then(|f| f.as_array())
                    .map(|arr| {
                        arr.iter().filter_map(|v| v.as_str()).map(|s| s.to_string()).collect()
                    })
                    .unwrap_or_default();

                Dependency { name: name.to_string(), version, optional, features }
            }
            _ => Dependency {
                name: name.to_string(),
                version: "*".to_string(),
                optional: false,
                features: Vec::new(),
            },
        }
    }

    /// Analyze all Rust source files in a directory.
    #[instrument(skip(self))]
    async fn analyze_source_files(
        &self,
        src_dir: &Path,
        crate_name: &str,
    ) -> Result<Vec<PublicApi>> {
        let mut apis = Vec::new();

        for entry in WalkDir::new(src_dir).follow_links(true).into_iter().filter_map(|e| e.ok()) {
            if entry.path().extension().and_then(|s| s.to_str()) == Some("rs") {
                let file_apis = self.analyze_rust_file(entry.path(), crate_name).await?;
                apis.extend(file_apis);
            }
        }

        Ok(apis)
    }

    /// Analyze a single Rust source file.
    #[instrument(skip(self))]
    async fn analyze_rust_file(
        &self,
        file_path: &Path,
        crate_name: &str,
    ) -> Result<Vec<PublicApi>> {
        debug!("Analyzing Rust file: {}", file_path.display());

        let content = std::fs::read_to_string(file_path).map_err(|e| AuditError::IoError {
            path: file_path.to_path_buf(),
            details: e.to_string(),
        })?;

        let syntax_tree = syn::parse_file(&content).map_err(|e| AuditError::SyntaxError {
            details: format!("Failed to parse {}: {}", file_path.display(), e),
        })?;

        let mut apis = Vec::new();
        let mut current_module_path = vec![crate_name.to_string()];

        self.extract_apis_from_items(
            &syntax_tree.items,
            &mut current_module_path,
            file_path,
            &mut apis,
        );

        Ok(apis)
    }

    /// Extract public APIs from a list of syntax tree items.
    fn extract_apis_from_items(
        &self,
        items: &[Item],
        module_path: &mut Vec<String>,
        file_path: &Path,
        apis: &mut Vec<PublicApi>,
    ) {
        for item in items {
            match item {
                Item::Fn(item_fn) => {
                    if self.is_public(&item_fn.vis) {
                        let api = self.create_function_api(item_fn, module_path, file_path);
                        apis.push(api);
                    }
                }
                Item::Struct(item_struct) => {
                    if self.is_public(&item_struct.vis) {
                        let api = self.create_struct_api(item_struct, module_path, file_path);
                        apis.push(api);
                    }
                }
                Item::Enum(item_enum) => {
                    if self.is_public(&item_enum.vis) {
                        let api = self.create_enum_api(item_enum, module_path, file_path);
                        apis.push(api);
                    }
                }
                Item::Trait(item_trait) => {
                    if self.is_public(&item_trait.vis) {
                        let api = self.create_trait_api(item_trait, module_path, file_path);
                        apis.push(api);
                    }
                }
                Item::Type(item_type) => {
                    if self.is_public(&item_type.vis) {
                        let api = self.create_type_api(item_type, module_path, file_path);
                        apis.push(api);
                    }
                }
                Item::Const(item_const) => {
                    if self.is_public(&item_const.vis) {
                        let api = self.create_const_api(item_const, module_path, file_path);
                        apis.push(api);
                    }
                }
                Item::Static(item_static) => {
                    if self.is_public(&item_static.vis) {
                        let api = self.create_static_api(item_static, module_path, file_path);
                        apis.push(api);
                    }
                }
                Item::Mod(item_mod) => {
                    if self.is_public(&item_mod.vis) {
                        // Recursively analyze module contents
                        module_path.push(item_mod.ident.to_string());
                        if let Some((_, items)) = &item_mod.content {
                            self.extract_apis_from_items(items, module_path, file_path, apis);
                        }
                        module_path.pop();
                    }
                }
                Item::Impl(item_impl) => {
                    // Extract methods from impl blocks
                    self.extract_impl_methods(item_impl, module_path, file_path, apis);
                }
                _ => {
                    // Other items (use, extern crate, etc.) are not public APIs
                }
            }
        }
    }

    /// Check if a visibility modifier indicates a public item.
    fn is_public(&self, vis: &Visibility) -> bool {
        matches!(vis, Visibility::Public(_))
    }

    /// Create a PublicApi for a function.
    fn create_function_api(
        &self,
        item_fn: &ItemFn,
        module_path: &[String],
        file_path: &Path,
    ) -> PublicApi {
        let path = format!("{}::{}", module_path.join("::"), item_fn.sig.ident);
        let signature = format!("fn {}", quote::quote!(#item_fn.sig));
        let documentation = self.extract_doc_comments(&item_fn.attrs);
        let deprecated = self.is_deprecated(&item_fn.attrs);

        PublicApi {
            path,
            signature,
            item_type: ApiItemType::Function,
            documentation,
            deprecated,
            source_file: file_path.to_path_buf(),
            line_number: item_fn.span().start().line,
        }
    }

    /// Create a PublicApi for a struct.
    fn create_struct_api(
        &self,
        item_struct: &ItemStruct,
        module_path: &[String],
        file_path: &Path,
    ) -> PublicApi {
        let path = format!("{}::{}", module_path.join("::"), item_struct.ident);
        let signature = format!("struct {}", quote::quote!(#item_struct));
        let documentation = self.extract_doc_comments(&item_struct.attrs);
        let deprecated = self.is_deprecated(&item_struct.attrs);

        PublicApi {
            path,
            signature,
            item_type: ApiItemType::Struct,
            documentation,
            deprecated,
            source_file: file_path.to_path_buf(),
            line_number: item_struct.span().start().line,
        }
    }

    /// Create a PublicApi for an enum.
    fn create_enum_api(
        &self,
        item_enum: &ItemEnum,
        module_path: &[String],
        file_path: &Path,
    ) -> PublicApi {
        let path = format!("{}::{}", module_path.join("::"), item_enum.ident);
        let signature = format!("enum {}", quote::quote!(#item_enum));
        let documentation = self.extract_doc_comments(&item_enum.attrs);
        let deprecated = self.is_deprecated(&item_enum.attrs);

        PublicApi {
            path,
            signature,
            item_type: ApiItemType::Enum,
            documentation,
            deprecated,
            source_file: file_path.to_path_buf(),
            line_number: item_enum.span().start().line,
        }
    }

    /// Create a PublicApi for a trait.
    fn create_trait_api(
        &self,
        item_trait: &ItemTrait,
        module_path: &[String],
        file_path: &Path,
    ) -> PublicApi {
        let path = format!("{}::{}", module_path.join("::"), item_trait.ident);
        let signature = format!("trait {}", quote::quote!(#item_trait));
        let documentation = self.extract_doc_comments(&item_trait.attrs);
        let deprecated = self.is_deprecated(&item_trait.attrs);

        PublicApi {
            path,
            signature,
            item_type: ApiItemType::Trait,
            documentation,
            deprecated,
            source_file: file_path.to_path_buf(),
            line_number: item_trait.span().start().line,
        }
    }

    /// Create a PublicApi for a type alias.
    fn create_type_api(
        &self,
        item_type: &ItemType,
        module_path: &[String],
        file_path: &Path,
    ) -> PublicApi {
        let path = format!("{}::{}", module_path.join("::"), item_type.ident);
        let signature = format!("type {}", quote::quote!(#item_type));
        let documentation = self.extract_doc_comments(&item_type.attrs);
        let deprecated = self.is_deprecated(&item_type.attrs);

        PublicApi {
            path,
            signature,
            item_type: ApiItemType::Struct, // Type aliases are categorized as structs for simplicity
            documentation,
            deprecated,
            source_file: file_path.to_path_buf(),
            line_number: item_type.span().start().line,
        }
    }

    /// Create a PublicApi for a constant.
    fn create_const_api(
        &self,
        item_const: &ItemConst,
        module_path: &[String],
        file_path: &Path,
    ) -> PublicApi {
        let path = format!("{}::{}", module_path.join("::"), item_const.ident);
        let signature = format!("const {}", quote::quote!(#item_const));
        let documentation = self.extract_doc_comments(&item_const.attrs);
        let deprecated = self.is_deprecated(&item_const.attrs);

        PublicApi {
            path,
            signature,
            item_type: ApiItemType::Constant,
            documentation,
            deprecated,
            source_file: file_path.to_path_buf(),
            line_number: item_const.span().start().line,
        }
    }

    /// Create a PublicApi for a static.
    fn create_static_api(
        &self,
        item_static: &ItemStatic,
        module_path: &[String],
        file_path: &Path,
    ) -> PublicApi {
        let path = format!("{}::{}", module_path.join("::"), item_static.ident);
        let signature = format!("static {}", quote::quote!(#item_static));
        let documentation = self.extract_doc_comments(&item_static.attrs);
        let deprecated = self.is_deprecated(&item_static.attrs);

        PublicApi {
            path,
            signature,
            item_type: ApiItemType::Constant, // Statics are categorized as constants for simplicity
            documentation,
            deprecated,
            source_file: file_path.to_path_buf(),
            line_number: item_static.span().start().line,
        }
    }

    /// Extract methods from impl blocks.
    fn extract_impl_methods(
        &self,
        item_impl: &ItemImpl,
        module_path: &[String],
        file_path: &Path,
        apis: &mut Vec<PublicApi>,
    ) {
        // Get the type being implemented
        let type_name = match &*item_impl.self_ty {
            syn::Type::Path(type_path) => type_path
                .path
                .segments
                .last()
                .map(|seg| seg.ident.to_string())
                .unwrap_or_else(|| "Unknown".to_string()),
            _ => "Unknown".to_string(),
        };

        for item in &item_impl.items {
            if let syn::ImplItem::Fn(method) = item {
                if self.is_public(&method.vis) {
                    let path =
                        format!("{}::{}::{}", module_path.join("::"), type_name, method.sig.ident);
                    let signature = format!("fn {}", quote::quote!(#method.sig));
                    let documentation = self.extract_doc_comments(&method.attrs);
                    let deprecated = self.is_deprecated(&method.attrs);

                    apis.push(PublicApi {
                        path,
                        signature,
                        item_type: ApiItemType::Method,
                        documentation,
                        deprecated,
                        source_file: file_path.to_path_buf(),
                        line_number: method.span().start().line,
                    });
                }
            }
        }
    }

    /// Extract documentation comments from attributes.
    fn extract_doc_comments(&self, attrs: &[Attribute]) -> Option<String> {
        let mut doc_lines = Vec::new();

        for attr in attrs {
            if attr.path().is_ident("doc") {
                if let Meta::NameValue(meta) = &attr.meta {
                    if let Expr::Lit(expr_lit) = &meta.value {
                        if let Lit::Str(lit_str) = &expr_lit.lit {
                            doc_lines.push(lit_str.value());
                        }
                    }
                }
            }
        }

        if doc_lines.is_empty() { None } else { Some(doc_lines.join("\n")) }
    }

    /// Check if an item is marked as deprecated.
    fn is_deprecated(&self, attrs: &[Attribute]) -> bool {
        attrs.iter().any(|attr| attr.path().is_ident("deprecated"))
    }

    /// Suggest similar crate names when a crate is not found.
    fn suggest_similar_crate_names(target: &str, registry: &CrateRegistry) -> String {
        Self::suggest_similar_crate_names_static(
            target,
            &registry.crates.keys().cloned().collect::<Vec<_>>(),
        )
    }

    /// Static version of suggest_similar_crate_names to avoid borrow checker issues.
    fn suggest_similar_crate_names_static(target: &str, available_crates: &[String]) -> String {
        let mut suggestions = Vec::new();

        for crate_name in available_crates {
            if crate_name.contains(target) || target.contains(crate_name) {
                suggestions.push(crate_name.clone());
            }
        }

        if suggestions.is_empty() {
            format!("Available crates: {}", available_crates.join(", "))
        } else {
            format!("Did you mean: {}?", suggestions.join(", "))
        }
    }

    /// Suggest similar API names when an API is not found.
    fn suggest_similar_api_names(target: &str, crate_info: &CrateInfo) -> String {
        Self::suggest_similar_api_names_static(target, &crate_info.public_apis)
    }

    /// Static version of suggest_similar_api_names to avoid borrow checker issues.
    fn suggest_similar_api_names_static(target: &str, public_apis: &[PublicApi]) -> String {
        let mut suggestions = Vec::new();

        for api in public_apis {
            let api_name = api.path.split("::").last().unwrap_or(&api.path);
            if api_name.contains(target) || target.contains(api_name) {
                suggestions.push(api.path.clone());
            }
        }

        if suggestions.is_empty() {
            "No similar APIs found".to_string()
        } else {
            format!("Did you mean: {}?", suggestions.join(", "))
        }
    }

    /// Normalize a function signature for comparison by removing whitespace and formatting.
    fn normalize_signature(&self, signature: &str) -> String {
        signature.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_lowercase()
    }

    /// Extract field names from a struct signature.
    fn extract_struct_fields(&self, signature: &str) -> Vec<String> {
        // This is a simplified implementation
        // In a real implementation, you'd want to parse the struct more carefully
        let mut fields = Vec::new();

        // Look for patterns like "field_name: Type" in the signature
        if let Some(start) = signature.find('{') {
            if let Some(end) = signature.rfind('}') {
                let fields_section = &signature[start + 1..end];
                for line in fields_section.lines() {
                    let trimmed = line.trim();
                    if let Some(colon_pos) = trimmed.find(':') {
                        let field_name = trimmed[..colon_pos].trim();
                        if !field_name.is_empty()
                            && field_name.chars().all(|c| c.is_alphanumeric() || c == '_')
                        {
                            fields.push(field_name.to_string());
                        }
                    }
                }
            }
        }

        fields
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_analyzer_creation() {
        let temp_dir = TempDir::new().unwrap();
        let analyzer = CodeAnalyzer::new(temp_dir.path().to_path_buf());
        assert_eq!(analyzer.workspace_path, temp_dir.path());
    }

    #[tokio::test]
    async fn test_find_cargo_files() {
        let temp_dir = TempDir::new().unwrap();

        // Create a test workspace structure
        let crate1_dir = temp_dir.path().join("crate1");
        fs::create_dir_all(&crate1_dir).unwrap();
        fs::write(
            crate1_dir.join("Cargo.toml"),
            r#"
[package]
name = "crate1"
version = "0.1.0"
"#,
        )
        .unwrap();

        let crate2_dir = temp_dir.path().join("crate2");
        fs::create_dir_all(&crate2_dir).unwrap();
        fs::write(
            crate2_dir.join("Cargo.toml"),
            r#"
[package]
name = "crate2"
version = "0.1.0"
"#,
        )
        .unwrap();

        let analyzer = CodeAnalyzer::new(temp_dir.path().to_path_buf());
        let cargo_files = analyzer.find_cargo_files().unwrap();

        assert_eq!(cargo_files.len(), 2);
        assert!(cargo_files.iter().any(|p| p.ends_with("crate1/Cargo.toml")));
        assert!(cargo_files.iter().any(|p| p.ends_with("crate2/Cargo.toml")));
    }

    #[test]
    fn test_extract_feature_flags() {
        let analyzer = CodeAnalyzer::new(PathBuf::from("."));

        let cargo_toml: toml::Value = toml::from_str(
            r#"
[features]
default = []
feature1 = []
feature2 = ["dep1"]
"#,
        )
        .unwrap();

        let flags = analyzer.extract_feature_flags(&cargo_toml);
        assert_eq!(flags.len(), 3);
        assert!(flags.contains(&"default".to_string()));
        assert!(flags.contains(&"feature1".to_string()));
        assert!(flags.contains(&"feature2".to_string()));
    }

    #[test]
    fn test_parse_dependency() {
        let analyzer = CodeAnalyzer::new(PathBuf::from("."));

        // Test string version
        let dep1 = analyzer.parse_dependency("serde", &toml::Value::String("1.0".to_string()));
        assert_eq!(dep1.name, "serde");
        assert_eq!(dep1.version, "1.0");
        assert!(!dep1.optional);

        // Test table version
        let table: toml::Value = toml::from_str(
            r#"
version = "1.0"
optional = true
features = ["derive"]
"#,
        )
        .unwrap();

        let dep2 = analyzer.parse_dependency("serde", &table);
        assert_eq!(dep2.name, "serde");
        assert_eq!(dep2.version, "1.0");
        assert!(dep2.optional);
        assert_eq!(dep2.features, vec!["derive"]);
    }
}
