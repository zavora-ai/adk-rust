//! Configuration types for mistral.rs model loading.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::mcp::McpClientConfig;

/// Configuration for mistral.rs model loading.
#[derive(Debug, Clone)]
pub struct MistralRsConfig {
    /// Model source: HuggingFace ID, local path, GGUF, or UQFF path
    pub model_source: ModelSource,

    /// Model architecture type
    pub architecture: ModelArchitecture,

    /// Data type for model weights
    pub dtype: DataType,

    /// Device configuration
    pub device: DeviceConfig,

    /// ISQ quantization settings (optional)
    pub isq: Option<IsqConfig>,

    /// LoRA/X-LoRA adapter configuration (optional)
    pub adapter: Option<AdapterConfig>,

    /// Generation defaults - temperature
    pub temperature: Option<f32>,

    /// Generation defaults - top_p
    pub top_p: Option<f32>,

    /// Generation defaults - top_k
    pub top_k: Option<i32>,

    /// Generation defaults - max_tokens
    pub max_tokens: Option<i32>,

    /// Context length
    pub num_ctx: Option<usize>,

    /// Enable PagedAttention
    pub paged_attention: bool,

    /// Topology file for per-layer config (optional)
    pub topology_path: Option<PathBuf>,

    /// Custom chat template (optional)
    pub chat_template: Option<String>,

    /// Custom tokenizer path (optional)
    pub tokenizer_path: Option<PathBuf>,

    /// MatFormer configuration for Gemma 3n (optional)
    pub matformer: Option<MatFormerConfig>,

    /// MCP client configuration file path (optional, deprecated - use mcp_client instead)
    pub mcp_config: Option<PathBuf>,

    /// MCP client configuration for external tools (optional)
    pub mcp_client: Option<McpClientConfig>,
}

impl Default for MistralRsConfig {
    fn default() -> Self {
        Self {
            model_source: ModelSource::HuggingFace(String::new()),
            architecture: ModelArchitecture::default(),
            dtype: DataType::default(),
            device: DeviceConfig::default(),
            isq: None,
            adapter: None,
            temperature: None,
            top_p: None,
            top_k: None,
            max_tokens: None,
            num_ctx: None,
            paged_attention: false,
            topology_path: None,
            chat_template: None,
            tokenizer_path: None,
            matformer: None,
            mcp_config: None,
            mcp_client: None,
        }
    }
}

impl MistralRsConfig {
    /// Create a new config builder
    pub fn builder() -> MistralRsConfigBuilder {
        MistralRsConfigBuilder::default()
    }
}

/// Builder for MistralRsConfig
#[derive(Debug, Clone, Default)]
pub struct MistralRsConfigBuilder {
    config: MistralRsConfig,
}

impl MistralRsConfigBuilder {
    /// Set the model source
    pub fn model_source(mut self, source: ModelSource) -> Self {
        self.config.model_source = source;
        self
    }

    /// Set the model architecture
    pub fn architecture(mut self, arch: ModelArchitecture) -> Self {
        self.config.architecture = arch;
        self
    }

    /// Set the data type
    pub fn dtype(mut self, dtype: DataType) -> Self {
        self.config.dtype = dtype;
        self
    }

    /// Set the device configuration
    pub fn device(mut self, device: DeviceConfig) -> Self {
        self.config.device = device;
        self
    }

    /// Enable ISQ quantization
    pub fn isq(mut self, level: QuantizationLevel) -> Self {
        self.config.isq = Some(IsqConfig { level, layer_overrides: None });
        self
    }

    /// Set ISQ configuration with layer overrides
    pub fn isq_config(mut self, config: IsqConfig) -> Self {
        self.config.isq = Some(config);
        self
    }

    /// Set adapter configuration
    pub fn adapter(mut self, adapter: AdapterConfig) -> Self {
        self.config.adapter = Some(adapter);
        self
    }

    /// Set temperature
    pub fn temperature(mut self, temp: f32) -> Self {
        self.config.temperature = Some(temp);
        self
    }

    /// Set top_p
    pub fn top_p(mut self, top_p: f32) -> Self {
        self.config.top_p = Some(top_p);
        self
    }

    /// Set top_k
    pub fn top_k(mut self, top_k: i32) -> Self {
        self.config.top_k = Some(top_k);
        self
    }

    /// Set max_tokens
    pub fn max_tokens(mut self, max_tokens: i32) -> Self {
        self.config.max_tokens = Some(max_tokens);
        self
    }

