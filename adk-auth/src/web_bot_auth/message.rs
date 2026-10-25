//! HTTP message components and the signature base ([RFC 9421] §2). Spec 3.5 §7.3.
//!
//! [RFC 9421]: https://www.rfc-editor.org/rfc/rfc9421

use http::{HeaderMap, Method, Uri};

use super::WebBotAuthError;
use super::structured::{BareItem, Dictionary, InnerList, Item, List, ListEntry, Parameters};

/// Fields whose structured type is known, so `sf` and `key` can reserialise them
/// (RFC 9421 §2.1.1–§2.1.2). All four are Dictionaries.
const DICTIONARY_FIELDS: &[&str] =
    &["signature", "signature-input", "signature-agent", "accept-signature"];

/// A component identifier (RFC 9421 §2): a component name and its ordered parameters.
///
/// Derived components start with `@`; any other name is a lowercase HTTP field name.
///
/// # Example
///
/// ```rust
/// use adk_auth::web_bot_auth::ComponentId;
///
/// let agent = ComponentId::signature_agent("sig1");
/// assert_eq!(agent.serialize().unwrap(), r#""signature-agent";key="sig1""#);
/// assert_eq!(ComponentId::new("@authority").serialize().unwrap(), r#""@authority""#);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentId {
    name: String,
    params: Parameters,
}

impl ComponentId {
    /// Creates an identifier without parameters.
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), params: Parameters::new() }
    }

    /// The `"signature-agent";key="<member>"` identifier the Web Bot Auth draft requires
    /// signatures to cover (§5.2.1).
    pub fn signature_agent(member: impl Into<String>) -> Self {
        Self::new(super::SIGNATURE_AGENT).with_param("key", BareItem::String(member.into()))
    }

    /// Adds or replaces a parameter, such as `key` or `name`.
    pub fn with_param(mut self, key: impl Into<String>, value: BareItem) -> Self {
        self.params.insert(key, value);
        self
    }

    /// The component name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The parameters, in order.
    pub fn params(&self) -> &Parameters {
        &self.params
    }

    /// Serialises the identifier as it appears in a signature base (RFC 9421 §2.5).
    ///
    /// # Errors
    ///
    /// Returns [`WebBotAuthError::SignatureBase`] when the name or a parameter is not a valid
    /// structured field value.
    pub fn serialize(&self) -> Result<String, WebBotAuthError> {
        self.to_item()
            .serialize()
            .map_err(|e| WebBotAuthError::SignatureBase { reason: e.to_string() })
    }

    fn to_item(&self) -> Item {
        Item { bare_item: BareItem::String(self.name.clone()), params: self.params.clone() }
    }

    fn from_item(item: &Item) -> Option<Self> {
        let name = item.bare_item.as_str()?;
        Some(Self { name: name.to_string(), params: item.params.clone() })
    }

    /// RFC 9421 §2: identifiers are equal when names and parameter sets are, in any order.
    pub(crate) fn same_as(&self, other: &Self) -> bool {
        self.name == other.name
            && self.params.len() == other.params.len()
            && self.params.iter().all(|(key, value)| other.params.get(key) == Some(value))
    }

    fn string_param(&self, key: &str) -> Option<&str> {
        self.params.get(key).and_then(BareItem::as_str)
    }

    fn flag(&self, key: &str) -> Result<bool, WebBotAuthError> {
        match self.params.get(key) {
            None => Ok(false),
            Some(BareItem::Boolean(true)) => Ok(true),
            Some(_) => {
                Err(base_error(format!("parameter `{key}` on {} must be a flag", self.name)))
            }
        }
    }
}

