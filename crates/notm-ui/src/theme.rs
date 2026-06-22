pub fn css() -> &'static str {
    r#"
    .notm-tag { padding: 2px 6px; border-radius: 10px; background: alpha(currentColor, .10); }
    #notm-left-sidebar button,
    #notm-left-sidebar entry {
        min-width: 96px;
    }
    #notm-left-sidebar,
    #notm-thread-pane,
    #notm-message-pane {
        border: 1px solid transparent;
        border-radius: 8px;
    }
    .notm-active-pane {
        border-color: alpha(@theme_selected_bg_color, .55);
        background-color: alpha(@theme_selected_bg_color, .055);
    }
    .notm-active-pane .notm-keyboard-cursor {
        box-shadow: inset 0 0 0 2px alpha(@theme_selected_bg_color, .85);
        background-color: alpha(@theme_selected_bg_color, .12);
    }
    #notm-thread-list row.unread label { font-weight: 700; }
    #notm-compose-body,
    #notm-compose-body text,
    #notm-compose-body gutter {
        background-color: @theme_bg_color;
        color: @theme_fg_color;
    }
    #notm-compose-body text selection {
        background-color: @theme_selected_bg_color;
        color: @theme_selected_fg_color;
    }
    #notm-message-header {
        padding: 8px;
        border: 1px solid alpha(currentColor, .18);
        border-radius: 8px;
        background: alpha(currentColor, .06);
    }
    #notm-debug-panel { font-family: monospace; }
    "#
}