    /// Set context length
    pub fn num_ctx(mut self, num_ctx: usize) -> Self {
        self.config.num_ctx = Some(num_ctx);
        self
    }

    /// Enable PagedAttention
    pub fn paged_attention(mut self, enabled: bool) -> Self {
        self.config.paged_attention = enabled;
        self
    }

    /// Set topology file path
    pub fn topology_path(mut self, path: PathBuf) -> Self {
        self.config.topology_path = Some(path);
        self
    }

    /// Set custom chat template
    pub fn chat_template(mut self, template: String) -> Self {
        self.config.chat_template = Some(template);
        self
    }

    /// Set custom tokenizer path
    pub fn tokenizer_path(mut self, path: PathBuf) -> Self {
        self.config.tokenizer_path = Some(path);
        self
    }

    /// Set MatFormer configuration
    pub fn matformer(mut self, config: MatFormerConfig) -> Self {
        self.config.matformer = Some(config);
        self
    }

    /// Set MCP client configuration path
    pub fn mcp_config(mut self, path: PathBuf) -> Self {
        self.config.mcp_config = Some(path);
        self
    }

    /// Set MCP client configuration directly
    ///
    /// This allows configuring MCP servers programmatically without a config file.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_mistralrs::{MistralRsConfig, McpClientConfig, McpServerConfig};
    ///
    /// let config = MistralRsConfig::builder()
    ///     .model_source(ModelSource::huggingface("microsoft/Phi-3.5-mini-instruct"))
    ///     .mcp_client(McpClientConfig::with_server(
    ///         McpServerConfig::process("Filesystem", "mcp-server-filesystem")
    ///             .with_args(vec!["--root".to_string(), "/tmp".to_string()])
    ///     ))
    ///     .build();
    /// ```
    pub fn mcp_client(mut self, config: McpClientConfig) -> Self {
        self.config.mcp_client = Some(config);
        self
    }

    /// Build the configuration
    pub fn build(self) -> MistralRsConfig {
        self.config
    }
}

/// Model source specification
#[derive(Debug, Clone)]
pub enum ModelSource {
    /// HuggingFace Hub model ID (e.g., "mistralai/Mistral-7B-v0.1")
    HuggingFace(String),
    /// Local directory path
    Local(PathBuf),
    /// GGUF file path
    Gguf(PathBuf),
    /// UQFF pre-quantized file path
    Uqff(PathBuf),
}

impl ModelSource {
    /// Create a HuggingFace model source
    pub fn huggingface(model_id: impl Into<String>) -> Self {
        Self::HuggingFace(model_id))
    }

    /// Create a local path model source
    pub fn local(path: impl Into<PathBuf>) -> Self {
        Self::Local(path))
    }

    /// Create a GGUF file model source
    pub fn gguf(path: impl Into<PathBuf>) -> Self {
        Self::Gguf(path))
    }

    /// Create a UQFF file model source
    pub fn uqff(path: impl Into<PathBuf>) -> Self {
        Self::Uqff(path))
    }
}

/// Model architecture type
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ModelArchitecture {
    /// Plain text model
    #[default]
    Plain,
    /// Vision-language model
    Vision,
    /// Diffusion model for image generation
    Diffusion,
    /// Speech generation model
    Speech,
    /// Embedding model
    Embedding,
    /// X-LoRA adapter model
    XLora,
    /// LoRA adapter model
    Lora,
}

/// Data type for model weights
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DataType {
    /// 32-bit floating point
    F32,
    /// 16-bit floating point
    F16,
    /// Brain floating point 16
    BF16,
    /// Auto-detect based on model and hardware
    #[default]
    Auto,
}

/// Device configuration
#[derive(Debug, Clone, Default)]
pub struct DeviceConfig {
    /// Primary device
    pub device: Device,
    /// Device mapping for multi-device (layer name -> device)
    pub device_map: Option<HashMap<String, Device>>,
    /// Layer range mapping for multi-device (start_layer, end_layer, device)
    pub layer_ranges: Option<Vec<LayerDeviceRange>>,
}

/// Layer range to device mapping for multi-device model splitting.
#[derive(Debug, Clone)]
pub struct LayerDeviceRange {
    /// Starting layer index (inclusive)
    pub start_layer: usize,
    /// Ending layer index (exclusive)
    pub end_layer: usize,
    /// Device to place these layers on
    pub device: Device,
}