/// The covered components and signature parameters of one signature (RFC 9421 §2.3, §4.1).
///
/// The parameters keep their order, which the signature covers.
///
/// # Example
///
/// ```rust
/// use adk_auth::web_bot_auth::SignatureInput;
///
/// let field = r#"sig-b26=("date" "@method" "@path" "@authority" "content-type" "content-length");created=1618884473;keyid="test-key-ed25519""#;
/// let inputs = SignatureInput::parse_field(field).unwrap();
/// let (label, input) = &inputs[0];
/// assert_eq!(label, "sig-b26");
/// assert_eq!(input.created(), Some(1_618_884_473));
/// assert_eq!(input.keyid(), Some("test-key-ed25519"));
/// assert_eq!(input.components.len(), 6);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SignatureInput {
    /// The covered components, in signature-base order.
    pub components: Vec<ComponentId>,
    /// The signature parameters, in order.
    pub params: Parameters,
}

impl SignatureInput {
    /// Creates an input covering `components`, with no parameters.
    pub fn new(components: Vec<ComponentId>) -> Self {
        Self { components, params: Parameters::new() }
    }

    /// Parses a `Signature-Input` field value into its labelled inputs (RFC 9421 §4.1).
    ///
    /// # Errors
    ///
    /// Returns [`WebBotAuthError::MalformedField`] when the value is not a Dictionary of Inner
    /// Lists of Strings, or repeats a label.
    pub fn parse_field(value: &str) -> Result<Vec<(String, Self)>, WebBotAuthError> {
        let malformed = |reason: String| WebBotAuthError::MalformedField {
            field: super::SIGNATURE_INPUT.to_string(),
            reason,
        };
        let dict = Dictionary::parse_unique(value).map_err(|e| malformed(e.to_string()))?;
        dict.iter()
            .map(|(label, member)| {
                let ListEntry::InnerList(list) = member else {
                    return Err(malformed(format!("member `{label}` is not an inner list")));
                };
                let components = list
                    .items
                    .iter()
                    .map(ComponentId::from_item)
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(|| {
                        malformed(format!(
                            "member `{label}` lists a component that is not a string"
                        ))
                    })?;
                Ok((label.to_string(), Self { components, params: list.params.clone() }))
            })
            .collect()
    }

    /// The `@signature-params` value: the covered components and parameters as an Inner List
    /// (RFC 9421 §2.3).
    ///
    /// # Errors
    ///
    /// Returns [`WebBotAuthError::SignatureBase`] when a component or parameter is not a valid
    /// structured field value.
    pub fn serialize(&self) -> Result<String, WebBotAuthError> {
        self.to_inner_list()
            .serialize()
            .map_err(|e| WebBotAuthError::SignatureBase { reason: e.to_string() })
    }

    pub(crate) fn to_inner_list(&self) -> InnerList {
        InnerList {
            items: self.components.iter().map(ComponentId::to_item).collect(),
            params: self.params.clone(),
        }
    }

    /// The `created` parameter.
    pub fn created(&self) -> Option<i64> {
        self.params.get("created").and_then(BareItem::as_integer)
    }

    /// The `expires` parameter.
    pub fn expires(&self) -> Option<i64> {
        self.params.get("expires").and_then(BareItem::as_integer)
    }

    /// The `keyid` parameter.
    pub fn keyid(&self) -> Option<&str> {
        self.params.get("keyid").and_then(BareItem::as_str)
    }

    /// The `alg` parameter.
    pub fn alg(&self) -> Option<&str> {
        self.params.get("alg").and_then(BareItem::as_str)
    }

    /// The `tag` parameter.
    pub fn tag(&self) -> Option<&str> {
        self.params.get("tag").and_then(BareItem::as_str)
    }

    /// The `nonce` parameter.
    pub fn nonce(&self) -> Option<&str> {
        self.params.get("nonce").and_then(BareItem::as_str)
    }

    /// Whether the input covers `component` (parameters compared as a set).
    pub fn covers(&self, component: &ComponentId) -> bool {
        self.components.iter().any(|covered| covered.same_as(component))
    }
}

