//! Reading web pages for the model.
//!
//! Only `http`/`https`. Hosts that resolve to loopback, private, link-local or
//! otherwise internal addresses are refused (so a page or a model can't make
//! 212 probe your router or the local inference server). Redirects are
//! limited and re-checked. Bodies are capped and converted to readable text.
//! The fetched text is untrusted data; the runtime labels it as such.

use std::net::IpAddr;
use std::time::Duration;

use futures::StreamExt;

pub type Client = reqwest::Client;

const MAX_BODY: usize = 3 * 1024 * 1024;
const MAX_TEXT: usize = 60_000;

pub fn client() -> Client {
    models::init_tls();
    reqwest::Client::builder()
        .user_agent(concat!("212/", env!("CARGO_PKG_VERSION"), " (terminal assistant; fetches pages on the user's request)"))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(25))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                attempt.error("too many redirects")
            } else if validate_url(attempt.url().as_str()).is_err() {
                attempt.error("redirect to a disallowed address")
            } else {
                attempt.follow()
            }
        }))
        .build()
        .expect("http client")
}

pub fn is_internal(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || o[0] == 0
                || (o[0] == 100 && (64..128).contains(&o[1])) // carrier-grade NAT
                || (o[0] == 198 && (o[1] == 18 || o[1] == 19)) // benchmarking
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_internal(IpAddr::V4(v4));
            }
            let s = v6.segments();
            v6.is_loopback() || v6.is_unspecified() || v6.is_multicast() || (s[0] & 0xfe00) == 0xfc00 || (s[0] & 0xffc0) == 0xfe80
        }
    }
}

/// Syntax and literal-address checks (DNS is checked again at fetch time).
pub fn validate_url(raw: &str) -> Result<reqwest::Url, String> {
    let raw = raw.trim().trim_start_matches('@');
    let url = reqwest::Url::parse(raw).map_err(|e| format!("invalid URL '{raw}': {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("only http and https URLs can be fetched (got {})", url.scheme()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("URLs with embedded credentials are not allowed".into());
    }
    let host = url.host_str().ok_or("URL has no host")?.trim_matches(['[', ']']).to_lowercase();
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") || host.ends_with(".internal") {
        return Err(format!("{host} is a local address; fetching it is not allowed"));
    }
    if let Ok(ip) = host.parse::<IpAddr>()
        && is_internal(ip)
    {
        return Err(format!("{host} is a private or local address; fetching it is not allowed"));
    }
    Ok(url)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub url: String,
    pub title: String,
    pub text: String,
}

impl Page {
    pub fn render(&self) -> String {
        format!("URL: {}\nTitle: {}\n\n{}", self.url, self.title, self.text)
    }
}

