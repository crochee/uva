//! The `web_search` tool: the model's way to look something up.
//!
//! The `web_fetch` tool can only read a URL the model already knows.
//! A model that cannot *search* is blind to anything that happened
//! after its training — and a headless harness has no human to hand
//! it a link. Both references have the tool: dsh's `web_search`
//! (`packages/web/tool-web/src/search.ts`) and omp's `web_search`.
//!
//! # The seam
//!
//! Where a search comes from is the deployment's business, not the
//! tool's: the tool holds one [`WebSearchProvider`] and calls it. The
//! shipped adapter talks to a **search endpoint that answers with
//! JSON** — the shape SearXNG and its compatible instances use:
//!
//! ```json
//! {"results": [{"title": "…", "url": "…", "content": "…"}]}
//! ```
//!
//! One shape, one adapter, and a deployment that wants another
//! provider writes one more [`WebSearchProvider`] — the tool does not
//! change.
//!
//! # No provider, no search
//!
//! A deployment that configures no endpoint still mounts the tool:
//! the call then answers with a clear refusal ("no web search
//! provider is configured") instead of pretending the web is empty.
//! dsh does the same — an enabled tool stays visible when its
//! provider is unavailable and fails with a structured error at
//! execution time.
//!
//! # The answer is bounded
//!
//! Ten results at most (dsh's `WEB_SEARCH_MAX_RESULTS`), each
//! summary at most 800 characters (dsh's
//! `WEB_SEARCH_SUMMARY_MAX_CHARS`), and the whole answer at most
//! [`MAX_ANSWER_BYTES`]. A search that returns a megabyte of text is
//! not a search result; it is a context-window accident.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use reqwest::Client;
use schemars_derive::JsonSchema;
use serde::Deserialize;
use uva_tool::{
    Context,
    ExecutionMode,
    RenderKind,
    Tool,
    ToolAnnotations,
    ToolOutput,
    ToolOutputDefinition,
};

/// The most results one search returns.
///
/// dsh's `WEB_SEARCH_MAX_RESULTS`; omp's search tools answer with a
/// comparable handful. More than ten is noise for a model deciding
/// what to read.
pub const MAX_SEARCH_RESULTS: usize = 10;

/// The most characters one result's summary carries.
///
/// dsh's `WEB_SEARCH_SUMMARY_MAX_CHARS`: the upper end of the range
/// that keeps a summary worth the context it costs.
pub const MAX_SUMMARY_CHARS: usize = 800;

/// The most bytes the whole answer carries.
///
/// The bound that keeps a search from eating a context window: the
/// results are truncated at the first result that would cross it, and
/// the answer says so.
pub const MAX_ANSWER_BYTES: usize = 16 * 1024;

/// How long the core waits for the search endpoint.
///
/// The same budget the fetch tool gives its request.
pub const SEARCH_TIMEOUT: Duration = Duration::from_secs(30);

/// One result a search returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    /// The result's title.
    pub title: String,
    /// Where the result lives.
    pub url: String,
    /// A short excerpt of the page.
    pub snippet: String,
}

/// Why a search did not answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchError {
    /// No provider is configured: the deployment has no search
    /// endpoint.
    Unconfigured,
    /// The endpoint could not be reached, or answered with something
    /// that is not a search result.
    Failed(String),
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SearchError::Unconfigured => {
                f.write_str("no web search provider is configured")
            }
            SearchError::Failed(message) => {
                write!(f, "the web search failed: {message}")
            }
        }
    }
}

/// Where a search comes from.
///
/// The seam the tool calls: the deployment installs one, and the tool
/// never speaks a transport itself.
#[async_trait]
pub trait WebSearchProvider: Send + Sync {
    /// Search for `query`, answering with at most `max_results`
    /// results.
    ///
    /// # Errors
    ///
    /// [`SearchError::Unconfigured`] when no endpoint is configured,
    /// [`SearchError::Failed`] when the endpoint did not answer.
    async fn search(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<SearchResult>, SearchError>;
}

/// The provider that answers nothing: the default a deployment has
/// when it configures no endpoint.
///
/// The tool stays mounted and says so — a search that silently
/// returned "nothing found" would be a lie.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoSearch;

#[async_trait]
impl WebSearchProvider for NoSearch {
    async fn search(
        &self,
        _query: &str,
        _max_results: usize,
    ) -> Result<Vec<SearchResult>, SearchError> {
        Err(SearchError::Unconfigured)
    }
}

/// The provider that talks to a JSON search endpoint.
///
/// The endpoint is the SearXNG-compatible shape: `GET {base}/search`
/// with `q` and `format=json`, answering `{"results": [{"title",
/// "url", "content"}]}`.
///
/// A deployment that needs a credential sends it as a header — the
/// header's name is the deployment's, the value is sent verbatim (a
/// deployment that wants `Bearer …` writes it that way).
#[derive(Debug, Clone)]
pub struct JsonSearchProvider {
    base_url: String,
    api_key: Option<String>,
    api_key_header: String,
    client: Client,
}

impl JsonSearchProvider {
    /// The provider over `base_url` (the search endpoint's base, with
    /// or without a trailing slash).
    ///
    /// # Panics
    ///
    /// Panics if the HTTP client cannot be built — the same rule the
    /// fetch tool follows: a client that cannot exist is a build-time
    /// fact, not a runtime failure.
    #[must_use]
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::with_credential(base_url, None, "Authorization")
    }