impl LayerDeviceRange {
    /// Create a new layer range mapping.
    pub fn new(start_layer: usize, end_layer: usize, device: Device) -> Self {
        Self { start_layer, end_layer, device }
    }
}

impl DeviceConfig {
    /// Create a new device config with the specified device
    pub fn new(device: Device) -> Self {
        Self { device, device_map: None, layer_ranges: None }
    }

    /// Create a device config with device mapping
    pub fn with_map(device: Device, device_map: HashMap<String, Device>) -> Self {
        Self { device, device_map: Some(device_map), layer_ranges: None }
    }

    /// Create a device config for multi-GPU model splitting.
    ///
    /// This splits the model across multiple CUDA GPUs based on layer ranges.
    ///
    /// # Arguments
    ///
    /// * `layer_ranges` - List of (start_layer, end_layer, gpu_index) tuples
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_mistralrs::{DeviceConfig, Device, LayerDeviceRange};
    ///
    /// // Split a 32-layer model across 2 GPUs
    /// let config = DeviceConfig::multi_gpu(vec![
    ///     LayerDeviceRange::new(0, 16, Device::Cuda(0)),
    ///     LayerDeviceRange::new(16, 32, Device::Cuda(1)),
    /// ]);
    /// ```
    pub fn multi_gpu(layer_ranges: Vec<LayerDeviceRange>) -> Self {
        Self { device: Device::Auto, device_map: None, layer_ranges: Some(layer_ranges) }
    }

    /// Add a layer range to the device config.
    pub fn with_layer_range(mut self, start: usize, end: usize, device: Device) -> Self {
        let range = LayerDeviceRange::new(start, end, device);
        match &mut self.layer_ranges {
            Some(ranges) => ranges.push(range),
            None => self.layer_ranges = Some(vec![range]),
        }
        self
    }

    /// Check if this config uses multi-device splitting.
    pub fn is_multi_device(&self) -> bool {
        self.device_map.is_some() || self.layer_ranges.is_some()
    }

    /// Get the number of unique devices used.
    pub fn device_count(&self) -> usize {
        let mut devices = std::collections::HashSet::new();
        devices.insert(format!("{:?}", self.device));

        if let Some(map) = &self.device_map {
            for device in map.values() {
                devices.insert(format!("{:?}", device));
            }
        }

        if let Some(ranges) = &self.layer_ranges {
            for range in ranges {
                devices.insert(format!("{:?}", range.device));
            }
        }

        devices.len()
    }
}

/// Device selection
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Device {
    /// Auto-detect best available device
    #[default]
    Auto,
    /// CPU
    Cpu,
    /// CUDA GPU with index
    Cuda(usize),
    /// Apple Metal
    Metal,
}

/// ISQ (In-Situ Quantization) configuration
#[derive(Debug, Clone)]
pub struct IsqConfig {
    /// Quantization level
    pub level: QuantizationLevel,
    /// Per-layer overrides (optional)
    pub layer_overrides: Option<HashMap<String, QuantizationLevel>>,
}

impl IsqConfig {
    /// Create a new ISQ config with the specified level
    pub fn new(level: QuantizationLevel) -> Self {
        Self { level, layer_overrides: None }
    }

    /// Create an ISQ config with layer overrides
    pub fn with_overrides(
        level: QuantizationLevel,
        overrides: HashMap<String, QuantizationLevel>,
    ) -> Self {
        Self { level, layer_overrides: Some(overrides) }
    }
}

/// Quantization level for ISQ
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuantizationLevel {
    /// 4-bit quantization (variant 0)
    Q4_0,
    /// 4-bit quantization (variant 1)
    Q4_1,
    /// 5-bit quantization (variant 0)
    Q5_0,
    /// 5-bit quantization (variant 1)
    Q5_1,
    /// 8-bit quantization (variant 0)
    Q8_0,
    /// 8-bit quantization (variant 1)
    Q8_1,
    /// 2-bit K-quant
    Q2K,
    /// 3-bit K-quant
    Q3K,
    /// 4-bit K-quant
    Q4K,
    /// 5-bit K-quant
    Q5K,
    /// 6-bit K-quant
    Q6K,
}

/// LoRA/X-LoRA adapter configuration
#[derive(Debug, Clone)]
pub struct AdapterConfig {
    /// Adapter type
    pub adapter_type: AdapterType,
    /// Primary adapter source (HuggingFace ID or local path)
    pub adapter_source: String,
    /// Additional adapter IDs for multi-adapter LoRA
    pub additional_adapters: Vec<String>,
    /// Adapter ordering file (for X-LoRA)
    pub ordering: Option<PathBuf>,
    /// Target non-granular index for X-LoRA (optional)
    pub tgt_non_granular_index: Option<usize>,
}

