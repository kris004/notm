pub fn css() -> &'static str {
    r#"
    .notm-tag { padding: 2px 6px; border-radius: 10px; background: alpha(currentColor, .10); }
    #notm-left-sidebar button,
    #notm-left-sidebar entry,
    #notm-left-sidebar-content button,
    #notm-left-sidebar-content entry {
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
    #notm-thread-list row.notm-visual-selected,
    #notm-thread-list .notm-visual-selected,
    #notm-thread-list row.notm-multi-selected,
    #notm-thread-list .notm-multi-selected {
        background-color: alpha(@theme_selected_bg_color, .28);
    }
    #notm-undo-tag-list row.notm-undo-selected {
        background-color: alpha(@theme_selected_bg_color, .30);
    }
    #notm-undo-tag-list row.notm-keyboard-cursor {
        box-shadow: inset 0 0 0 2px alpha(@theme_selected_bg_color, .85);
    }
    #notm-thread-list row.unread label,
    #notm-thread-list .unread label { font-weight: 700; }
    #notm-thread-list .notm-thread-number,
    #notm-thread-list .notm-thread-date {
        font-feature-settings: "tnum";
        opacity: .78;
    }
    #notm-thread-list .notm-thread-number {
        padding-right: 6px;
        border-right: 1px solid alpha(currentColor, .14);
    }
    #notm-thread-list .notm-thread-date {
        padding-right: 8px;
        border-right: 1px solid alpha(currentColor, .14);
    }
    #notm-compose-body,
    #notm-compose-body text,
    #notm-compose-body gutter {
        background-color: @theme_bg_color;
        color: @theme_fg_color;
        caret-color: @theme_fg_color;
    }
    #notm-compose-body text selection {
        background-color: @theme_selected_bg_color;
        color: @theme_selected_fg_color;
    }
    #notm-message-header {
        padding: 10px;
        border: 1px solid alpha(currentColor, .18);
        border-radius: 8px;
        background: alpha(currentColor, .06);
    }
    #notm-message-header .notm-message-header-badge {
        padding: 2px 8px;
        border-radius: 999px;
        background: alpha(@theme_selected_bg_color, .16);
        color: @theme_selected_bg_color;
        font-weight: 800;
    }
    #notm-message-header .notm-message-header-subject {
        font-size: 1.08em;
        font-weight: 800;
    }
    #notm-message-header .notm-message-header-key {
        color: @theme_selected_bg_color;
        font-weight: 800;
    }
    #notm-command-palette {
        padding: 0;
        border-radius: 8px;
        box-shadow: 0 8px 24px alpha(black, .35);
    }
    #notm-settings-dialog .notm-settings-section {
        font-size: 1.08em;
        font-weight: 800;
        color: @theme_selected_bg_color;
    }
    #notm-shortcuts-overlay .notm-settings-section {
        font-size: 1.08em;
        font-weight: 800;
        color: @theme_selected_bg_color;
    }
    #notm-settings-dialog .notm-settings-label {
        font-weight: 700;
    }
    #notm-shortcuts-overlay .notm-settings-label {
        font-weight: 700;
    }
    #notm-settings-dialog .notm-settings-note {
        padding: 8px;
        border-radius: 8px;
        background: alpha(@theme_selected_bg_color, .10);
    }
    #notm-settings-dialog .notm-settings-help {
        font-size: .92em;
    }
    #notm-search-suggestions-list,
    #notm-address-suggestions-list {
        padding: 2px;
        border: 1px solid alpha(@theme_selected_bg_color, .45);
        border-radius: 8px;
        background: @theme_bg_color;
        color: @theme_fg_color;
    }
    #notm-search-suggestions-list row,
    #notm-address-suggestions-list row {
        padding: 2px 4px;
        border-radius: 6px;
    }
    #notm-search-suggestions-list row:hover,
    #notm-search-suggestions-list row:selected,
    #notm-address-suggestions-list row:hover,
    #notm-address-suggestions-list row:selected {
        background: alpha(@theme_selected_bg_color, .35);
        color: @theme_selected_fg_color;
    }
    #notm-tag-command-row {
        padding: 2px;
        border-radius: 8px;
        background: alpha(@theme_selected_bg_color, .10);
    }
    #notm-tag-command-row entry {
        min-width: 180px;
    }
    #notm-debug-panel { font-family: monospace; }
    "#
}