pub async fn fetch(client: &Client, raw: &str) -> Result<Page, String> {
    let url = validate_url(raw)?;
    let host = url.host_str().unwrap_or("").to_string();
    let port = url.port_or_known_default().unwrap_or(443);
    let addrs: Vec<_> =
        tokio::net::lookup_host((host.trim_matches(['[', ']']), port)).await.map_err(|e| format!("cannot resolve {host}: {e}"))?.collect();
    if addrs.is_empty() || addrs.iter().any(|a| is_internal(a.ip())) {
        return Err(format!("{host} resolves to a private or local address; fetching it is not allowed"));
    }
    let resp = client
        .get(url.clone())
        .header(reqwest::header::ACCEPT, "text/html,text/plain,text/markdown,application/json;q=0.9,*/*;q=0.5")
        .send()
        .await
        .map_err(|e| format!("request to {host} failed: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("{} returned HTTP {}", resp.url(), status.as_u16()));
    }
    let final_url = resp.url().to_string();
    let ctype = resp.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_lowercase();
    if ctype.contains("pdf")
        || ctype.starts_with("image/")
        || ctype.starts_with("audio/")
        || ctype.starts_with("video/")
        || ctype.contains("octet-stream")
    {
        return Err(format!("{final_url} is not a text document ({ctype}); only HTML and text can be read"));
    }
    let mut body = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        body.extend_from_slice(&chunk);
        if body.len() > MAX_BODY {
            break;
        }
    }
    let raw_text = String::from_utf8_lossy(&body).into_owned();
    let is_html = ctype.contains("html") || (ctype.is_empty() && raw_text.trim_start().starts_with('<'));
    let (title, mut text) = if is_html { html_to_text(&raw_text) } else { (String::new(), raw_text) };
    if text.chars().count() > MAX_TEXT {
        text = text.chars().take(MAX_TEXT).collect::<String>() + "\n… [page truncated]";
    }
    Ok(Page { url: final_url, title: if title.is_empty() { host } else { title }, text })
}

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let end = tail.find(';').filter(|e| *e <= 10);
        let decoded = end.and_then(|e| {
            let ent = &tail[1..e];
            let c = match ent {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" | "#39" => Some('\''),
                "nbsp" => Some(' '),
                "mdash" => Some('—'),
                "ndash" => Some('–'),
                "hellip" => Some('…'),
                "copy" => Some('©'),
                _ if ent.starts_with("#x") || ent.starts_with("#X") => u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32),
                _ if ent.starts_with('#') => ent[1..].parse::<u32>().ok().and_then(char::from_u32),
                _ => None,
            }?;
            Some((c, e + 1))
        });
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &tail[len..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Convert HTML to readable text: drops scripts/styles/navigation, keeps
/// headings, paragraphs, list items and preformatted blocks.
pub fn html_to_text(html: &str) -> (String, String) {
    let lower = html.to_lowercase();
    let title = lower
        .find("<title")
        .and_then(|s| lower[s..].find('>').map(|e| s + e + 1))
        .and_then(|s| lower[s..].find("</title>").map(|e| (s, s + e)))
        .map(|(s, e)| decode_entities(html[s..e].trim()))
        .unwrap_or_default();
    let skip = ["script", "style", "noscript", "svg", "head", "nav", "template", "iframe", "form"];
    let mut out = String::new();
    let mut i = 0;
    let bytes = html.as_bytes();
    let mut pre = 0usize;
    let mut skipping: Option<String> = None;
    while i < html.len() {
        if bytes[i] == b'<' {
            // Comments.
            if html[i..].starts_with("<!--") {
                i = html[i..].find("-->").map_or(html.len(), |e| i + e + 3);
                continue;
            }
            let Some(end) = html[i..].find('>') else { break };
            let tag_raw = &html[i + 1..i + end];
            i += end + 1;
            let closing = tag_raw.starts_with('/');
            let name: String =
                tag_raw.trim_start_matches('/').chars().take_while(|c| c.is_ascii_alphanumeric()).collect::<String>().to_lowercase();
            if let Some(s) = &skipping {
                if closing && name == *s {
                    skipping = None;
                }
                continue;
            }
            if !closing && skip.contains(&name.as_str()) && !tag_raw.ends_with('/') {
                skipping = Some(name);
                continue;
            }
            match name.as_str() {
                "br" => out.push('\n'),
                "pre" => {
                    if closing {
                        pre = pre.saturating_sub(1);
                        out.push_str("\n```\n");
                    } else {
                        pre += 1;
                        out.push_str("\n```\n");
                    }
                }
                "li" if !closing => out.push_str("\n- "),
                h if h.len() == 2 && h.starts_with('h') && h.as_bytes()[1].is_ascii_digit() => {
                    if closing {
                        out.push('\n');
                    } else {
                        let level = (h.as_bytes()[1] - b'0').clamp(1, 6) as usize;
                        out.push_str(&format!("\n\n{} ", "#".repeat(level)));
                    }
                }
                "p" | "div" | "section" | "article" | "main" | "tr" | "table" | "ul" | "ol" | "blockquote" | "header" | "footer" | "dt"
                | "dd" | "figure" | "aside" => out.push('\n'),
                "td" | "th" if !closing => out.push_str(" | "),
                "code" if pre == 0 => out.push('`'),
                _ => {}
            }
            continue;
        }
        let next = html[i..].find('<').map_or(html.len(), |n| i + n);
        if skipping.is_none() {
            let text = decode_entities(&html[i..next]);
            if pre > 0 {
                out.push_str(&text);
            } else {
                let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
                if !collapsed.is_empty() {
                    if text.starts_with(char::is_whitespace) && !out.ends_with(['\n', ' ']) {
                        out.push(' ');
                    }
                    out.push_str(&collapsed);
                    if text.ends_with(char::is_whitespace) {
                        out.push(' ');
                    }
                }
            }
        }
        i = next;
    }
    // Tidy: trim lines, at most one blank line in a row.
    let mut tidy = String::new();
    let mut blank = 0;
    let mut in_code = false;
    for line in out.lines() {
        let l = line.trim_end();
        if l.trim_start() == "```" {
            in_code = !in_code;
        }
        if l.trim().is_empty() && !in_code {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        tidy.push_str(if in_code { l } else { l.trim_start_matches(' ') });
        tidy.push('\n');
    }
    (title, tidy.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_internal_and_odd_urls() {
        for bad in [
            "http://localhost:8080",
            "http://127.0.0.1/",
            "http://10.0.0.1",
            "http://192.168.1.1/admin",
            "http://169.254.169.254/latest/meta-data",
            "http://[::1]/",
            "http://[fd00::1]/",
            "file:///etc/passwd",
            "ftp://example.com",
            "https://user:pw@example.com",
            "http://printer.local",
            "not a url",
        ] {
            assert!(validate_url(bad).is_err(), "{bad} should be refused");
        }
        assert!(validate_url("https://docs.rs/tokio").is_ok());
        assert!(validate_url("@https://example.com/a?b=c").is_ok());
        assert!(is_internal("100.100.1.1".parse().unwrap()));
        assert!(!is_internal("93.184.215.14".parse().unwrap()));
        assert!(is_internal("::ffff:127.0.0.1".parse().unwrap()));
    }

    #[test]
    fn converts_html_to_readable_text() {
        let html = r#"<!doctype html><html><head><title>Tokio &amp; Friends</title><style>p{color:red}</style>
            <script>alert('x')</script></head><body><nav><a href="/">Home</a></nav>
            <h1>Getting   started</h1><p>Add <code>tokio</code> to your
            Cargo.toml.</p><ul><li>fast</li><li>reliable &lt;3</li></ul>
            <pre>fn main() {
    run();
}</pre><!-- hidden --><p>Done&#33;</p></body></html>"#;
        let (title, text) = html_to_text(html);
        assert_eq!(title, "Tokio & Friends");
        assert!(text.contains("# Getting started"), "{text}");
        assert!(text.contains("Add `tokio` to your Cargo.toml."), "{text}");
        assert!(text.contains("- fast\n- reliable <3"), "{text}");
        assert!(text.contains("fn main() {\n    run();\n}"), "{text}");
        assert!(text.contains("Done!"));
        assert!(!text.contains("alert") && !text.contains("color:red") && !text.contains("Home") && !text.contains("hidden"));
    }

    #[tokio::test]
    async fn fetch_refuses_hosts_resolving_to_loopback() {
        // `localhost.` style tricks are caught by validate; resolution check covers DNS names.
        let c = client();
        let err = fetch(&c, "http://127.0.0.1.nip.io/").await.unwrap_err();
        assert!(err.contains("private or local") || err.contains("cannot resolve"), "{err}");
    }
}
