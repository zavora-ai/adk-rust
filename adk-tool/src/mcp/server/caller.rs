//! The caller identity of each MCP request (spec 3.5 §7.3, §7.9).

use std::net::SocketAddr;

use adk_core::Result;
use async_trait::async_trait;
use axum::extract::ConnectInfo;
use axum::http::{Extensions, HeaderMap, Method, Uri, header, request::Parts};

/// Who sent an MCP request, as the server's [`McpCallerResolver`] established it.
///
/// The executor receives the caller with every call and the tool listing is filtered by it.
/// A resolver attaches whatever its executor needs, such as a verified requester and trust
/// tier, as a typed extension.
///
/// # Example
///
/// ```rust
/// use adk_tool::mcp::server::McpCaller;
///
/// #[derive(Clone, Debug, PartialEq)]
/// enum Tier {
///     Public,
///     Partner,
/// }
///
/// let caller = McpCaller::authenticated("partner:acme").with_extension(Tier::Partner);
/// assert_eq!(caller.id(), "partner:acme");
/// assert!(caller.is_authenticated());
/// assert_eq!(caller.extension::<Tier>(), Some(&Tier::Partner));
/// assert_eq!(McpCaller::anonymous().extension::<Tier>(), None);
/// ```
#[derive(Debug, Clone)]
pub struct McpCaller {
    id: String,
    authenticated: bool,
    extensions: Extensions,
}

impl McpCaller {
    /// The id of every unauthenticated caller.
    pub const ANONYMOUS_ID: &'static str = "anonymous";

    /// A caller that proved nothing.
    pub fn anonymous() -> Self {
        Self {
            id: Self::ANONYMOUS_ID.to_string(),
            authenticated: false,
            extensions: Extensions::new(),
        }
    }

    /// A caller whose credential the resolver verified, known by `id`.
    ///
    /// The id keys sessions and appears in logs, so it names the credential's holder and never
    /// carries the credential itself.
    pub fn authenticated(id: impl Into<String>) -> Self {
        Self { id: id.into(), authenticated: true, extensions: Extensions::new() }
    }

    /// The caller's id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Whether the resolver verified a credential.
    pub fn is_authenticated(&self) -> bool {
        self.authenticated
    }

    /// Attaches `value`, replacing any earlier value of the same type.
    pub fn with_extension<T: Clone + Send + Sync + 'static>(mut self, value: T) -> Self {
        self.extensions.insert(value);
        self
    }

    /// The attached value of type `T`, if any.
    pub fn extension<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.extensions.get::<T>()
    }
}

/// The HTTP request an [`McpCallerResolver`] reads: method, URI, headers, the peer address and
/// the request extensions earlier middleware added.
///
/// # Example
///
/// ```rust
/// use adk_tool::mcp::server::McpHttpRequest;
/// use axum::http::Request;
///
/// let (parts, _) = Request::builder()
///     .header("Authorization", "Bearer s3cret")
///     .body(())
///     .unwrap()
///     .into_parts();
/// let request = McpHttpRequest::new(&parts);
/// assert_eq!(request.bearer_token(), Some("s3cret"));
/// assert_eq!(request.peer_addr(), None);
/// ```
#[derive(Debug, Clone, Copy)]
pub struct McpHttpRequest<'a> {
    parts: &'a Parts,
}

impl<'a> McpHttpRequest<'a> {
    /// Wraps the request head `parts`.
    pub fn new(parts: &'a Parts) -> Self {
        Self { parts }
    }

    /// The request method.
    pub fn method(&self) -> &'a Method {
        &self.parts.method
    }

    /// The request URI.
    pub fn uri(&self) -> &'a Uri {
        &self.parts.uri
    }

    /// The request headers.
    pub fn headers(&self) -> &'a HeaderMap {
        &self.parts.headers
    }

    /// The extensions earlier middleware attached, such as a verified requester.
    pub fn extensions(&self) -> &'a Extensions {
        &self.parts.extensions
    }

    /// The peer's address, when the server was started with
    /// `into_make_service_with_connect_info::<SocketAddr>()`.
    pub fn peer_addr(&self) -> Option<SocketAddr> {
        self.parts.extensions.get::<ConnectInfo<SocketAddr>>().map(|info| info.0)
    }

    /// The token of an `Authorization: Bearer <token>` header.
    pub fn bearer_token(&self) -> Option<&'a str> {
        let value = self.parts.headers.get(header::AUTHORIZATION)?.to_str().ok()?;
        let (scheme, token) = value.split_once(' ')?;
        let token = token.trim();
        (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then_some(token)
    }
}

/// Turns each HTTP request to the MCP endpoint into the caller the executor receives
/// (spec 3.5 §7.9).
///
/// The router calls it for every request before the MCP transport reads the body. An error
/// refuses the request with the error's HTTP status ([`AdkError::http_status_code`]), so an
/// `Unauthorized` error answers `401` with `WWW-Authenticate: Bearer`. The agent edge resolves
/// its verified requester here; the example accepts tokens the owner issued.
///
/// [`AdkError::http_status_code`]: adk_core::AdkError::http_status_code
///
/// # Example
///
/// ```rust
/// use std::collections::HashMap;
///
/// use adk_core::{AdkError, ErrorComponent, Result};
/// use adk_tool::mcp::server::{McpCaller, McpCallerResolver, McpHttpRequest};
///
/// /// Callers holding a token the site owner issued; everyone else is refused.
/// struct IssuedTokens(HashMap<String, String>);
///
/// #[async_trait::async_trait]
/// impl McpCallerResolver for IssuedTokens {
///     async fn resolve(&self, request: &McpHttpRequest<'_>) -> Result<McpCaller> {
///         let holder = request.bearer_token().and_then(|token| self.0.get(token));
///         match holder {
///             Some(holder) => Ok(McpCaller::authenticated(holder.clone())),
///             None => Err(AdkError::unauthorized(
///                 ErrorComponent::Auth,
///                 "mcp.caller.token",
///                 "a bearer token the site issued is required",
///             )),
///         }
///     }
/// }
/// ```
#[async_trait]
pub trait McpCallerResolver: Send + Sync {
    /// Resolves the caller of `request`, or refuses the request.
    ///
    /// # Errors
    ///
    /// Returns the error the request is refused with.
    async fn resolve(&self, request: &McpHttpRequest<'_>) -> Result<McpCaller>;
}

/// Resolves every request as [`McpCaller::anonymous`]: the default, for a server whose
/// executor decides by tool alone.
///
/// # Example
///
/// ```rust
/// # tokio_test_block(async {
/// use adk_tool::mcp::server::{AnonymousCallers, McpCallerResolver, McpHttpRequest};
/// use axum::http::Request;
///
/// let (parts, _) = Request::builder().body(()).unwrap().into_parts();
/// let caller = AnonymousCallers.resolve(&McpHttpRequest::new(&parts)).await.unwrap();
/// assert!(!caller.is_authenticated());
/// # });
/// # fn tokio_test_block(f: impl std::future::Future<Output = ()>) {
/// #     tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f)
/// # }
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct AnonymousCallers;

#[async_trait]
impl McpCallerResolver for AnonymousCallers {
    async fn resolve(&self, _request: &McpHttpRequest<'_>) -> Result<McpCaller> {
        Ok(McpCaller::anonymous())
    }
}
