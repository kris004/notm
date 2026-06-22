pub fn css() -> &'static str {
    r#"
    .notm-tag { padding: 2px 6px; border-radius: 10px; background: alpha(currentColor, .10); }
    #notm-left-sidebar button,
    #notm-left-sidebar entry {
        min-width: 96px;
    }
    #notm-thread-list row.unread label { font-weight: 700; }
    #notm-message-header {
        padding: 8px;
        border: 1px solid alpha(currentColor, .18);
        border-radius: 8px;
        background: alpha(currentColor, .06);
    }
    #notm-debug-panel { font-family: monospace; }
    "#
}
