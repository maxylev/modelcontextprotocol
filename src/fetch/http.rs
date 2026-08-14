use std::time::Duration;

use url::Url;

pub const DEFAULT_USER_AGENT_AUTONOMOUS: &str =
    "ModelContextProtocol/1.0 (Autonomous; +https://github.com/modelcontextprotocol/servers)";
pub const DEFAULT_USER_AGENT_MANUAL: &str =
    "ModelContextProtocol/1.0 (User-Specified; +https://github.com/modelcontextprotocol/servers)";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Install the `ring` rustls crypto provider, required by reqwest's
/// `rustls-no-provider` feature. Safe to call repeatedly; only the first
/// install in a process takes effect.
fn install_rustls_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Thin wrapper around an HTTP client with a fixed request timeout.
pub struct FetchClient {
    client: reqwest::Client,
}

impl FetchClient {
    pub fn new(proxy_url: Option<&str>) -> Result<Self, String> {
        install_rustls_provider();
        let mut builder = reqwest::Client::builder();
        if let Some(proxy) = proxy_url {
            let proxy = reqwest::Proxy::all(proxy)
                .map_err(|e| format!("Invalid proxy URL {proxy:?}: {e}"))?;
            builder = builder.proxy(proxy);
        }
        let client = builder
            .build()
            .map_err(|e| format!("Failed to build HTTP client: {e}"))?;
        Ok(Self { client })
    }

    /// GET a URL with the given User-Agent. Returns (body, content-type, status).
    pub async fn get(&self, url: &str, user_agent: &str) -> Result<(String, String, u16), String> {
        let response = self
            .client
            .get(url)
            .header(reqwest::header::USER_AGENT, user_agent)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|e| format!("Failed to fetch {url}: {e:?}"))?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = response
            .text()
            .await
            .map_err(|e| format!("Failed to read response body from {url}: {e}"))?;
        Ok((body, content_type, status))
    }
}

/// Build the robots.txt URL for a website URL.
pub fn robots_txt_url(url: &str) -> Result<String, String> {
    let parsed = Url::parse(url).map_err(|e| format!("Invalid URL {url}: {e}"))?;
    Ok(format!(
        "{}://{}/robots.txt",
        parsed.scheme(),
        parsed.authority()
    ))
}

/// Check that the given user agent is allowed to autonomously fetch `url`,
/// according to the site's robots.txt. Returns an error describing the
/// rejection. Mirrors the reference Python server's behavior:
///  - robots.txt unreachable -> error
///  - 401/403 from robots.txt -> error (assume fetching is not allowed)
///  - other 4xx from robots.txt -> allowed
///  - otherwise parse the robots.txt and match `url` against the user agent
pub async fn check_may_fetch(
    client: &FetchClient,
    url: &str,
    user_agent: &str,
) -> Result<(), String> {
    let robots_url = robots_txt_url(url)?;
    let (body, _content_type, status) = client.get(&robots_url, user_agent).await.map_err(|e| {
        format!("Failed to fetch robots.txt {robots_url} due to a connection issue: {e}")
    })?;

    if status == 401 || status == 403 {
        return Err(format!(
            "When fetching robots.txt ({robots_url}), received status {status} so assuming \
             that autonomous fetching is not allowed, the user can try manually fetching \
             by using the fetch prompt"
        ));
    }
    if (400..500).contains(&status) {
        return Ok(());
    }

    // Strip comments before parsing, like the reference implementation.
    let processed: String = body
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");

    let mut matcher = robotstxt::DefaultMatcher::default();
    if !matcher.one_agent_allowed_by_robots(&processed, user_agent, url) {
        return Err(format!(
            "The sites robots.txt ({robots_url}), specifies that autonomous fetching of \
             this page is not allowed, \
             <useragent>{user_agent}</useragent>\n\
             <url>{url}</url>\n\
             <robots>\n{body}\n</robots>\n\
             The assistant must let the user know that it failed to view the page. \
             The assistant may provide further guidance based on the above information. \
             The assistant can tell the user that they can try manually fetching the page \
             by using the fetch prompt within their UI."
        ));
    }
    Ok(())
}

/// Fetch a URL and return content ready for the model, plus an optional
/// status prefix (used when the body could not be simplified to markdown).
pub async fn fetch_url(
    client: &FetchClient,
    url: &str,
    user_agent: &str,
    force_raw: bool,
) -> Result<(String, String), String> {
    let (page_raw, content_type, status) = client.get(url, user_agent).await?;
    if status >= 400 {
        return Err(format!("Failed to fetch {url} - status code {status}"));
    }

    let first_100: String = page_raw.chars().take(100).collect();
    let is_page_html = first_100.contains("<html")
        || content_type.contains("text/html")
        || content_type.is_empty();

    if is_page_html && !force_raw {
        Ok((extract_content_from_html(&page_raw), String::new()))
    } else {
        Ok((
            page_raw,
            format!(
                "Content type {content_type} cannot be simplified to markdown, but here is \
                 the raw content:\n"
            ),
        ))
    }
}

/// Convert HTML to a simplified markdown form. Returns an error marker when
/// nothing could be extracted.
pub fn extract_content_from_html(html: &str) -> String {
    let content = parse_html(html);
    let content = content.trim();
    if content.is_empty() {
        "<error>Page failed to be simplified from HTML</error>".to_string()
    } else {
        content.to_string()
    }
}

/// HTML→markdown conversion that drops non-content tags (`style`,
/// `script`, ...) instead of leaking their raw text into the output.
fn parse_html(html: &str) -> String {
    let mut custom: std::collections::HashMap<String, Box<dyn html2md::TagHandlerFactory>> =
        std::collections::HashMap::new();
    for tag in ["style", "script", "noscript", "template"] {
        custom.insert(tag.to_string(), Box::new(SuppressFactory));
    }
    html2md::parse_html_custom(html, &custom)
}

