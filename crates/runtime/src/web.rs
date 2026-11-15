//! `@https://…` link ingestion in user messages.

/// URLs the user mentioned with `@`, in order, deduplicated.
pub fn mentioned_urls(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tok in text.split_whitespace() {
        let Some(u) = tok.strip_prefix('@') else { continue };
        if !(u.starts_with("http://") || u.starts_with("https://")) {
            continue;
        }
        let u = u.trim_end_matches([',', '.', ';', ')', '!', '?']).to_string();
        if !out.contains(&u) && out.len() < 5 {
            out.push(u);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_at_urls() {
        let v = super::mentioned_urls("read @https://a.dev/x, and https://plain.dev and @https://a.dev/x @file.rs");
        assert_eq!(v, vec!["https://a.dev/x".to_string()]);
    }
}
