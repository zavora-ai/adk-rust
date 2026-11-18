//! The `browser_state` content block of browser use toolset results.

use serde::{Deserialize, Serialize};

/// Tab state reported on a browser use member result.
///
/// `tabs` is the full inventory after the call; whenever it is not empty, exactly one tab
/// is `active`. A successful `new_tab`, `list_tabs`, `switch_tab` or `close_tab` result is
/// exactly one of these blocks; any other successful result may add one beside its text or
/// image. An error result never carries one. The API renders the block into the text
/// Claude reads.
///
/// # Example
///
/// ```rust
/// use adk_anthropic::{BrowserStateBlock, BrowserStateChange, BrowserTab, Content};
/// use serde_json::json;
///
/// let block = BrowserStateBlock::new(vec![
///     BrowserTab::new("tab-1", "Docs", "https://example.com/docs"),
///     BrowserTab::new("tab-2", "", "about:blank").active(),
/// ])
/// .with_state_change(BrowserStateChange::TabOpened { tab_id: "tab-2".into() });
/// assert_eq!(
///     serde_json::to_value(Content::BrowserState(block)).unwrap(),
///     json!({
///         "type": "browser_state",
///         "tabs": [
///             { "tab_id": "tab-1", "title": "Docs", "url": "https://example.com/docs" },
///             { "tab_id": "tab-2", "title": "", "url": "about:blank", "active": true }
///         ],
///         "state_changes": [{ "type": "tab_opened", "tab_id": "tab-2" }]
///     })
/// );
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserStateBlock {
    /// Every open tab after the call.
    pub tabs: Vec<BrowserTab>,

    /// Side effects of the call; omitted, never empty, when there are none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_changes: Option<Vec<BrowserStateChange>>,
}

impl BrowserStateBlock {
    /// A block listing `tabs` with no state changes.
    pub fn new(tabs: Vec<BrowserTab>) -> Self {
        Self { tabs, state_changes: None }
    }

    /// Adds a state change.
    pub fn with_state_change(mut self, change: BrowserStateChange) -> Self {
        self.state_changes.get_or_insert_with(Vec::new).push(change);
        self
    }

    /// The active tab, if any.
    pub fn active_tab(&self) -> Option<&BrowserTab> {
        self.tabs.iter().find(|tab| tab.active)
    }
}

/// One open tab in a [`BrowserStateBlock`].
///
/// `tab_id`, `title` and `url` are each at most 4,096 characters with no control
/// characters; `tab_id` is non-empty and is not reused while a tab with it is open.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserTab {
    /// The identifier the application assigned.
    pub tab_id: String,
    /// The page title, unescaped; the API escapes it when rendering.
    #[serde(default)]
    pub title: String,
    /// The page URL, sanitised; the API renders it verbatim.
    #[serde(default)]
    pub url: String,
    /// Whether this is the active tab.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub active: bool,
}

impl BrowserTab {
    /// An inactive tab.
    pub fn new(
        tab_id: impl Into<String>,
        title: impl Into<String>,
        url: impl Into<String>,
    ) -> Self {
        Self { tab_id: tab_id.into(), title: title.into(), url: url.into(), active: false }
    }

    /// Marks the tab active.
    pub fn active(mut self) -> Self {
        self.active = true;
        self
    }
}

/// A side effect of a browser use call, reported in `state_changes`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum BrowserStateChange {
    /// The call opened a tab that is still open; its `tab_id` is in `tabs`.
    TabOpened {
        /// The opened tab.
        tab_id: String,
    },
    /// A download started during the call.
    DownloadStarted {
        /// The identifier the application assigned to the download.
        download_id: String,
        /// The final URL the file is served from.
        url: String,
    },
    /// A download completed during the call.
    DownloadCompleted {
        /// The download's identifier.
        download_id: String,
        /// The final URL the file was served from.
        url: String,
        /// Where another tool in the same environment can read the file.
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        /// The file's size.
        #[serde(skip_serializing_if = "Option::is_none")]
        size_bytes: Option<u64>,
    },
    /// A download failed or was cancelled during the call.
    DownloadFailed {
        /// The download's identifier.
        download_id: String,
        /// The final URL the file was served from.
        url: String,
        /// Why, when the browser says.
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn documented_download_block_round_trips() {
        let documented = json!({
            "tabs": [
                { "tab_id": "tab-1", "title": "Documentation", "url": "https://example.com/docs" },
                {
                    "tab_id": "tab-2",
                    "title": "Pricing",
                    "url": "https://example.com/pricing",
                    "active": true
                }
            ],
            "state_changes": [
                {
                    "type": "download_completed",
                    "download_id": "dl-1",
                    "url": "https://example.com/pricing/price-list.csv",
                    "path": "/home/user/downloads/price-list.csv",
                    "size_bytes": 48213
                }
            ]
        });
        let block: BrowserStateBlock = serde_json::from_value(documented.clone()).unwrap();
        assert_eq!(block.active_tab().map(|tab| tab.tab_id.as_str()), Some("tab-2"));
        assert_eq!(serde_json::to_value(&block).unwrap(), documented);
    }

    #[test]
    fn no_state_changes_omits_the_field() {
        let block = BrowserStateBlock::new(vec![BrowserTab::new("t", "", "about:blank").active()]);
        assert_eq!(
            serde_json::to_value(&block).unwrap(),
            json!({ "tabs": [{ "tab_id": "t", "title": "", "url": "about:blank", "active": true }] })
        );
    }
}
