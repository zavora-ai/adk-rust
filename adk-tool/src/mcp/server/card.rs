//! MCP Server Cards (SEP-2127) and AI Catalog entries (spec 3.5 §4.4, §7.9).
//!
//! The types follow the Server Card extension's `schema.ts` at the snapshot SEP-2127 was
//! accepted against (`modelcontextprotocol/experimental-ext-server-card@526201bb`), and the AI
//! Catalog entry its `docs/discovery.md` describes.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The `$schema` URI every Server Card carries (SEP-2127, schema `v1`).
pub const SERVER_CARD_SCHEMA_URL: &str =
    "https://static.modelcontextprotocol.io/schemas/v1/server-card.schema.json";

/// The media type of a Server Card document.
pub const SERVER_CARD_MEDIA_TYPE: &str = "application/mcp-server-card+json";

/// The media type of an AI Catalog document.
pub const AI_CATALOG_MEDIA_TYPE: &str = "application/ai-catalog+json";

/// The AI Catalog `specVersion` the catalog documents carry.
pub const AI_CATALOG_SPEC_VERSION: &str = "1.0";

/// The card's primary location, relative to the Streamable HTTP endpoint: SEP-2127 reserves
/// `<streamable-http-url>/server-card`, and AI Catalog entries point there.
pub const SERVER_CARD_ENDPOINT_SUFFIX: &str = "/server-card";

/// The alias path spec 3.5 §7.9 names for the card on the agent edge.
pub const SERVER_CARD_WELL_KNOWN_PATH: &str = "/.well-known/mcp/server-card.json";

/// The well-known path of a domain's AI Catalog.
pub const AI_CATALOG_WELL_KNOWN_PATH: &str = "/.well-known/ai-catalog.json";

/// A static description of a remote MCP server for discovery before connecting (SEP-2127).
///
/// The card names the server, its version and how to reach it. It lists no tools: clients
/// read those from `tools/list` once connected. [`McpServer::server_card`] builds the card
/// from the same declaration that serves `initialize`, so the two agree.
///
/// [`McpServer::server_card`]: super::McpServer::server_card
///
/// # Example
///
/// ```rust
/// use adk_tool::mcp::server::{SERVER_CARD_SCHEMA_URL, ServerCard};
///
/// let card: ServerCard = serde_json::from_value(serde_json::json!({
///     "$schema": SERVER_CARD_SCHEMA_URL,
///     "name": "com.example/shop",
///     "version": "1.0.0",
///     "description": "Quotes and bookings",
/// }))?;
/// assert_eq!(card.name, "com.example/shop");
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ServerCard {
    /// The schema URI, always [`SERVER_CARD_SCHEMA_URL`].
    #[serde(rename = "$schema")]
    pub schema: String,
    /// Reverse-DNS name with exactly one `/`, such as `com.example/shop`.
    pub name: String,
    /// The server's version; matches `serverInfo.version`.
    pub version: String,
    /// What the server does, 1 to 100 characters.
    pub description: String,
    /// Display name, 1 to 100 characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The server's homepage or documentation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub website_url: Option<String>,
    /// Where the server's source code lives.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<ServerCardRepository>,
    /// Icons a client may display.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icons: Option<Vec<ServerCardIcon>>,
    /// The endpoints a client connects to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remotes: Option<Vec<ServerCardRemote>>,
    /// Vendor metadata under reverse-DNS keys.
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
}

/// Repository metadata for a server's source code.
///
/// # Example
///
/// ```rust
/// use adk_tool::mcp::server::ServerCardRepository;
///
/// let repo = ServerCardRepository::new("https://github.com/example/shop-mcp", "github")
///     .with_subfolder("server");
/// assert_eq!(repo.subfolder.as_deref(), Some("server"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ServerCardRepository {
    /// Repository URL for browsing and cloning.
    pub url: String,
    /// The hosting service, such as `github`.
    pub source: String,
    /// Path of the server within the repository.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subfolder: Option<String>,
    /// The hosting service's stable repository identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

impl ServerCardRepository {
    /// Creates repository metadata for `url` hosted on `source`.
    pub fn new(url: impl Into<String>, source: impl Into<String>) -> Self {
        Self { url: url.into(), source: source.into(), subfolder: None, id: None }
    }

    /// Sets the server's path within the repository.
    pub fn with_subfolder(mut self, subfolder: impl Into<String>) -> Self {
        self.subfolder = Some(subfolder.into());
        self
    }

    /// Sets the hosting service's repository identifier.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }
}

