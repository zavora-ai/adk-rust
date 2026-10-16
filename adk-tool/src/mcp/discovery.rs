//! Bounds on MCP catalog listings, and the checks applied to server-chosen tool names.

use std::collections::HashSet;
use std::fmt;
use std::future::Future;
use std::time::Duration;

use rmcp::model::{PaginatedRequestParams, Tool};
use rmcp::service::Peer;
use rmcp::{RoleClient, ServiceError};

/// Bounds on each paginated catalog listing an MCP client sends.
///
/// [`McpToolset`](super::McpToolset) follows the server's `nextCursor` itself
/// for `tools/list`, `resources/list`, `resources/templates/list`, and
/// `prompts/list`, so a server that never stops paging cannot hold the caller.
/// [`McpServerManager`](super::McpServerManager), [`ConnectionRefresher`](super::ConnectionRefresher),
/// and [`SimpleClient`](super::SimpleClient) apply the same bounds.
///
/// | Bound | Fails the listing when | Default |
/// |-------|------------------------|---------|
/// | `max_pages` | The server still returns a `nextCursor` after this many pages | 64 |
/// | `timeout` | The whole listing, reconnects included, takes longer than this | 30 seconds |
///
/// A listing also fails as soon as the server returns a cursor it already
/// returned, because following it would request the same pages again. Each
/// failure is an `AdkError::Tool` naming the bound, so an agent turn that
/// resolves the toolset either skips it with a warning or, with
/// `strict_toolsets(true)`, fails promptly.
///
/// # Example
///
/// ```
/// use adk_tool::mcp::McpDiscoveryLimits;
/// use std::time::Duration;
///
/// let limits = McpDiscoveryLimits::default()
///     .with_max_pages(256)
///     .with_timeout(Duration::from_secs(10));
///
/// assert_eq!(limits.max_pages, 256);
/// assert_eq!(McpDiscoveryLimits::default().max_pages, McpDiscoveryLimits::DEFAULT_MAX_PAGES);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpDiscoveryLimits {
    /// Most pages requested for one listing. At least one page is always requested.
    pub max_pages: usize,
    /// Longest time one listing may take, including reconnect attempts.
    pub timeout: Duration,
}

impl McpDiscoveryLimits {
    /// Default value of [`max_pages`](Self::max_pages).
    pub const DEFAULT_MAX_PAGES: usize = 64;

    /// Default value of [`timeout`](Self::timeout): 30 seconds.
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

    /// Sets the most pages requested for one listing.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_tool::mcp::McpDiscoveryLimits;
    ///
    /// assert_eq!(McpDiscoveryLimits::default().with_max_pages(8).max_pages, 8);
    /// ```
    pub fn with_max_pages(mut self, max_pages: usize) -> Self {
        self.max_pages = max_pages;
        self
    }