    /// The provider over `base_url`, sending `api_key` in the
    /// `api_key_header` header.
    #[must_use]
    pub fn with_credential(
        base_url: impl Into<String>,
        api_key: Option<String>,
        api_key_header: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key,
            api_key_header: api_key_header.into(),
            client: Client::builder()
                .timeout(SEARCH_TIMEOUT)
                .redirect(reqwest::redirect::Policy::limited(5))
                .build()
                .expect("the HTTP client builds"),
        }
    }

    /// The endpoint's base URL.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The URL one search asks: the endpoint's `/search`, with the
    /// query and the JSON format, keeping whatever the base URL
    /// already carried (a token in the query string, for one).
    fn search_url(&self, query: &str) -> String {
        let (base, carried) = match self.base_url.split_once('?') {
            Some((base, carried)) => (base, Some(carried)),
            None => (self.base_url.as_str(), None),
        };
        let mut url = format!("{}/search", base.trim_end_matches('/'));
        let mut separator = '?';
        if let Some(carried) = carried {
            url.push_str(&format!("?{carried}"));
            separator = '&';
        }
        url.push(separator);
        url.push_str(&format!("q={}&format=json", percent_encode(query)));
        url
    }
}

/// Percent-encode a query string for a URL query value.
///
/// Everything outside the unreserved set is encoded, so a query with
/// a space, a quote or an ampersand cannot break the request's
/// shape.
fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~' => {
                encoded.push(char::from(*byte));
            }
            other => {
                encoded.push('%');
                encoded.push_str(&format!("{other:02X}"));
            }
        }
    }
    encoded
}

/// The endpoint's answer: the shape the JSON search contract uses.
#[derive(Debug, Deserialize)]
struct JsonSearchResponse {
    #[serde(default)]
    results: Vec<JsonSearchResult>,
}

/// One result in the endpoint's answer.
#[derive(Debug, Deserialize)]
struct JsonSearchResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    /// The excerpt; the SearXNG shape calls it `content`.
    #[serde(default)]
    content: String,
}

#[async_trait]
impl WebSearchProvider for JsonSearchProvider {
    async fn search(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<SearchResult>, SearchError> {
        // The query is percent-encoded by hand: the URL crate is
        // already a dependency of this feature, and the HTTP client
        // is not asked for a feature it does not need elsewhere.
        let endpoint = self.search_url(query);
        let mut request = self.client.get(&endpoint);
        if let Some(key) = self.api_key.as_deref() {
            request = request.header(&self.api_key_header, key);
        }
        let response = request
            .send()
            .await
            .map_err(|error| SearchError::Failed(error.to_string()))?;
        if !response.status().is_success() {
            return Err(SearchError::Failed(format!(
                "the search endpoint answered {}",
                response.status()
            )));
        }
        let body: JsonSearchResponse =
            response.json().await.map_err(|error| {
                SearchError::Failed(format!(
                    "the search endpoint's answer was not a search result: \
                     {error}"
                ))
            })?;
        Ok(body
            .results
            .into_iter()
            .filter(|result| !result.url.is_empty())
            .take(max_results)
            .map(|result| SearchResult {
                title: result.title,
                url: result.url,
                snippet: result.content,
            })
            .collect())
    }
}

/// What the tool is asked.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(extend("additionalProperties" = false))]
struct WebSearchRequest {
    #[schemars(
        length(min = 1),
        description = "What to search for. A short, specific query."
    )]
    query: String,
    #[serde(default)]
    #[schemars(
        range(min = 1, max = 10),
        extend("default" = MAX_SEARCH_RESULTS),
        description = "How many results to return (1-10). Default: 10."
    )]
    max_results: Option<usize>,
}

/// `web_search` — look something up on the web.
pub struct WebSearchTool {
    provider: Arc<dyn WebSearchProvider>,
}

impl std::fmt::Debug for WebSearchTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebSearchTool").finish_non_exhaustive()
    }
}