/// An icon a client may display for the server.
///
/// # Example
///
/// ```rust
/// use adk_tool::mcp::server::{IconTheme, ServerCardIcon};
///
/// let icon = ServerCardIcon::new("https://example.com/icon.png")
///     .with_mime_type("image/png")
///     .with_sizes(["48x48"])
///     .with_theme(IconTheme::Light);
/// assert_eq!(icon.theme, Some(IconTheme::Light));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ServerCardIcon {
    /// An `https:` URL or a `data:` URI.
    pub src: String,
    /// The image's MIME type when the source does not state it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// Sizes as `WxH` or `any`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sizes: Option<Vec<String>>,
    /// The background the icon is drawn for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<IconTheme>,
}

impl ServerCardIcon {
    /// Creates an icon at `src`.
    pub fn new(src: impl Into<String>) -> Self {
        Self { src: src.into(), mime_type: None, sizes: None, theme: None }
    }

    /// Sets the MIME type.
    pub fn with_mime_type(mut self, mime_type: impl Into<String>) -> Self {
        self.mime_type = Some(mime_type.into());
        self
    }

    /// Sets the sizes the icon fits.
    pub fn with_sizes(mut self, sizes: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.sizes = Some(sizes.into_iter().map(Into::into).collect());
        self
    }

    /// Sets the background theme.
    pub fn with_theme(mut self, theme: IconTheme) -> Self {
        self.theme = Some(theme);
        self
    }
}

/// The background an icon is designed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IconTheme {
    /// A light background.
    Light,
    /// A dark background.
    Dark,
}

/// The transport a remote endpoint speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RemoteTransport {
    /// MCP Streamable HTTP.
    StreamableHttp,
    /// The deprecated HTTP+SSE transport.
    Sse,
}

/// One endpoint a client can connect to.
///
/// # Example
///
/// ```rust
/// use adk_tool::mcp::server::{RemoteTransport, ServerCardHeader, ServerCardRemote};
///
/// let remote = ServerCardRemote::new(RemoteTransport::StreamableHttp, "https://agent.example.com/mcp")
///     .with_header(ServerCardHeader::new("Authorization").required().secret())
///     .with_protocol_versions(["2025-11-25"]);
/// assert_eq!(remote.headers.as_ref().map(Vec::len), Some(1));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ServerCardRemote {
    /// The transport.
    #[serde(rename = "type")]
    pub transport: RemoteTransport,
    /// The endpoint URL, or a template whose `{variables}` are listed in `variables`.
    pub url: String,
    /// Headers the client sends.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<Vec<ServerCardHeader>>,
    /// Variables referenced from `url`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variables: Option<BTreeMap<String, ServerCardInput>>,
    /// MCP protocol versions the endpoint supports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supported_protocol_versions: Option<Vec<String>>,
}

impl ServerCardRemote {
    /// Creates an endpoint description.
    pub fn new(transport: RemoteTransport, url: impl Into<String>) -> Self {
        Self {
            transport,
            url: url.into(),
            headers: None,
            variables: None,
            supported_protocol_versions: None,
        }
    }

    /// Adds a header the client sends.
    pub fn with_header(mut self, header: ServerCardHeader) -> Self {
        self.headers.get_or_insert_with(Vec::new).push(header);
        self
    }

    /// Adds a variable referenced from the URL.
    pub fn with_variable(mut self, name: impl Into<String>, input: ServerCardInput) -> Self {
        self.variables.get_or_insert_with(BTreeMap::new).insert(name.into(), input);
        self
    }

    /// Sets the supported MCP protocol versions.
    pub fn with_protocol_versions(
        mut self,
        versions: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.supported_protocol_versions = Some(versions.into_iter().map(Into::into).collect());
        self
    }
}

/// A value a client supplies or a card presets: a URL variable or a header value.
///
/// # Example
///
/// ```rust
/// use adk_tool::mcp::server::ServerCardInput;
///
/// let tenant = ServerCardInput::new().with_description("Tenant subdomain").required();
/// assert_eq!(tenant.is_required, Some(true));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ServerCardInput {
    /// What the value is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether the connection needs it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_required: Option<bool>,
    /// Whether the value is a secret.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_secret: Option<bool>,
    /// `string`, `number`, `boolean` or `filepath`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<InputFormat>,
    /// The default value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// An example shown while configuring.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// A preset value the user cannot change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// The allowed values.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub choices: Option<Vec<String>>,
}

impl ServerCardInput {
    /// Creates an input with no properties set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Marks the input as required.
    pub fn required(mut self) -> Self {
        self.is_required = Some(true);
        self
    }

    /// Marks the input as a secret.
    pub fn secret(mut self) -> Self {
        self.is_secret = Some(true);
        self
    }

    /// Sets the preset value.
    pub fn with_value(mut self, value: impl Into<String>) -> Self {
        self.value = Some(value.into());
        self
    }
}