impl AdapterConfig {
    /// Create a LoRA adapter config with a single adapter
    ///
    /// # Arguments
    ///
    /// * `source` - HuggingFace adapter ID or local path to adapter weights
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_mistralrs::AdapterConfig;
    ///
    /// let config = AdapterConfig::lora("username/my-lora-adapter");
    /// ```
    pub fn lora(source: impl Into<String>) -> Self {
        Self {
            adapter_type: AdapterType::LoRA,
            adapter_source: source.into(),
            additional_adapters: Vec::new(),
            ordering: None,
            tgt_non_granular_index: None,
        }
    }

    /// Create a LoRA adapter config with multiple adapters
    ///
    /// # Arguments
    ///
    /// * `adapters` - Iterator of HuggingFace adapter IDs or local paths
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_mistralrs::AdapterConfig;
    ///
    /// let config = AdapterConfig::lora_multi(vec![
    ///     "username/adapter1",
    ///     "username/adapter2",
    /// ]);
    /// ```
    pub fn lora_multi(adapters: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let mut adapters: Vec<String> = adapters.into_iter().map(|s| s)).collect();
        let primary = adapters.remove(0);
        Self {
            adapter_type: AdapterType::LoRA,
            adapter_source: primary,
            additional_adapters: adapters,
            ordering: None,
            tgt_non_granular_index: None,
        }
    }

    /// Create an X-LoRA adapter config with dynamic adapter mixing
    ///
    /// # Arguments
    ///
    /// * `source` - HuggingFace X-LoRA model ID or local path
    /// * `ordering` - Path to the ordering JSON file that specifies adapter configuration
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_mistralrs::AdapterConfig;
    /// use std::path::PathBuf;
    ///
    /// let config = AdapterConfig::xlora(
    ///     "username/my-xlora-model",
    ///     PathBuf::from("ordering.json")
    /// );
    /// ```
    pub fn xlora(source: impl Into<String>, ordering: PathBuf) -> Self {
        Self {
            adapter_type: AdapterType::XLoRA,
            adapter_source: source.into(),
            additional_adapters: Vec::new(),
            ordering: Some(ordering),
            tgt_non_granular_index: None,
        }
    }

    /// Set the target non-granular index for X-LoRA
    ///
    /// This is used for X-LoRA models to specify which adapter to use
    /// for non-granular (global) scaling.
    pub fn with_tgt_non_granular_index(mut self, index: usize) -> Self {
        self.tgt_non_granular_index = Some(index);
        self
    }

    /// Add additional adapters for multi-adapter LoRA
    pub fn with_additional_adapters(
        mut self,
        adapters: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.additional_adapters.extend(adapters.into_iter().map(|s| s)));
        self
    }

    /// Get all adapter IDs (primary + additional)
    pub fn all_adapter_ids(&self) -> Vec<String> {
        let mut ids = vec![self.adapter_source.clone()];
        ids.extend(self.additional_adapters.clone());
        ids
    }

    /// Check if this is a multi-adapter configuration
    pub fn is_multi_adapter(&self) -> bool {
        !self.additional_adapters.is_empty()
    }
}

/// Adapter type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterType {
    /// Standard LoRA adapter
    LoRA,
    /// X-LoRA with dynamic adapter mixing
    XLoRA,
}

impl std::fmt::Display for AdapterType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdapterType::LoRA => write!(f, "LoRA"),
            AdapterType::XLoRA => write!(f, "X-LoRA"),
        }
    }
}

/// MatFormer configuration for Gemma 3n models.
///
/// MatFormer (Matryoshka Transformer) allows creating smaller model variants
/// from a larger model by selecting specific "slices" of the model.
///
/// # Example
///
/// ```rust
/// use adk_mistralrs::MatFormerConfig;
///
/// // Simple configuration with just target size
/// let config = MatFormerConfig::new("2b");
///
/// // Configuration with custom config file
/// let config = MatFormerConfig::with_config_path(
///     "4b",
///     "/path/to/matformer_config.csv"
/// );
/// ```
#[derive(Debug, Clone)]
pub struct MatFormerConfig {
    /// Target model size/slice name (e.g., "2b", "4b", "E2B", "E4B")
    pub target_size: String,
    /// Optional path to MatFormer configuration CSV file
    pub config_path: Option<PathBuf>,
}