impl WebSearchTool {
    /// The tool that asks `provider`.
    #[must_use]
    pub fn new(provider: Arc<dyn WebSearchProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web for current information. Give a short, specific \
         query; the answer lists the pages that matched, with their \
         titles, URLs and short excerpts. Use `web_fetch` to read a \
         page the search found."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::to_value(schemars::schema_for!(WebSearchRequest))
            .expect("WebSearchRequest schema is always serializable")
    }

    /// A search is a network round trip: it runs alone.
    fn mode(&self) -> ExecutionMode {
        ExecutionMode::Sequential
    }

    fn annotations(&self) -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
    }

    fn output_definition(&self) -> ToolOutputDefinition {
        ToolOutputDefinition::passthrough("web_search")
            .with_kind(RenderKind::Text)
            .with_title("Web search")
    }

    async fn call(
        &self,
        input: serde_json::Value,
        _context: &Context,
    ) -> ToolOutput {
        let request: WebSearchRequest = match serde_json::from_value(input) {
            Ok(request) => request,
            Err(error) => {
                return ToolOutput::error(format!(
                    "Invalid arguments: {error}"
                ));
            }
        };
        let query = request.query.trim();
        if query.is_empty() {
            return ToolOutput::error(
                "Invalid arguments: the query must not be blank",
            );
        }
        let max_results = request
            .max_results
            .unwrap_or(MAX_SEARCH_RESULTS)
            .clamp(1, MAX_SEARCH_RESULTS);
        match self.provider.search(query, max_results).await {
            Ok(results) => render(&results),
            Err(SearchError::Unconfigured) => ToolOutput::error(
                "no web search provider is configured; this deployment \
                 cannot search the web",
            ),
            Err(SearchError::Failed(message)) => {
                ToolOutput::error(format!("the web search failed: {message}"))
            }
        }
    }
}

