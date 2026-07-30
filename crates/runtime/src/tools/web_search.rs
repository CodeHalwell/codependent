//! The `web.search` tool (PR C1 — agent capabilities).
//!
//! Thin argument-parsing, action-building, and rendering glue between the
//! agent loop and the `codypendent-integrations` search client, mirroring
//! [`super::github`]. The loop holds the client handle (a shared
//! `Arc<dyn SearchApi>`); this module only parses the model's arguments,
//! names the policy-visible [`ProposedAction`], and renders the outcome —
//! the loop's middleware runs the action through the policy engine and
//! [`crate::agent`] sanitizes the rendered text as untrusted evidence before
//! it enters the observation stream.
//!
//! A search is a network read to the Tavily API: [`ProposedAction::NetworkRequest`],
//! allowed when the endpoint is on the network policy's allow-list and the
//! mode permits network, never requiring approval.

// Single source of truth for the endpoint: the policy engine owns it (the
// executor admits exactly this string when a search client is configured), and
// the tool layer reuses it so the `NetworkRequest` destination can never drift
// out of sync with what the policy admits.
use codypendent_daemon::policy::TAVILY_API_ENDPOINT;
use codypendent_integrations::search::SearchOutcome;
use codypendent_protocol::ProposedAction;
use serde_json::Value;

/// The default number of results a search asks for when the model does not
/// say — small, because the rendered outcome is model context.
pub const DEFAULT_MAX_RESULTS: u32 = 5;
/// The most results a single search may ask for. Model-supplied values above
/// this are clamped, not rejected: the ceiling is a context budget, not a
/// contract the model should have to bounce off.
pub const MAX_RESULTS_LIMIT: u32 = 10;

/// The typed input for `web.search`.
pub struct WebSearchInput {
    /// The search query.
    pub query: String,
    /// How many sources to request (default [`DEFAULT_MAX_RESULTS`], clamped
    /// to [`MAX_RESULTS_LIMIT`]).
    pub max_results: u32,
}

/// Search the web (Tavily).
pub struct WebSearch;

impl WebSearch {
    /// The stable dotted tool name.
    pub const NAME: &'static str = "web.search";

    /// A web search is a network request to the Tavily endpoint (no approval).
    pub fn proposed_action() -> ProposedAction {
        ProposedAction::NetworkRequest {
            destination: TAVILY_API_ENDPOINT.to_string(),
        }
    }
}

/// Parse `web.search` arguments: a non-empty `query`, plus an optional
/// `max_results` (default [`DEFAULT_MAX_RESULTS`], clamped to
/// [`MAX_RESULTS_LIMIT`]; a non-integer is a legible error).
pub fn parse_web_search(args: &Value) -> Result<WebSearchInput, String> {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .ok_or("web.search requires a non-empty string `query`")?
        .to_string();
    let max_results = match args.get("max_results") {
        None | Some(Value::Null) => DEFAULT_MAX_RESULTS,
        Some(value) => {
            let raw = value
                .as_u64()
                .ok_or("web.search `max_results` must be a positive integer")?;
            u32::try_from(raw)
                .unwrap_or(MAX_RESULTS_LIMIT)
                .clamp(1, MAX_RESULTS_LIMIT)
        }
    };
    Ok(WebSearchInput { query, max_results })
}

/// Render a search outcome as a compact observation for the transcript: an
/// `answer: …` line when the backend synthesized one, then numbered
/// title/url/content entries. The caller sanitizes this text as untrusted
/// evidence before it enters the observation stream.
pub fn render_search_outcome(outcome: &SearchOutcome) -> String {
    let mut lines: Vec<String> = Vec::new();
    if let Some(answer) = &outcome.answer {
        lines.push(format!("answer: {answer}"));
    }
    for (index, result) in outcome.results.iter().enumerate() {
        lines.push(format!(
            "{}. {}\n   {}\n   {}",
            index + 1,
            result.title,
            result.url,
            result.content
        ));
    }
    if lines.is_empty() {
        "no results".to_string()
    } else {
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codypendent_integrations::search::SearchResult;
    use serde_json::json;

    #[test]
    fn max_results_defaults_to_five() {
        let input = parse_web_search(&json!({"query": "q"})).unwrap();
        assert_eq!(input.query, "q");
        assert_eq!(input.max_results, DEFAULT_MAX_RESULTS);
    }

    #[test]
    fn max_results_is_clamped_to_ten() {
        let input = parse_web_search(&json!({"query": "q", "max_results": 50})).unwrap();
        assert_eq!(input.max_results, MAX_RESULTS_LIMIT);
        let at_limit = parse_web_search(&json!({"query": "q", "max_results": 10})).unwrap();
        assert_eq!(at_limit.max_results, MAX_RESULTS_LIMIT);
    }

    #[test]
    fn parse_rejects_a_missing_or_blank_query() {
        assert!(parse_web_search(&json!({})).is_err());
        assert!(parse_web_search(&json!({"query": "   "})).is_err());
        assert!(parse_web_search(&json!({"query": 42})).is_err());
    }

    #[test]
    fn parse_rejects_a_non_integer_max_results() {
        assert!(parse_web_search(&json!({"query": "q", "max_results": "lots"})).is_err());
        assert!(parse_web_search(&json!({"query": "q", "max_results": 2.5})).is_err());
    }

    #[test]
    fn render_shows_the_answer_then_numbered_results() {
        let outcome = SearchOutcome {
            answer: Some("the answer".to_string()),
            results: vec![
                SearchResult {
                    title: "First".to_string(),
                    url: "https://a.test".to_string(),
                    content: "alpha".to_string(),
                },
                SearchResult {
                    title: "Second".to_string(),
                    url: "https://b.test".to_string(),
                    content: "beta".to_string(),
                },
            ],
        };
        let rendered = render_search_outcome(&outcome);
        assert_eq!(
            rendered,
            "answer: the answer\n\
             1. First\n   https://a.test\n   alpha\n\
             2. Second\n   https://b.test\n   beta"
        );
    }

    #[test]
    fn render_an_empty_outcome_says_so() {
        let rendered = render_search_outcome(&SearchOutcome {
            answer: None,
            results: Vec::new(),
        });
        assert_eq!(rendered, "no results");
    }
}