/// The format of a [`ServerCardInput`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputFormat {
    /// A string.
    String,
    /// A decimal number.
    Number,
    /// `true` or `false`.
    Boolean,
    /// A path on the user's file system.
    Filepath,
}

/// A header a client sends to a remote endpoint (`KeyValueInput` in SEP-2127).
///
/// # Example
///
/// ```rust
/// use adk_tool::mcp::server::{ServerCardHeader, ServerCardInput};
///
/// let auth = ServerCardHeader::new("Authorization")
///     .with_description("Bearer token issued by the site owner")
///     .with_value("Bearer {token}")
///     .with_variable("token", ServerCardInput::new().required().secret())
///     .required()
///     .secret();
/// assert_eq!(auth.name, "Authorization");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ServerCardHeader {
    /// The header name.
    pub name: String,
    /// The value's properties.
    #[serde(flatten)]
    pub input: ServerCardInput,
    /// Variables referenced from `value`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variables: Option<BTreeMap<String, ServerCardInput>>,
}

impl ServerCardHeader {
    /// Creates a header description.
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), input: ServerCardInput::default(), variables: None }
    }

    /// Sets the description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.input.description = Some(description.into());
        self
    }

    /// Sets the value, which may reference variables as `{name}`.
    pub fn with_value(mut self, value: impl Into<String>) -> Self {
        self.input.value = Some(value.into());
        self
    }

    /// Adds a variable referenced from the value.
    pub fn with_variable(mut self, name: impl Into<String>, input: ServerCardInput) -> Self {
        self.variables.get_or_insert_with(BTreeMap::new).insert(name.into(), input);
        self
    }

    /// Marks the header as required.
    pub fn required(mut self) -> Self {
        self.input.is_required = Some(true);
        self
    }

    /// Marks the header's value as a secret.
    pub fn secret(mut self) -> Self {
        self.input.is_secret = Some(true);
        self
    }
}

/// An AI Catalog document (`application/ai-catalog+json`).
///
/// # Example
///
/// ```rust
/// use adk_tool::mcp::server::{AiCatalog, AiCatalogEntry, AiCatalogHost};
///
/// let catalog = AiCatalog::new([AiCatalogEntry::server_card_url(
///     "urn:air:example.com:mcp:shop",
///     "https://agent.example.com/mcp/server-card",
/// )])
/// .with_host(AiCatalogHost::new("Example Solar"));
/// let json = serde_json::to_value(&catalog)?;
/// assert_eq!(json["specVersion"], "1.0");
/// assert_eq!(json["entries"][0]["type"], "application/mcp-server-card+json");
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AiCatalog {
    /// The AI Catalog version, `1.0`.
    pub spec_version: String,
    /// Who operates the catalog.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<AiCatalogHost>,
    /// The artifacts the catalog lists.
    pub entries: Vec<AiCatalogEntry>,
}

impl AiCatalog {
    /// Creates a catalog listing `entries`.
    pub fn new(entries: impl IntoIterator<Item = AiCatalogEntry>) -> Self {
        Self {
            spec_version: AI_CATALOG_SPEC_VERSION.to_string(),
            host: None,
            entries: entries.into_iter().collect(),
        }
    }

    /// Names the catalog's operator.
    pub fn with_host(mut self, host: AiCatalogHost) -> Self {
        self.host = Some(host);
        self
    }
}

/// The operator of an AI Catalog.
///
/// # Example
///
/// ```rust
/// use adk_tool::mcp::server::AiCatalogHost;
///
/// let host = AiCatalogHost::new("Example Solar").with_identifier("did:web:example.com");
/// assert_eq!(host.identifier.as_deref(), Some("did:web:example.com"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AiCatalogHost {
    /// The operator's name.
    pub display_name: String,
    /// A verifiable identifier, such as a DID or a domain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identifier: Option<String>,
    /// The operator's documentation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documentation_url: Option<String>,
    /// The operator's logo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logo_url: Option<String>,
}

impl AiCatalogHost {
    /// Creates host information for `display_name`.
    pub fn new(display_name: impl Into<String>) -> Self {
        Self {
            display_name: display_name.into(),
            identifier: None,
            documentation_url: None,
            logo_url: None,
        }
    }

    /// Sets the verifiable identifier.
    pub fn with_identifier(mut self, identifier: impl Into<String>) -> Self {
        self.identifier = Some(identifier.into());
        self
    }

    /// Sets the documentation URL.
    pub fn with_documentation_url(mut self, url: impl Into<String>) -> Self {
        self.documentation_url = Some(url.into());
        self
    }
}

