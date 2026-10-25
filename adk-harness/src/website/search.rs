//! `search_site`: the public partition's search over the current release's Markdown twins,
//! chunked with `adk-rag`'s Markdown chunker (spec 3.5 §7.10, A-I2).

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, RwLock};

use adk_core::{AdkError, Result, Tool, ToolContext, ToolEffect};
use adk_rag::{Chunker, Document, MarkdownChunker};
use adk_site::host::SiteHost;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::catalog::{PUBLIC_ORIGIN, SEARCH_SITE};

/// Bytes per chunk of a page.
const CHUNK_BYTES: usize = 1_200;
/// Bytes shared by consecutive chunks.
const CHUNK_OVERLAP: usize = 120;
/// Characters of a result's snippet.
const SNIPPET_CHARS: usize = 320;
/// Most results one search returns.
const MAX_RESULTS: usize = 10;

/// One result of a site search (spec 3.5 §7.10).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteHit {
    /// The page's URL path, such as `/posts/hello/`.
    pub url: String,
    /// The page's title.
    pub title: String,
    /// The matching passage.
    pub snippet: String,
    /// The relevance score, higher first.
    pub score: f32,
}

#[derive(Clone, Debug)]
struct Passage {
    url: String,
    title: String,
    text: String,
    terms: HashMap<String, u32>,
    len: usize,
}

fn terms_of(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.chars().count() >= 2)
        .map(str::to_lowercase)
        .collect()
}

/// The page a Markdown twin stands for: `/posts/hello/index.md` is `/posts/hello/`.
fn page_url(twin: &str) -> String {
    let path = format!("/{}", twin.trim_start_matches('/'));
    match path.strip_suffix("index.md") {
        Some(dir) => dir.to_string(),
        None => path.trim_end_matches(".md").to_string(),
    }
}

fn title_of(markdown: &str, url: &str) -> String {
    markdown
        .lines()
        .find_map(|line| line.strip_prefix("# "))
        .map(|title| title.trim().to_string())
        .unwrap_or_else(|| url.to_string())
}

/// The index `search_site` reads: the Markdown twins of the release the site serves, chunked
/// with `adk-rag`'s [`MarkdownChunker`] and ranked by BM25 (spec 3.5 §7.10). It holds the
/// site's published content only, never owner facts or conversations; reindex it when a new
/// release is published.
///
/// # Example
///
/// ```rust
/// use adk_harness::website::SiteSearch;
///
/// let search = SiteSearch::new();
/// search.index_documents([("/opening-hours/index.md".to_string(), "# Opening hours\n\nWe open at 7 every weekday.".to_string())]);
/// let hits = search.search("when do you open", 3);
/// assert_eq!(hits[0].url, "/opening-hours/");
/// assert_eq!(hits[0].title, "Opening hours");
/// ```
#[derive(Debug, Default)]
pub struct SiteSearch {
    passages: RwLock<Vec<Passage>>,
}

impl SiteSearch {
    /// An empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the index with `twins`: `(path of the Markdown twin, its text)`. Returns the
    /// number of passages.
    pub fn index_documents(&self, twins: impl IntoIterator<Item = (String, String)>) -> usize {
        let chunker = MarkdownChunker::new(CHUNK_BYTES, CHUNK_OVERLAP);
        let mut passages = Vec::new();
        for (path, markdown) in twins {
            let url = page_url(&path);
            let title = title_of(&markdown, &url);
            let document = Document {
                id: url.clone(),
                text: markdown,
                metadata: std::collections::HashMap::new(),
                source_uri: Some(url.clone()),
            };
            for chunk in chunker.chunk(&document) {
                let words = terms_of(&format!("{title} {}", chunk.text));
                let mut terms = HashMap::new();
                for word in &words {
                    *terms.entry(word.clone()).or_insert(0) += 1;
                }
                passages.push(Passage {
                    url: url.clone(),
                    title: title.clone(),
                    text: chunk.text,
                    terms,
                    len: words.len(),
                });
            }
        }
        let count = passages.len();
        *self.passages.write().unwrap_or_else(|e| e.into_inner()) = passages;
        count
    }