/// The parts of an HTTP request a signature can cover: method, target URI and header fields.
///
/// A server sees an origin-form URI (`/path?query`) and the authority in `Host` (HTTP/1.1) or
/// in the URI (HTTP/2 and HTTP/3); the scheme of an origin-form URI is the connection's,
/// `https` unless [`with_default_scheme`](Self::with_default_scheme) says otherwise.
///
/// # Example
///
/// ```rust
/// use adk_auth::web_bot_auth::{ComponentId, RequestHead, SignatureInput};
///
/// let request = http::Request::get("/foo?a=1").header("host", "Example.COM:443").body(()).unwrap();
/// let input = SignatureInput::new(vec![
///     ComponentId::new("@method"),
///     ComponentId::new("@authority"),
///     ComponentId::new("@target-uri"),
/// ]);
/// let base = RequestHead::from_request(&request).signature_base(&input).unwrap();
/// assert_eq!(
///     base,
///     "\"@method\": GET\n\"@authority\": example.com\n\"@target-uri\": https://example.com/foo?a=1\n\
///      \"@signature-params\": (\"@method\" \"@authority\" \"@target-uri\")"
/// );
/// ```
#[derive(Debug, Clone, Copy)]
pub struct RequestHead<'a> {
    method: &'a Method,
    uri: &'a Uri,
    headers: &'a HeaderMap,
    default_scheme: &'a str,
}

impl<'a> RequestHead<'a> {
    /// Creates a request head from its parts.
    pub fn new(method: &'a Method, uri: &'a Uri, headers: &'a HeaderMap) -> Self {
        Self { method, uri, headers, default_scheme: "https" }
    }

    /// Creates a request head from an [`http::Request`].
    pub fn from_request<B>(request: &'a http::Request<B>) -> Self {
        Self::new(request.method(), request.uri(), request.headers())
    }

    /// Sets the scheme used when the URI carries none.
    pub fn with_default_scheme(mut self, scheme: &'a str) -> Self {
        self.default_scheme = scheme;
        self
    }