/// One artifact in an AI Catalog: here, an MCP Server Card by URL or inline.
///
/// The card carries its own title, description and version, so the entry repeats none of
/// them, as the SEP-2127 discovery document recommends.
///
/// # Example
///
/// ```rust
/// use adk_tool::mcp::server::{AiCatalogContent, AiCatalogEntry};
///
/// let entry = AiCatalogEntry::server_card_url(
///     "urn:air:example.com:mcp:shop",
///     "https://agent.example.com/mcp/server-card",
/// );
/// assert!(matches!(entry.content, AiCatalogContent::Url(_)));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AiCatalogEntry {
    /// A stable identifier, `urn:air:{publisher}:{namespace}:{name}` for open systems.
    pub identifier: String,
    /// The artifact's type; [`SERVER_CARD_MEDIA_TYPE`] for a Server Card.
    #[serde(rename = "type")]
    pub artifact_type: String,
    /// Where the artifact is, or the artifact itself.
    #[serde(flatten)]
    pub content: AiCatalogContent,
    /// Keywords for filtering.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

impl AiCatalogEntry {
    /// An entry pointing at the Server Card served at `url`.
    pub fn server_card_url(identifier: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            identifier: identifier.into(),
            artifact_type: SERVER_CARD_MEDIA_TYPE.to_string(),
            content: AiCatalogContent::Url(url.into()),
            tags: None,
        }
    }

    /// An entry carrying `card` inline.
    ///
    /// # Errors
    ///
    /// Returns the serialisation error when `card` holds a value JSON cannot represent.
    pub fn server_card_inline(
        identifier: impl Into<String>,
        card: &ServerCard,
    ) -> serde_json::Result<Self> {
        Ok(Self {
            identifier: identifier.into(),
            artifact_type: SERVER_CARD_MEDIA_TYPE.to_string(),
            content: AiCatalogContent::Data(serde_json::to_value(card)?),
            tags: None,
        })
    }

    /// Sets the entry's tags.
    pub fn with_tags(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.tags = Some(tags.into_iter().map(Into::into).collect());
        self
    }
}

/// Exactly one of an AI Catalog entry's `url` or `data` members.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AiCatalogContent {
    /// The artifact is served at this URL.
    Url(String),
    /// The artifact, inline.
    Data(Value),
}

/// The AI Catalog identifier for a card named `namespace/name`:
/// `urn:air:{reversed namespace}:mcp:{name}`, so `com.example/shop` becomes
/// `urn:air:example.com:mcp:shop` as in the SEP-2127 discovery document.
pub(crate) fn catalog_identifier(card_name: &str) -> String {
    let (namespace, name) = card_name.split_once('/').unwrap_or(("", card_name));
    let publisher: Vec<&str> = namespace.split('.').rev().collect();
    format!("urn:air:{}:mcp:{name}", publisher.join("."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_catalog_identifier_reverses_the_namespace() {
        assert_eq!(catalog_identifier("com.example/weather"), "urn:air:example.com:mcp:weather");
        assert_eq!(catalog_identifier("io.github.acme/shop"), "urn:air:acme.github.io:mcp:shop");
    }

    #[test]
    fn a_header_flattens_its_input_properties() {
        let header = ServerCardHeader::new("Authorization")
            .with_value("Bearer {token}")
            .with_variable("token", ServerCardInput::new().required().secret())
            .required()
            .secret();
        assert_eq!(
            serde_json::to_value(&header).unwrap(),
            json!({
                "name": "Authorization",
                "isRequired": true,
                "isSecret": true,
                "value": "Bearer {token}",
                "variables": { "token": { "isRequired": true, "isSecret": true } },
            })
        );
    }

    #[test]
    fn an_entry_carries_exactly_one_of_url_or_data() {
        let by_url = AiCatalogEntry::server_card_url("urn:air:example.com:mcp:shop", "https://x/y");
        assert_eq!(
            serde_json::to_value(&by_url).unwrap(),
            json!({
                "identifier": "urn:air:example.com:mcp:shop",
                "type": SERVER_CARD_MEDIA_TYPE,
                "url": "https://x/y",
            })
        );
        let card = ServerCard {
            schema: SERVER_CARD_SCHEMA_URL.into(),
            name: "com.example/shop".into(),
            version: "1.0.0".into(),
            description: "Shop".into(),
            title: None,
            website_url: None,
            repository: None,
            icons: None,
            remotes: None,
            meta: None,
        };
        let inline =
            AiCatalogEntry::server_card_inline("urn:air:example.com:mcp:shop", &card).unwrap();
        let value = serde_json::to_value(&inline).unwrap();
        assert_eq!(value["data"]["name"], "com.example/shop");
        assert!(value.get("url").is_none());
        let back: AiCatalogEntry = serde_json::from_value(value).unwrap();
        assert_eq!(back, inline);
    }
}