    /// Replaces the index with every `index.md` twin under `root`, a release's build
    /// directory.
    ///
    /// # Errors
    ///
    /// Returns the I/O error of a twin that cannot be read.
    pub async fn index_directory(&self, root: &Path) -> Result<usize> {
        let mut twins = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            let mut entries = tokio::fs::read_dir(&dir)
                .await
                .map_err(|e| AdkError::tool(format!("read {}: {e}", dir.display())))?;
            while let Some(entry) = entries
                .next_entry()
                .await
                .map_err(|e| AdkError::tool(format!("read {}: {e}", dir.display())))?
            {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().is_some_and(|ext| ext == "md") {
                    let text = tokio::fs::read_to_string(&path)
                        .await
                        .map_err(|e| AdkError::tool(format!("read {}: {e}", path.display())))?;
                    let relative = path.strip_prefix(root).unwrap_or(&path);
                    twins.push((relative.to_string_lossy().replace('\\', "/"), text));
                }
            }
        }
        twins.sort();
        Ok(self.index_documents(twins))
    }

    /// Replaces the index with the twins among `files` — a release manifest's paths — read
    /// from what `host` serves.
    ///
    /// # Errors
    ///
    /// Returns the host's error.
    pub async fn index_release<'a>(
        &self,
        host: &dyn SiteHost,
        files: impl IntoIterator<Item = &'a str>,
    ) -> Result<usize> {
        let mut twins = Vec::new();
        for path in files.into_iter().filter(|path| path.ends_with(".md")) {
            if let Some(bytes) = host.fetch(path).await.map_err(super::site::site_error)? {
                twins.push((path.to_string(), String::from_utf8_lossy(&bytes).into_owned()));
            }
        }
        Ok(self.index_documents(twins))
    }

    /// The best `limit` passages for `query`, one per page.
    pub fn search(&self, query: &str, limit: usize) -> Vec<SiteHit> {
        let passages = self.passages.read().unwrap_or_else(|e| e.into_inner());
        let query: Vec<String> = terms_of(query);
        if passages.is_empty() || query.is_empty() {
            return Vec::new();
        }
        let n = passages.len() as f32;
        let average = passages.iter().map(|p| p.len).sum::<usize>() as f32 / n;
        let (k1, b) = (1.2_f32, 0.75_f32);
        let mut best: BTreeMap<String, SiteHit> = BTreeMap::new();
        for passage in passages.iter() {
            let mut score = 0.0;
            for term in &query {
                let Some(tf) = passage.terms.get(term).map(|tf| *tf as f32) else { continue };
                let df = passages.iter().filter(|p| p.terms.contains_key(term)).count() as f32;
                let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
                score += idf * tf * (k1 + 1.0)
                    / (tf + k1 * (1.0 - b + b * passage.len as f32 / average.max(1.0)));
            }
            if score <= 0.0 {
                continue;
            }
            let better = best.get(&passage.url).is_none_or(|hit| hit.score < score);
            if better {
                best.insert(
                    passage.url.clone(),
                    SiteHit {
                        url: passage.url.clone(),
                        title: passage.title.clone(),
                        snippet: passage.text.chars().take(SNIPPET_CHARS).collect(),
                        score,
                    },
                );
            }
        }
        let mut hits: Vec<SiteHit> = best.into_values().collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.url.cmp(&b.url)));
        hits.truncate(limit);
        hits
    }
}

/// `search_site` (spec 3.5 §7.10): a read of the current release's content, the only
/// knowledge a public agent holds.
#[derive(Debug, Clone)]
pub struct SearchSiteTool {
    index: Arc<SiteSearch>,
}

impl SearchSiteTool {
    /// The tool over `index`.
    pub fn new(index: Arc<SiteSearch>) -> Self {
        Self { index }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    query: String,
    #[serde(default)]
    limit: Option<usize>,
}

#[async_trait]
impl Tool for SearchSiteTool {
    fn name(&self) -> &str {
        SEARCH_SITE
    }

    fn description(&self) -> &str {
        "Searches the site's published pages. Returns matching pages with their URL, title and a passage."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "What to look for, in the visitor's words" },
                "limit": { "type": "integer", "minimum": 1, "maximum": MAX_RESULTS }
            },
            "required": ["query"],
            "additionalProperties": false
        }))
    }

    fn origin(&self) -> &str {
        PUBLIC_ORIGIN
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let args: SearchArgs = serde_json::from_value(args)
            .map_err(|e| AdkError::tool(format!("search_site takes query and limit: {e}")))?;
        let hits = self.index.search(&args.query, args.limit.unwrap_or(5).clamp(1, MAX_RESULTS));
        Ok(json!({ "results": hits }))
    }
}