impl MatFormerConfig {
    /// Create a new MatFormer config with target size.
    ///
    /// # Arguments
    ///
    /// * `target_size` - The target model size or slice name (e.g., "2b", "4b")
    pub fn new(target_size: impl Into<String>) -> Self {
        Self { target_size: target_size.into(), config_path: None }
    }

    /// Create a MatFormer config with a custom configuration file.
    ///
    /// # Arguments
    ///
    /// * `target_size` - The target model size or slice name
    /// * `config_path` - Path to the MatFormer configuration CSV file
    pub fn with_config_path(
        target_size: impl Into<String>,
        config_path: impl Into<PathBuf>,
    ) -> Self {
        Self { target_size: target_size.into(), config_path: Some(config_path)) }
    }

    /// Set the configuration file path.
    pub fn config_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.config_path = Some(path.into());        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_builder() {
        let config = MistralRsConfig::builder()
            .model_source(ModelSource::huggingface("test/model"))
            .architecture(ModelArchitecture::Plain)
            .dtype(DataType::Auto)
            .temperature(0.7)
            .top_p(0.9)
            .max_tokens(1024)
            .paged_attention(true)
            .build();

        assert!(matches!(config.model_source, ModelSource::HuggingFace(_)));
        assert_eq!(config.architecture, ModelArchitecture::Plain);
        assert_eq!(config.dtype, DataType::Auto);
        assert_eq!(config.temperature, Some(0.7));
        assert_eq!(config.top_p, Some(0.9));
        assert_eq!(config.max_tokens, Some(1024));
        assert!(config.paged_attention);
    }

    #[test]
    fn test_model_source_variants() {
        let hf = ModelSource::huggingface("org/model");
        assert!(matches!(hf, ModelSource::HuggingFace(_)));

        let local = ModelSource::local("/path/to/model");
        assert!(matches!(local, ModelSource::Local(_)));

        let gguf = ModelSource::gguf("/path/to/model.gguf");
        assert!(matches!(gguf, ModelSource::Gguf(_)));

        let uqff = ModelSource::uqff("/path/to/model.uqff");
        assert!(matches!(uqff, ModelSource::Uqff(_)));
    }

    #[test]
    fn test_all_architecture_variants() {
        let variants = [
            ModelArchitecture::Plain,
            ModelArchitecture::Vision,
            ModelArchitecture::Diffusion,
            ModelArchitecture::Speech,
            ModelArchitecture::Embedding,
            ModelArchitecture::XLora,
            ModelArchitecture::Lora,
        ];
        assert_eq!(variants.len(), 7);
    }

    #[test]
    fn test_all_dtype_variants() {
        let variants = [DataType::F32, DataType::F16, DataType::BF16, DataType::Auto];
        assert_eq!(variants.len(), 4);
    }

    #[test]
    fn test_all_device_variants() {
        let variants = [Device::Auto, Device::Cpu, Device::Cuda(0), Device::Cuda(1), Device::Metal];
        assert_eq!(variants.len(), 5);
    }

    #[test]
    fn test_all_quantization_variants() {
        let variants = [
            QuantizationLevel::Q4_0,
            QuantizationLevel::Q4_1,
            QuantizationLevel::Q5_0,
            QuantizationLevel::Q5_1,
            QuantizationLevel::Q8_0,
            QuantizationLevel::Q8_1,
            QuantizationLevel::Q2K,
            QuantizationLevel::Q3K,
            QuantizationLevel::Q4K,
            QuantizationLevel::Q5K,
            QuantizationLevel::Q6K,
        ];
        assert_eq!(variants.len(), 11);
    }

    #[test]
    fn test_isq_config() {
        let config = IsqConfig::new(QuantizationLevel::Q4_0);
        assert_eq!(config.level, QuantizationLevel::Q4_0);
        assert!(config.layer_overrides.is_none());

        let mut overrides = HashMap::new();
        overrides.insert("layer1".to_string(), QuantizationLevel::Q8_0);
        let config_with_overrides = IsqConfig::with_overrides(QuantizationLevel::Q4_0, overrides);
        assert!(config_with_overrides.layer_overrides.is_some());
    }

    #[test]
    fn test_adapter_config() {
        let lora = AdapterConfig::lora("path/to/adapter");
        assert_eq!(lora.adapter_type, AdapterType::LoRA);
        assert!(lora.ordering.is_none());
        assert!(!lora.is_multi_adapter());
        assert_eq!(lora.all_adapter_ids(), vec!["path/to/adapter"]);

        let xlora = AdapterConfig::xlora("path/to/adapter", PathBuf::from("ordering.json"));
        assert_eq!(xlora.adapter_type, AdapterType::XLoRA);
        assert!(xlora.ordering.is_some());

        // Test multi-adapter LoRA
        let multi_lora = AdapterConfig::lora_multi(vec!["adapter1", "adapter2", "adapter3"]);
        assert_eq!(multi_lora.adapter_type, AdapterType::LoRA);
        assert!(multi_lora.is_multi_adapter());
        assert_eq!(multi_lora.all_adapter_ids(), vec!["adapter1", "adapter2", "adapter3"]);

        // Test with_additional_adapters
        let extended =
            AdapterConfig::lora("primary").with_additional_adapters(vec!["secondary", "tertiary"]);
        assert_eq!(extended.all_adapter_ids(), vec!["primary", "secondary", "tertiary"]);

        // Test X-LoRA with tgt_non_granular_index
        let xlora_with_index = AdapterConfig::xlora("xlora-model", PathBuf::from("order.json"))
            .with_tgt_non_granular_index(2);
        assert_eq!(xlora_with_index.tgt_non_granular_index, Some(2));
    }

    #[test]
    fn test_adapter_type_display() {
        assert_eq!(format!("{}", AdapterType::LoRA), "LoRA");
        assert_eq!(format!("{}", AdapterType::XLoRA), "X-LoRA");
    }

    #[test]
    fn test_matformer_config() {
        // Test simple creation
        let config = MatFormerConfig::new("2b");
        assert_eq!(config.target_size, "2b");
        assert!(config.config_path.is_none());

        // Test with config path
        let config_with_path = MatFormerConfig::with_config_path("4b", "/path/to/config.csv");
        assert_eq!(config_with_path.target_size, "4b");
        assert!(config_with_path.config_path.is_some());
        assert_eq!(config_with_path.config_path.unwrap(), PathBuf::from("/path/to/config.csv"));

        // Test builder pattern
        let config_builder = MatFormerConfig::new("E2B").config_path("/custom/path.csv");
        assert_eq!(config_builder.target_size, "E2B");
        assert!(config_builder.config_path.is_some());
    }

    #[test]
    fn test_matformer_in_config() {
        let config = MistralRsConfig::builder()
            .model_source(ModelSource::huggingface("google/gemma-3n-E4B-it"))
            .architecture(ModelArchitecture::Vision)
            .matformer(MatFormerConfig::new("E4B"))
            .build();

        assert!(config.matformer.is_some());
        assert_eq!(config.matformer.as_ref().unwrap().target_size, "E4B");
    }

    #[test]
    fn test_device_config_single() {
        let config = DeviceConfig::new(Device::Cuda(0));
        assert_eq!(config.device, Device::Cuda(0));
        assert!(!config.is_multi_device());
        assert_eq!(config.device_count(), 1);
    }

    #[test]
    fn test_device_config_with_map() {
        let mut map = HashMap::new();
        map.insert("layer1".to_string(), Device::Cuda(0));
        map.insert("layer2".to_string(), Device::Cuda(1));

        let config = DeviceConfig::with_map(Device::Auto, map);
        assert!(config.is_multi_device());
        assert!(config.device_count() >= 2);
    }

    #[test]
    fn test_device_config_multi_gpu() {
        let config = DeviceConfig::multi_gpu(vec![
            LayerDeviceRange::new(0, 16, Device::Cuda(0)),
            LayerDeviceRange::new(16, 32, Device::Cuda(1)),
        ]);

        assert!(config.is_multi_device());
        assert!(config.layer_ranges.is_some());
        assert_eq!(config.layer_ranges.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn test_device_config_with_layer_range() {
        let config = DeviceConfig::new(Device::Auto)
            .with_layer_range(0, 10, Device::Cuda(0))
            .with_layer_range(10, 20, Device::Cuda(1))
            .with_layer_range(20, 30, Device::Metal);

        assert!(config.is_multi_device());
        assert_eq!(config.layer_ranges.as_ref().unwrap().len(), 3);
    }

    #[test]
    fn test_layer_device_range() {
        let range = LayerDeviceRange::new(0, 16, Device::Cuda(0));
        assert_eq!(range.start_layer, 0);
        assert_eq!(range.end_layer, 16);
        assert_eq!(range.device, Device::Cuda(0));
    }
}