    /// The header fields.
    pub fn headers(&self) -> &'a HeaderMap {
        self.headers
    }

    /// Builds the signature base for `input` over this request (RFC 9421 §2.5).
    ///
    /// # Errors
    ///
    /// Returns [`WebBotAuthError::SignatureBase`] when a covered component is unknown, repeated,
    /// absent from the request, or carries a parameter that does not apply to a request.
    pub fn signature_base(&self, input: &SignatureInput) -> Result<String, WebBotAuthError> {
        signature_base(input, |component| self.component_value(component))
    }

    fn scheme(&self) -> String {
        self.uri.scheme_str().unwrap_or(self.default_scheme).to_ascii_lowercase()
    }

    fn authority(&self) -> Result<String, WebBotAuthError> {
        let raw = match self.uri.authority() {
            Some(authority) => authority.as_str().to_string(),
            None => {
                let mut hosts = self.headers.get_all(http::header::HOST).iter();
                let (Some(host), None) = (hosts.next(), hosts.next()) else {
                    return Err(base_error("the request has no single authority or Host field"));
                };
                host.to_str()
                    .map_err(|_| base_error("the Host field is not ASCII"))?
                    .trim()
                    .to_string()
            }
        };
        let authority: http::uri::Authority =
            raw.parse().map_err(|_| base_error(format!("`{raw}` is not a valid authority")))?;
        if authority.as_str().contains('@') {
            return Err(base_error("the authority must not carry user information"));
        }
        let host = authority.host().to_ascii_lowercase();
        let default_port = match self.scheme().as_str() {
            "https" => Some(443),
            "http" => Some(80),
            _ => None,
        };
        Ok(match authority.port_u16() {
            Some(port) if Some(port) != default_port => format!("{host}:{port}"),
            Some(_) | None => host,
        })
    }

    fn path(&self) -> &str {
        match self.uri.path() {
            "" => "/",
            path => path,
        }
    }

    fn component_value(&self, component: &ComponentId) -> Result<String, WebBotAuthError> {
        let name = component.name();
        if component.params.get("req").is_some() {
            return Err(base_error(format!(
                "`{name}` has the `req` flag, which a request cannot use"
            )));
        }
        if name.starts_with('@') {
            let allowed: &[&str] = if name == "@query-param" { &["name"] } else { &[] };
            if let Some((key, _)) = component.params.iter().find(|(key, _)| !allowed.contains(key))
            {
                return Err(base_error(format!("parameter `{key}` does not apply to `{name}`")));
            }
        }
        match name {
            "@method" => Ok(self.method.as_str().to_string()),
            "@scheme" => Ok(self.scheme()),
            "@authority" => self.authority(),
            "@path" => Ok(self.path().to_string()),
            "@query" => Ok(format!("?{}", self.uri.query().unwrap_or_default())),
            "@target-uri" => {
                let query = self.uri.query().map(|q| format!("?{q}")).unwrap_or_default();
                Ok(format!("{}://{}{}{query}", self.scheme(), self.authority()?, self.path()))
            }
            "@request-target" => Ok(self
                .uri
                .path_and_query()
                .map_or_else(|| self.path().to_string(), |pq| pq.as_str().to_string())),
            "@query-param" => {
                let wanted = component
                    .string_param("name")
                    .ok_or_else(|| base_error("`@query-param` needs a string `name` parameter"))?;
                query_param(self.uri.query().unwrap_or_default(), wanted)
            }
            "@status" => Err(base_error("`@status` applies only to responses")),
            "@signature-params" => {
                Err(base_error("`@signature-params` must not be listed as a covered component"))
            }
            _ if name.starts_with('@') => {
                Err(base_error(format!("`{name}` is not a derived component this verifier knows")))
            }
            _ => self.field_value(component),
        }
    }

    fn field_value(&self, component: &ComponentId) -> Result<String, WebBotAuthError> {
        let name = component.name();
        if name.is_empty() || name.bytes().any(|b| b.is_ascii_uppercase()) {
            return Err(base_error(format!("field name `{name}` must be lowercase and non-empty")));
        }
        if let Some((key, _)) =
            component.params.iter().find(|(key, _)| !["key", "sf", "bs", "tr"].contains(key))
        {
            return Err(base_error(format!("parameter `{key}` does not apply to field `{name}`")));
        }
        if component.flag("tr")? {
            return Err(base_error(format!("trailer `{name}` is not available")));
        }
        let values = self
            .headers
            .get_all(name)
            .iter()
            .map(|value| {
                value
                    .to_str()
                    .map(|v| v.trim_matches([' ', '\t']).to_string())
                    .map_err(|_| base_error(format!("field `{name}` is not ASCII")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if values.is_empty() {
            return Err(base_error(format!("the request has no `{name}` field")));
        }
        let key = component.params.get("key");
        let sf = component.flag("sf")?;
        if component.flag("bs")? {
            if key.is_some() || sf {
                return Err(base_error(format!(
                    "`bs` cannot combine with `key` or `sf` on `{name}`"
                )));
            }
            let wrapped = List(
                values
                    .into_iter()
                    .map(|v| ListEntry::Item(Item::new(BareItem::ByteSequence(v.into_bytes()))))
                    .collect(),
            );
            return wrapped.serialize().map_err(|e| base_error(e.to_string()));
        }
        let combined = values.join(", ");
        if key.is_none() && !sf {
            return Ok(combined);
        }
        if !DICTIONARY_FIELDS.contains(&name) {
            return Err(base_error(format!("the structured type of field `{name}` is not known")));
        }
        let dict = Dictionary::parse(&combined)
            .map_err(|e| base_error(format!("field `{name}` is not a dictionary: {e}")))?;
        let serialized = match key {
            Some(BareItem::String(member)) => dict
                .get(member)
                .ok_or_else(|| base_error(format!("field `{name}` has no member `{member}`")))?
                .serialize(),
            Some(_) => return Err(base_error(format!("`key` on `{name}` must be a string"))),
            None => dict.serialize(),
        };
        serialized.map_err(|e| base_error(e.to_string()))
    }
}

/// Builds a signature base from covered components whose values `resolve` supplies.
pub(crate) fn signature_base(
    input: &SignatureInput,
    mut resolve: impl FnMut(&ComponentId) -> Result<String, WebBotAuthError>,
) -> Result<String, WebBotAuthError> {
    let mut base = String::new();
    for (index, component) in input.components.iter().enumerate() {
        if input.components[..index].iter().any(|earlier| earlier.same_as(component)) {
            return Err(base_error(format!("`{}` is covered twice", component.name())));
        }
        let value = resolve(component)?;
        if value.contains(['\n', '\r']) || !value.is_ascii() {
            return Err(base_error(format!(
                "the value of `{}` contains a newline or non-ASCII character",
                component.name()
            )));
        }
        base.push_str(&component.serialize()?);
        base.push_str(": ");
        base.push_str(&value);
        base.push('\n');
    }
    base.push_str("\"@signature-params\": ");
    base.push_str(&input.serialize()?);
    Ok(base)
}

fn base_error(reason: impl Into<String>) -> WebBotAuthError {
    WebBotAuthError::SignatureBase { reason: reason.into() }
}

/// The `@query-param` value of `wanted` (RFC 9421 §2.2.8): parsed as
/// `application/x-www-form-urlencoded`, then percent-encoded with spaces as `%20`.
fn query_param(query: &str, wanted: &str) -> Result<String, WebBotAuthError> {
    let mut found = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| pair.split_once('=').unwrap_or((pair, "")))
        .filter(|(name, _)| form_encode(&form_decode(name)) == wanted)
        .map(|(_, value)| form_encode(&form_decode(value)));
    match (found.next(), found.next()) {
        (Some(value), None) => Ok(value),
        (None, _) => Err(base_error(format!("the query has no parameter `{wanted}`"))),
        (Some(_), Some(_)) => {
            Err(base_error(format!("query parameter `{wanted}` occurs more than once")))
        }
    }
}

fn form_decode(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => out.push(b' '),
            b'%' if index + 2 < bytes.len() => {
                match (hex_value(bytes[index + 1]), hex_value(bytes[index + 2])) {
                    (Some(high), Some(low)) => {
                        out.push(high << 4 | low);
                        index += 2;
                    }
                    _ => out.push(b'%'),
                }
            }
            byte => out.push(byte),
        }
        index += 1;
    }
    out
}