    /// Sets the longest time one listing may take.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_tool::mcp::McpDiscoveryLimits;
    /// use std::time::Duration;
    ///
    /// let limits = McpDiscoveryLimits::default().with_timeout(Duration::from_secs(5));
    /// assert_eq!(limits.timeout, Duration::from_secs(5));
    /// ```
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

impl Default for McpDiscoveryLimits {
    fn default() -> Self {
        Self { max_pages: Self::DEFAULT_MAX_PAGES, timeout: Self::DEFAULT_TIMEOUT }
    }
}

/// Why a listing, or a request sent through the reconnect loop, failed.
#[derive(Debug)]
pub(crate) enum ListError {
    /// The request failed; reconnecting may help, depending on the error.
    Service(ServiceError),
    /// The server still returned a cursor after `pages` pages.
    PageLimit { method: &'static str, pages: usize },
    /// The server returned `cursor` a second time.
    RepeatedCursor { method: &'static str, cursor: String },
    /// The listing ran past `timeout`.
    Timeout { method: &'static str, timeout: Duration },
}

impl From<ServiceError> for ListError {
    fn from(error: ServiceError) -> Self {
        Self::Service(error)
    }
}

impl fmt::Display for ListError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Service(error) => write!(f, "{error}"),
            Self::PageLimit { method, pages } => write!(
                f,
                "the server still returned a nextCursor after {pages} pages of {method}; raise \
                 McpDiscoveryLimits::max_pages if the catalog really is that large"
            ),
            Self::RepeatedCursor { method, cursor } => write!(
                f,
                "the server returned cursor \"{}\" twice while paging {method}, so its \
                 pagination loops",
                escape_for_display(cursor)
            ),
            Self::Timeout { method, timeout } => write!(
                f,
                "{method} did not finish within {timeout:?}; raise McpDiscoveryLimits::timeout if \
                 the server is this slow"
            ),
        }
    }
}

/// Requests pages through `fetch` until the server stops returning a cursor.
///
/// `fetch` receives the cursor to send (`None` for the first page) and returns the
/// page's items with its `nextCursor`.
pub(crate) async fn list_pages<T, F, Fut>(
    method: &'static str,
    max_pages: usize,
    mut fetch: F,
) -> Result<Vec<T>, ListError>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: Future<Output = Result<(Vec<T>, Option<String>), ServiceError>>,
{
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    let mut cursor = None;
    let pages = max_pages.max(1);
    for _ in 0..pages {
        let (page, next) = fetch(cursor.take()).await?;
        items.extend(page);
        let Some(next) = next else {
            return Ok(items);
        };
        if !seen.insert(next.clone()) {
            return Err(ListError::RepeatedCursor { method, cursor: next });
        }
        cursor = Some(next);
    }
    Err(ListError::PageLimit { method, pages })
}

/// Lists every tool `peer` publishes, at most `max_pages` pages.
pub(crate) async fn list_tools(
    peer: &Peer<RoleClient>,
    max_pages: usize,
) -> Result<Vec<Tool>, ListError> {
    list_pages("tools/list", max_pages, |cursor| async move {
        let page =
            peer.list_tools(Some(PaginatedRequestParams::default().with_cursor(cursor))).await?;
        Ok((page.tools, page.next_cursor))
    })
    .await
}

/// Runs `listing` under `timeout`, reporting an overrun as [`ListError::Timeout`].
pub(crate) async fn within<T>(
    method: &'static str,
    timeout: Duration,
    listing: impl Future<Output = Result<T, ListError>>,
) -> Result<T, ListError> {
    tokio::time::timeout(timeout, listing)
        .await
        .unwrap_or(Err(ListError::Timeout { method, timeout }))
}

/// Longest tool name every built-in model provider accepts.
///
/// `SchemaAdapter::normalize_tool_name` truncates longer names, and nothing maps a
/// truncated name back, so the model's call to one would never reach the tool.
pub(crate) const MAX_TOOL_NAME_BYTES: usize = 64;

/// Longest prefix of a server-chosen string written to a log line or error message.
const MAX_DISPLAYED_BYTES: usize = 128;

/// Why no model can call a tool named `name`, if that is the case.
pub(crate) fn unroutable_name(name: &str) -> Option<String> {
    if name.is_empty() {
        Some("the name is empty".to_string())
    } else if name.chars().any(char::is_control) {
        Some("the name contains control characters".to_string())
    } else if name.len() > MAX_TOOL_NAME_BYTES {
        Some(format!(
            "the name is {} bytes; model providers accept at most {MAX_TOOL_NAME_BYTES}, and a \
             truncated name would not route back to the tool",
            name.len()
        ))
    } else {
        None
    }
}

/// `text` with control characters escaped and at most 128 bytes kept, for a log
/// line or error message.
pub(crate) fn escape_for_display(text: &str) -> String {
    let end = text.floor_char_boundary(MAX_DISPLAYED_BYTES);
    let mut shown: String = text[..end].escape_debug().collect();
    if end < text.len() {
        shown.push_str("...");
    }
    shown
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn page_through(
        pages: Vec<(u32, Option<&str>)>,
        max_pages: usize,
    ) -> Result<Vec<u32>, ListError> {
        let mut pages = pages.into_iter();
        list_pages("tools/list", max_pages, |_cursor| {
            let page = pages.next();
            async move {
                let (item, next) = page.expect("requested a page past the script");
                Ok((vec![item], next.map(str::to_string)))
            }
        })
        .await
    }

    #[tokio::test]
    async fn listing_follows_cursors_until_the_last_page() {
        let items = page_through(vec![(1, Some("a")), (2, Some("b")), (3, None)], 64).await;
        assert_eq!(items.unwrap(), vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn listing_stops_at_the_page_limit() {
        let pages = (0..5)
            .map(|page| (page, Some(["c0", "c1", "c2", "c3", "c4"][page as usize])))
            .collect();
        let error = page_through(pages, 3).await.unwrap_err();
        assert!(matches!(error, ListError::PageLimit { pages: 3, .. }), "{error}");
    }

    #[tokio::test]
    async fn listing_stops_on_a_repeated_cursor() {
        let error = page_through(vec![(1, Some("a")), (2, Some("b")), (3, Some("a"))], 64)
            .await
            .unwrap_err();
        assert!(
            matches!(&error, ListError::RepeatedCursor { cursor, .. } if cursor == "a"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_zero_page_limit_still_requests_one_page() {
        assert_eq!(page_through(vec![(1, None)], 0).await.unwrap(), vec![1]);
    }

    #[tokio::test]
    async fn a_listing_past_its_timeout_fails() {
        let error = within("prompts/list", Duration::from_millis(10), async {
            std::future::pending::<Result<(), ListError>>().await
        })
        .await
        .unwrap_err();
        assert!(matches!(error, ListError::Timeout { method: "prompts/list", .. }), "{error}");
    }

    #[test]
    fn names_no_provider_can_route_are_detected() {
        assert_eq!(unroutable_name("lookup_order"), None);
        assert_eq!(unroutable_name(&"a".repeat(64)), None);
        assert!(unroutable_name("").is_some());
        assert!(unroutable_name("line\nbreak").is_some());
        assert!(unroutable_name("red\u{1b}[31m").is_some());
        assert!(unroutable_name(&"a".repeat(65)).is_some());
    }

    #[test]
    fn displayed_text_is_escaped_and_capped() {
        assert_eq!(escape_for_display("line\nbreak\u{1b}[31m"), "line\\nbreak\\u{1b}[31m");
        let long = "é".repeat(100);
        let shown = escape_for_display(&long);
        assert!(shown.ends_with("..."));
        assert_eq!(shown.trim_end_matches("...").len(), 128);
    }
}