/// Renders a tag and all of its descendants as nothing.
#[derive(Default)]
struct SuppressHandler;

impl html2md::TagHandler for SuppressHandler {
    fn handle(&mut self, _tag: &html2md::Handle, _printer: &mut html2md::StructuredPrinter) {}
    fn after_handle(&mut self, _printer: &mut html2md::StructuredPrinter) {}
    fn skip_descendants(&self) -> bool {
        true
    }
}

#[derive(Default)]
struct SuppressFactory;

impl html2md::TagHandlerFactory for SuppressFactory {
    fn instantiate(&self) -> Box<dyn html2md::TagHandler> {
        Box::new(SuppressHandler)
    }
}

/// Apply `max_length`/`start_index` truncation semantics, mirroring the
/// reference server: character-based indexing, an error marker when no more
/// content is available, and a hint telling the model how to continue when
/// the content was truncated.
pub fn truncate(content: &str, start_index: usize, max_length: usize) -> String {
    let chars: Vec<char> = content.chars().collect();
    if start_index >= chars.len() {
        return "<error>No more content available.</error>".to_string();
    }
    let end = (start_index + max_length).min(chars.len());
    let actual = end - start_index;
    let mut out: String = chars[start_index..end].iter().collect();
    if actual == max_length && end < chars.len() {
        let next_start = start_index + actual;
        out.push_str(&format!(
            "\n\n<error>Content truncated. Call the fetch tool with a start_index of \
             {next_start} to get more content.</error>"
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn robots_url_construction() {
        assert_eq!(
            robots_txt_url("https://example.com/page").unwrap(),
            "https://example.com/robots.txt"
        );
        assert_eq!(
            robots_txt_url("https://example.com/some/deep/path/page.html").unwrap(),
            "https://example.com/robots.txt"
        );
        assert_eq!(
            robots_txt_url("https://example.com/page?foo=bar&baz=qux").unwrap(),
            "https://example.com/robots.txt"
        );
        assert_eq!(
            robots_txt_url("https://example.com:8080/page").unwrap(),
            "https://example.com:8080/robots.txt"
        );
        assert_eq!(
            robots_txt_url("https://example.com/page#section").unwrap(),
            "https://example.com/robots.txt"
        );
        assert_eq!(
            robots_txt_url("http://example.com/page").unwrap(),
            "http://example.com/robots.txt"
        );
    }

    #[test]
    fn robots_url_rejects_garbage() {
        assert!(robots_txt_url("not a url").is_err());
    }

    #[test]
    fn extract_simple_html() {
        let html = "<html><body><article><h1>Hello World</h1><p>This is a test paragraph.</p></article></body></html>";
        let result = extract_content_from_html(html);
        assert!(result.contains("Hello World"), "got: {result}");
        assert!(result.contains("test paragraph"), "got: {result}");
    }

    #[test]
    fn extract_html_with_link() {
        let html = "<html><body><article><p>Visit <a href=\"https://example.com\">Example</a> for more.</p></article></body></html>";
        let result = extract_content_from_html(html);
        assert!(result.contains("Example"), "got: {result}");
    }

    #[test]
    fn extract_empty_returns_error() {
        let result = extract_content_from_html("");
        assert!(result.contains("<error>"));
    }

    #[test]
    fn extract_drops_style_and_script_noise() {
        let html = "<html><head><style>body{color:red;background:#eee}</style>\
        <script>console.log('x')</script></head>\
        <body><article><h1>Real Title</h1><p>Real content.</p></article></body></html>";
        let result = extract_content_from_html(html);
        assert!(result.contains("Real Title"), "got: {result}");
        assert!(result.contains("Real content"), "got: {result}");
        assert!(!result.contains("color:red"), "style leaked: {result}");
        assert!(!result.contains("console.log"), "script leaked: {result}");
    }

    #[test]
    fn truncate_basic_slice() {
        // Content fully consumed: no continuation hint.
        assert_eq!(truncate("abc", 0, 3), "abc");
        // Partial consumption with more content: hint added.
        let result = truncate("abcdef", 0, 3);
        assert!(result.starts_with("abc"));
        assert!(result.contains("start_index of 3"));
        let result = truncate("abcdef", 2, 2);
        assert!(result.starts_with("cd"));
        assert!(result.contains("start_index of 4"));
    }

    #[test]
    fn truncate_with_continuation_hint() {
        let result = truncate("abcdefgh", 0, 5);
        assert!(result.starts_with("abcde"));
        assert!(result.contains("Content truncated"));
        assert!(result.contains("start_index of 5"));
    }

    #[test]
    fn truncate_exact_boundary_has_no_hint() {
        let result = truncate("abcdef", 0, 6);
        assert_eq!(result, "abcdef");
    }

    #[test]
    fn truncate_past_end_returns_error() {
        assert_eq!(
            truncate("abcdef", 6, 5),
            "<error>No more content available.</error>"
        );
        assert_eq!(
            truncate("abcdef", 100, 5),
            "<error>No more content available.</error>"
        );
    }

    #[test]
    fn truncate_is_character_indexed() {
        // Multibyte characters: "héllo" is 5 chars but more bytes.
        let result = truncate("héllo wörld", 0, 5);
        assert!(result.starts_with("héllo"));
        assert!(result.contains("start_index of 5"));
        // Re-fetch from the continuation point.
        let result = truncate("héllo wörld", 6, 5);
        assert_eq!(result, "wörld");
    }
}
