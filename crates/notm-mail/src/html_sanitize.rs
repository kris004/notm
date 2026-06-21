pub fn sanitize_html(html: &str) -> String {
    ammonia::Builder::default()
        .rm_tags(&[
            "script", "style", "iframe", "object", "embed", "link", "meta",
        ])
        .url_schemes(["http", "https", "mailto"].into_iter().collect())
        .clean(html)
        .to_string()
}

pub fn html_to_safe_text(html: &str) -> String {
    let sanitized = sanitize_html(html);
    html2text::from_read(sanitized.as_bytes(), 100).unwrap_or(sanitized)
}