fn hex_value(byte: u8) -> Option<u8> {
    char::from(byte).to_digit(16).and_then(|digit| u8::try_from(digit).ok())
}

/// Percent-encodes with the `application/x-www-form-urlencoded` set, leaving spaces as `%20`
/// as RFC 9421 §2.2.8 shows. Invalid UTF-8 is replaced first, as the URL standard's parser does.
fn form_encode(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"*-._".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head_base(uri: &str, components: &[ComponentId]) -> Result<String, WebBotAuthError> {
        let request = http::Request::get(uri).body(()).unwrap();
        RequestHead::from_request(&request)
            .signature_base(&SignatureInput::new(components.to_vec()))
    }

    #[test]
    fn query_param_values_follow_rfc_9421_examples() {
        // RFC 9421 §2.2.8.
        let query = "var=this%20is%20a%20big%0Amultiline%20value&bar=with+plus+whitespace&fa%C3%A7ade%22%3A%20=something";
        assert_eq!(query_param(query, "var").unwrap(), "this%20is%20a%20big%0Amultiline%20value");
        assert_eq!(query_param(query, "bar").unwrap(), "with%20plus%20whitespace");
        assert_eq!(query_param(query, "fa%C3%A7ade%22%3A%20").unwrap(), "something");
        let query = "param=value&foo=bar&baz=batman&qux=";
        assert_eq!(query_param(query, "baz").unwrap(), "batman");
        assert_eq!(query_param(query, "qux").unwrap(), "");
        assert!(query_param(query, "missing").is_err());
        assert!(query_param("a=1&a=2", "a").is_err());
    }

    #[test]
    fn derived_components_follow_rfc_9421_section_2_2() {
        let base = head_base(
            "https://www.example.com/path?param=value&foo=bar&baz=bat%2Dman",
            &[
                ComponentId::new("@scheme"),
                ComponentId::new("@query"),
                ComponentId::new("@request-target"),
                ComponentId::new("@query-param").with_param("name", BareItem::String("baz".into())),
            ],
        )
        .unwrap();
        let lines: Vec<&str> = base.lines().collect();
        assert_eq!(lines[0], "\"@scheme\": https");
        assert_eq!(lines[1], "\"@query\": ?param=value&foo=bar&baz=bat%2Dman");
        assert_eq!(lines[2], "\"@request-target\": /path?param=value&foo=bar&baz=bat%2Dman");
        assert_eq!(lines[3], "\"@query-param\";name=\"baz\": bat-man");
    }

    #[test]
    fn empty_query_and_path_normalise() {
        let base = head_base(
            "https://example.com",
            &[ComponentId::new("@path"), ComponentId::new("@query")],
        )
        .unwrap();
        assert!(base.starts_with("\"@path\": /\n\"@query\": ?\n"));
    }

    #[test]
    fn authority_drops_default_port_and_lowercases() {
        let base =
            head_base("https://EXAMPLE.com:443/x", &[ComponentId::new("@authority")]).unwrap();
        assert!(base.starts_with("\"@authority\": example.com\n"));
        let base =
            head_base("https://example.com:8443/x", &[ComponentId::new("@authority")]).unwrap();
        assert!(base.starts_with("\"@authority\": example.com:8443\n"));
    }

    #[test]
    fn invalid_components_are_errors() {
        let uri = "https://example.com/";
        assert!(head_base(uri, &[ComponentId::new("@status")]).is_err());
        assert!(head_base(uri, &[ComponentId::new("@signature-params")]).is_err());
        assert!(head_base(uri, &[ComponentId::new("@unknown")]).is_err());
        assert!(head_base(uri, &[ComponentId::new("x-missing")]).is_err());
        assert!(head_base(uri, &[ComponentId::new("Host")]).is_err());
        assert!(
            head_base(
                uri,
                &[ComponentId::new("@authority").with_param("req", BareItem::Boolean(true))]
            )
            .is_err()
        );
        assert!(
            head_base(uri, &[ComponentId::new("@authority"), ComponentId::new("@authority")])
                .is_err()
        );
    }

    #[test]
    fn field_parameters_follow_rfc_9421_section_2_1() {
        let request = http::Request::get("https://example.com/")
            .header("example-header", "value, with, lots")
            .header("example-header", "of, commas")
            .header(
                "signature-agent",
                "a=\"https://a.example\",   b=\"https://b.example\";type=jwks_uri",
            )
            .body(())
            .unwrap();
        let input = SignatureInput::new(vec![
            ComponentId::new("example-header"),
            ComponentId::new("example-header").with_param("bs", BareItem::Boolean(true)),
            ComponentId::new("signature-agent").with_param("sf", BareItem::Boolean(true)),
            ComponentId::signature_agent("b"),
        ]);
        let base = RequestHead::from_request(&request).signature_base(&input).unwrap();
        let lines: Vec<&str> = base.lines().collect();
        assert_eq!(lines[0], "\"example-header\": value, with, lots, of, commas");
        assert_eq!(
            lines[1],
            "\"example-header\";bs: :dmFsdWUsIHdpdGgsIGxvdHM=:, :b2YsIGNvbW1hcw==:"
        );
        assert_eq!(
            lines[2],
            "\"signature-agent\";sf: a=\"https://a.example\", b=\"https://b.example\";type=jwks_uri"
        );
        assert_eq!(lines[3], "\"signature-agent\";key=\"b\": \"https://b.example\";type=jwks_uri");
    }
}