/// The answer the model reads.
///
/// The results, one per line, with the title, the URL and a summary
/// cut to [`MAX_SUMMARY_CHARS`]. The whole answer is cut to
/// [`MAX_ANSWER_BYTES`] — and says so when it is.
fn render(results: &[SearchResult]) -> ToolOutput {
    if results.is_empty() {
        return ToolOutput::text("No results.");
    }
    let mut answer = String::new();
    let mut shown = 0usize;
    let mut truncated = false;
    for result in results {
        let snippet: String =
            result.snippet.chars().take(MAX_SUMMARY_CHARS).collect();
        let entry = format!("{}\n{}\n{}\n", result.title, result.url, snippet);
        if answer.len() + entry.len() > MAX_ANSWER_BYTES {
            truncated = true;
            break;
        }
        answer.push_str(&entry);
        shown += 1;
    }
    if truncated {
        answer.push_str(
            "\n(the answer was cut here; ask again with a narrower \
             query for the rest)\n",
        );
    }
    ToolOutput::text(answer)
        .with_metadata("results", serde_json::json!(shown))
        .with_metadata("truncated", serde_json::json!(truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A provider the tests script: it answers with a fixed list and
    /// remembers what it was asked.
    struct ScriptedSearch {
        answer: Vec<SearchResult>,
        asked: parking_lot::Mutex<Option<(String, usize)>>,
    }

    impl ScriptedSearch {
        fn new(answer: Vec<SearchResult>) -> Self {
            Self {
                answer,
                asked: parking_lot::Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl WebSearchProvider for ScriptedSearch {
        async fn search(
            &self,
            query: &str,
            max_results: usize,
        ) -> Result<Vec<SearchResult>, SearchError> {
            *self.asked.lock() = Some((query.to_string(), max_results));
            Ok(self.answer.clone())
        }
    }

    fn result(title: &str, url: &str, snippet: &str) -> SearchResult {
        SearchResult {
            title: title.to_string(),
            url: url.to_string(),
            snippet: snippet.to_string(),
        }
    }

    fn make_context() -> Context {
        Context::new("s1".to_string(), std::path::PathBuf::from("/tmp"))
    }

    fn first_text(out: &ToolOutput) -> String {
        out.content
            .iter()
            .find_map(|part| part.text().map(str::to_string))
            .unwrap_or_default()
    }

    #[test]
    fn the_tool_is_named_for_the_model() {
        let tool = WebSearchTool::new(Arc::new(NoSearch));
        assert_eq!(tool.name(), "web_search");
    }

    #[test]
    fn a_search_is_a_network_round_trip() {
        let tool = WebSearchTool::new(Arc::new(NoSearch));
        assert_eq!(tool.mode(), ExecutionMode::Sequential);
    }

    #[test]
    fn the_parameters_are_the_query() {
        let tool = WebSearchTool::new(Arc::new(NoSearch));
        let schema = tool.parameters();
        assert!(
            schema["properties"]["query"].is_object(),
            "the schema spells the query: {schema}"
        );
    }

    #[tokio::test]
    async fn the_results_are_the_answer() {
        let provider = ScriptedSearch::new(vec![
            result("Rust", "https://www.rust-lang.org", "A language."),
            result("Crates", "https://crates.io", "A registry."),
        ]);
        let tool = WebSearchTool::new(Arc::new(provider));
        let out = tool
            .call(serde_json::json!({ "query": "rust" }), &make_context())
            .await;
        assert!(out.is_error.is_none(), "{:?}", out.content);
        let text = first_text(&out);
        assert!(text.contains("https://www.rust-lang.org"), "{text}");
        assert!(text.contains("A registry."), "{text}");
    }

    #[tokio::test]
    async fn the_query_reaches_the_provider() {
        let provider = Arc::new(ScriptedSearch::new(vec![]));
        let asked = Arc::clone(&provider);
        let tool = WebSearchTool::new(provider);
        tool.call(
            serde_json::json!({ "query": "rust", "max_results": 3 }),
            &make_context(),
        )
        .await;
        let asked = asked.asked.lock().clone();
        assert_eq!(
            asked,
            Some(("rust".to_string(), 3)),
            "the provider is asked what the model asked: {asked:?}"
        );
    }

    #[tokio::test]
    async fn no_provider_fails_closed() {
        let tool = WebSearchTool::new(Arc::new(NoSearch));
        let out = tool
            .call(serde_json::json!({ "query": "rust" }), &make_context())
            .await;
        assert_eq!(out.is_error, Some(true));
        assert!(
            first_text(&out).contains("no web search provider"),
            "the refusal names the reason"
        );
    }

    #[tokio::test]
    async fn a_blank_query_never_reaches_the_provider() {
        let provider = Arc::new(ScriptedSearch::new(vec![]));
        let asked = Arc::clone(&provider);
        let tool = WebSearchTool::new(provider);
        let out = tool
            .call(serde_json::json!({ "query": "  " }), &make_context())
            .await;
        assert_eq!(out.is_error, Some(true));
        assert!(asked.asked.lock().is_none(), "a blank query never travels");
    }

    #[tokio::test]
    async fn an_empty_result_says_so() {
        let provider = ScriptedSearch::new(vec![]);
        let tool = WebSearchTool::new(Arc::new(provider));
        let out = tool
            .call(serde_json::json!({ "query": "rust" }), &make_context())
            .await;
        assert!(out.is_error.is_none());
        assert_eq!(first_text(&out), "No results.");
    }

    #[tokio::test]
    async fn the_result_count_is_bounded() {
        let provider = Arc::new(ScriptedSearch::new(vec![]));
        let asked = Arc::clone(&provider);
        let tool = WebSearchTool::new(provider);
        tool.call(
            serde_json::json!({ "query": "rust", "max_results": 99 }),
            &make_context(),
        )
        .await;
        let asked = asked.asked.lock().clone();
        assert_eq!(
            asked.as_ref().map(|(_, count)| *count),
            Some(MAX_SEARCH_RESULTS),
            "the bound is the tool's own: {asked:?}"
        );
    }

    #[test]
    fn the_search_url_carries_the_query_and_the_format() {
        let provider = JsonSearchProvider::new("http://search.example.com/");
        let url = provider.search_url("rust & cargo");
        assert_eq!(
            url,
            "http://search.example.com/search?q=rust%20%26%20cargo&format=json",
            "the query is percent-encoded: {url}"
        );
    }

    #[test]
    fn a_base_url_that_already_carries_a_query_keeps_it() {
        let provider =
            JsonSearchProvider::new("http://search.example.com/?token=x");
        let url = provider.search_url("rust");
        assert_eq!(
            url, "http://search.example.com/search?token=x&q=rust&format=json",
            "the carried query stays: {url}"
        );
    }

    #[test]
    fn a_summary_is_cut_to_its_cap() {
        let long = "x".repeat(MAX_SUMMARY_CHARS + 100);
        let out = render(&[result("t", "https://example.com", &long)]);
        let text = first_text(&out);
        assert!(
            text.len() < long.len(),
            "the summary is cut: {} bytes",
            text.len()
        );
    }

    #[test]
    fn a_truncated_answer_says_so() {
        let many: Vec<SearchResult> = (0..100)
            .map(|index| {
                result(
                    &format!("title {index}"),
                    "https://example.com",
                    &"x".repeat(MAX_SUMMARY_CHARS),
                )
            })
            .collect();
        let out = render(&many);
        let text = first_text(&out);
        assert!(
            text.contains("cut here"),
            "the answer says it was cut: {} bytes",
            text.len()
        );
        assert!(
            text.len() <= MAX_ANSWER_BYTES + 200,
            "the answer stays bounded: {} bytes",
            text.len()
        );
    }
}
