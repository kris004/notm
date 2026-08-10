use std::collections::HashSet;

pub fn sanitize_html(html: &str) -> String {
    let mut builder = ammonia::Builder::default();
    builder
        .rm_tags(&[
            "script", "style", "iframe", "object", "embed", "link", "meta",
        ])
        .url_schemes(["http", "https", "mailto"].into_iter().collect())
        .add_tags(&["font"])
        .add_generic_attributes(&["style"])
        .add_tag_attributes("font", &["size"])
        .add_tag_attributes(
            "table",
            &[
                "width",
                "height",
                "border",
                "cellpadding",
                "cellspacing",
                "bgcolor",
            ],
        )
        .add_tag_attributes("tbody", &["width", "height", "bgcolor", "valign"])
        .add_tag_attributes("thead", &["width", "height", "bgcolor", "valign"])
        .add_tag_attributes("tfoot", &["width", "height", "bgcolor", "valign"])
        .add_tag_attributes("tr", &["width", "height", "bgcolor", "valign"])
        .add_tag_attributes("td", &["width", "height", "bgcolor", "valign"])
        .add_tag_attributes("th", &["width", "height", "bgcolor", "valign"])
        .attribute_filter(|element, attr, value| {
            if (attr == "style" && style_value_looks_dangerous(value))
                || (element == "font" && attr == "size" && !font_size_value_is_safe(value))
            {
                None
            } else {
                Some(value.into())
            }
        })
        .filter_style_properties(safe_email_style_properties());
    builder.clean(html).to_string()
}

pub fn html_to_safe_text(html: &str) -> String {
    let sanitized = sanitize_html(html);
    html2text::from_read(sanitized.as_bytes(), 100).unwrap_or(sanitized)
}

fn safe_email_style_properties() -> HashSet<&'static str> {
    HashSet::from([
        "background-color",
        "border",
        "border-bottom",
        "border-collapse",
        "border-color",
        "border-left",
        "border-radius",
        "border-right",
        "border-spacing",
        "border-style",
        "border-top",
        "border-width",
        "color",
        "display",
        "float",
        "font-family",
        "font-size",
        "font-style",
        "font-weight",
        "height",
        "letter-spacing",
        "line-height",
        "margin",
        "margin-bottom",
        "margin-left",
        "margin-right",
        "margin-top",
        "max-height",
        "max-width",
        "min-height",
        "min-width",
        "object-fit",
        "opacity",
        "outline",
        "overflow",
        "padding",
        "padding-bottom",
        "padding-left",
        "padding-right",
        "padding-top",
        "text-align",
        "text-decoration",
        "text-transform",
        "vertical-align",
        "white-space",
        "width",
    ])
}

fn style_value_looks_dangerous(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    [
        "url(",
        "expression(",
        "@import",
        "behavior:",
        "-moz-binding",
        "javascript:",
    ]
    .iter()
    .any(|needle| value.contains(needle))
}

fn font_size_value_is_safe(value: &str) -> bool {
    let value = value.trim();
    if let Some(rest) = value.strip_prefix(['+', '-']) {
        return matches!(rest, "1" | "2" | "3" | "4" | "5" | "6" | "7");
    }
    matches!(value, "1" | "2" | "3" | "4" | "5" | "6" | "7")
}

#[cfg(test)]
mod tests {
    use super::{html_to_safe_text, sanitize_html};

    #[test]
    fn html_to_text_handles_rowspan_with_empty_rows() {
        let rendered = html_to_safe_text(r#"<table><td rowspan="8"><tr><tr>"#);

        assert!(rendered.trim().is_empty());
    }

    #[test]
    fn preserves_safe_email_inline_layout_styles() {
        let sanitized = sanitize_html(
            r##"<table width="600" cellpadding="0" cellspacing="0" style="width:600px;background-color:#fff;border-collapse:collapse"><tr><td valign="top" style="padding:12px 16px;color:#123456;border-radius:8px;text-align:center">Hello</td></tr></table>"##,
        );

        assert!(sanitized.contains(r#"width="600""#));
        assert!(sanitized.contains(r#"cellpadding="0""#));
        assert!(sanitized.contains(r#"cellspacing="0""#));
        assert!(
            sanitized
                .contains("style=\"width:600px;background-color:#fff;border-collapse:collapse\"")
        );
        assert!(sanitized.contains(
            "style=\"padding:12px 16px;color:#123456;border-radius:8px;text-align:center\""
        ));
    }

    #[test]
    fn drops_dangerous_style_values_and_scripts() {
        let sanitized = sanitize_html(
            r#"<script>alert(1)</script><div style="padding:10px;background-image:url(https://tracker.example/pixel.png);color:red">Hello</div>"#,
        );

        assert!(!sanitized.contains("script"));
        assert!(!sanitized.contains("url("));
        assert!(!sanitized.contains("style="));
        assert!(sanitized.contains(">Hello</div>"));
    }

    #[test]
    fn preserves_legacy_font_size_markup_from_html_mail() {
        let sanitized = sanitize_html(
            r#"<div><font size="6">Large</font><font size="4">Medium</font><font size="1">Small</font></div>"#,
        );

        assert!(sanitized.contains(r#"<font size="6">Large</font>"#));
        assert!(sanitized.contains(r#"<font size="4">Medium</font>"#));
        assert!(sanitized.contains(r#"<font size="1">Small</font>"#));
    }

    #[test]
    fn drops_unbounded_legacy_font_size_values() {
        let sanitized = sanitize_html(r#"<font size="999999">Huge</font>"#);

        assert!(sanitized.contains("<font>Huge</font>"));
        assert!(!sanitized.contains("999999"));
    }
}
