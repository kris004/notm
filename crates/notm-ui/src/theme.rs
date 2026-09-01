use gtk::prelude::*;
use gtk4 as gtk;
use serde::Serialize;

use crate::model::ThemePreference;

const CSS_PROVIDER_COLOR_SCHEME_PROPERTY: &str = "prefers-color-scheme";
const SETTINGS_INTERFACE_COLOR_SCHEME_PROPERTY: &str = "gtk-interface-color-scheme";
const SETTINGS_THEME_NAME_PROPERTY: &str = "gtk-theme-name";
const SETTINGS_PREFER_DARK_PROPERTY: &str = "gtk-application-prefer-dark-theme";

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ThemeState {
    pub requested: ThemePreference,
    pub effective: ThemePreference,
    pub resolved_theme_bg_color: String,
    pub resolved_theme_bg_luminance: f32,
    pub gtk_theme_name: Option<String>,
    pub gtk_application_prefer_dark_theme: bool,
    pub provider_color_scheme: Option<String>,
    pub gtk_interface_color_scheme: Option<String>,
}

/// Install the application stylesheet and return its provider for theme updates.
pub fn install_css(display: &gtk::gdk::Display) -> gtk::CssProvider {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(css());
    gtk::style_context_add_provider_for_display(
        display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    provider
}

/// Apply an application theme preference without conflating `system` and `light`.
///
/// GTK 4.20 added `GtkSettings:gtk-interface-color-scheme`; it is selected
/// dynamically so the crate can retain its GTK 4.12 minimum. Forced modes also
/// use GTK's `Default` theme with the legacy dark-variant switch for the
/// GTK 4.12-4.18 fallback. Returning to `system` removes the display Settings
/// overrides so the built-in theme resumes following the session preference.
/// The application provider is also restored to its `default` scheme; notm's
/// CSS currently has no color-scheme media queries, so that provider value is
/// diagnostic only and is not treated as evidence of the rendered scheme.
pub fn apply_theme_preference(
    settings: &gtk::Settings,
    provider: &gtk::CssProvider,
    requested: ThemePreference,
) {
    match requested {
        ThemePreference::System => {
            settings.reset_property(SETTINGS_THEME_NAME_PROPERTY);
            settings.reset_property(SETTINGS_PREFER_DARK_PROPERTY);
            if settings
                .find_property(SETTINGS_INTERFACE_COLOR_SCHEME_PROPERTY)
                .is_some()
            {
                settings.reset_property(SETTINGS_INTERFACE_COLOR_SCHEME_PROPERTY);
            }
        }
        ThemePreference::Light => {
            settings.set_gtk_theme_name(Some("Default"));
            settings.set_gtk_application_prefer_dark_theme(false);
        }
        ThemePreference::Dark => {
            settings.set_gtk_theme_name(Some("Default"));
            settings.set_gtk_application_prefer_dark_theme(true);
        }
    }

    // Apply the modern override after the legacy fallback. Changing the legacy
    // properties can reload GTK's built-in theme provider, so doing this last is
    // required for a forced light mode to win over a dark GTK 4.20 preference.
    if requested != ThemePreference::System
        && settings
            .find_property(SETTINGS_INTERFACE_COLOR_SCHEME_PROPERTY)
            .is_some()
    {
        set_enum_property_by_nick(
            settings,
            SETTINGS_INTERFACE_COLOR_SCHEME_PROPERTY,
            requested.as_str(),
        );
    }

    let provider_scheme = provider_scheme_nick(requested);
    set_enum_property_by_nick(
        provider,
        CSS_PROVIDER_COLOR_SCHEME_PROPERTY,
        provider_scheme,
    );
}

fn provider_scheme_nick(requested: ThemePreference) -> &'static str {
    match requested {
        ThemePreference::System => "default",
        ThemePreference::Light => "light",
        ThemePreference::Dark => "dark",
    }
}

/// Resolve GTK's live `theme_bg_color` through a styled probe and report its
/// luminance alongside the raw GTK properties. This intentionally does not
/// treat the serialized request or provider enum as proof that a theme override
/// took effect. Callers should query again in system mode because the session
/// preference may change.
pub fn theme_state<W>(
    background_probe: &W,
    settings: &gtk::Settings,
    provider: &gtk::CssProvider,
    requested: ThemePreference,
) -> ThemeState
where
    W: IsA<gtk::Widget>,
{
    let background = background_probe.color();
    let luminance = relative_luminance(&background);
    ThemeState {
        requested,
        effective: if luminance < 0.5 {
            ThemePreference::Dark
        } else {
            ThemePreference::Light
        },
        resolved_theme_bg_color: background.to_string(),
        resolved_theme_bg_luminance: luminance,
        gtk_theme_name: settings.gtk_theme_name().map(|name| name.to_string()),
        gtk_application_prefer_dark_theme: settings.is_gtk_application_prefer_dark_theme(),
        provider_color_scheme: enum_property_nick(provider, CSS_PROVIDER_COLOR_SCHEME_PROPERTY),
        gtk_interface_color_scheme: enum_property_nick(
            settings,
            SETTINGS_INTERFACE_COLOR_SCHEME_PROPERTY,
        ),
    }
}

fn relative_luminance(color: &gtk::gdk::RGBA) -> f32 {
    fn linear(channel: f32) -> f32 {
        if channel <= 0.04045 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    }

    0.2126 * linear(color.red()) + 0.7152 * linear(color.green()) + 0.0722 * linear(color.blue())
}

pub fn gtk_settings_theme_preference(settings: &gtk::Settings) -> ThemePreference {
    match enum_property_nick(settings, SETTINGS_INTERFACE_COLOR_SCHEME_PROPERTY).as_deref() {
        Some("dark") => ThemePreference::Dark,
        Some("light") => ThemePreference::Light,
        _ if settings.is_gtk_application_prefer_dark_theme()
            || settings
                .gtk_theme_name()
                .is_some_and(|name| name.to_ascii_lowercase().contains("dark")) =>
        {
            ThemePreference::Dark
        }
        _ => ThemePreference::Light,
    }
}

fn set_enum_property_by_nick<O>(object: &O, property: &str, nick: &str) -> bool
where
    O: IsA<gtk::glib::Object>,
{
    let Some(specification) = object.find_property(property) else {
        return false;
    };
    let Some(class) = gtk::glib::EnumClass::with_type(specification.value_type()) else {
        return false;
    };
    let Some(value) = class.to_value_by_nick(nick) else {
        return false;
    };
    object.set_property_from_value(property, &value);
    true
}

fn enum_property_nick<O>(object: &O, property: &str) -> Option<String>
where
    O: IsA<gtk::glib::Object>,
{
    object.find_property(property)?;
    let value = object.property_value(property);
    let (_, enum_value) = gtk::glib::EnumValue::from_value(&value)?;
    Some(enum_value.nick().to_string())
}

pub fn css() -> &'static str {
    r#"
    .notm-theme-background-probe { color: @theme_bg_color; }
    .notm-tag { padding: 2px 6px; border-radius: 10px; background: alpha(currentColor, .10); }
    #notm-left-sidebar button,
    #notm-left-sidebar entry,
    #notm-left-sidebar-content button,
    #notm-left-sidebar-content entry {
        min-width: 96px;
    }
    #notm-pane-toggle-bar button {
        min-width: 36px;
        padding-left: 8px;
        padding-right: 8px;
        border-bottom: 3px solid transparent;
    }
    #notm-pane-toggle-bar button.notm-pane-visible {
        background-color: alpha(@theme_selected_bg_color, .11);
        box-shadow: inset 0 0 0 1px alpha(@theme_selected_bg_color, .22);
    }
    #notm-pane-toggle-bar button.notm-pane-visible:hover,
    #notm-pane-toggle-bar button.notm-pane-visible:focus,
    #notm-pane-toggle-bar button.notm-pane-visible:active {
        background-color: alpha(@theme_selected_bg_color, .16);
        box-shadow: inset 0 0 0 1px alpha(@theme_selected_bg_color, .36);
    }
    #notm-pane-toggle-bar button.notm-current-pane-button {
        border-bottom-color: alpha(@theme_selected_bg_color, .92);
    }
    #notm-pane-toggle-bar button.notm-current-pane-button:hover,
    #notm-pane-toggle-bar button.notm-current-pane-button:focus,
    #notm-pane-toggle-bar button.notm-current-pane-button:active {
        border-bottom-color: alpha(@theme_selected_bg_color, .95);
    }
    #notm-pane-toggle-bar button.notm-pane-hidden {
        border-bottom-color: transparent;
        box-shadow: none;
    }
    #notm-pane-toggle-bar button.notm-pane-hidden:hover,
    #notm-pane-toggle-bar button.notm-pane-hidden:focus,
    #notm-pane-toggle-bar button.notm-pane-hidden:active {
        border-bottom-color: transparent;
        box-shadow: none;
    }
    #notm-left-sidebar,
    #notm-thread-pane,
    #notm-message-pane {
        border: 1px solid transparent;
        border-radius: 8px;
    }
    #notm-left-sidebar .notm-keyboard-cursor,
    #notm-left-sidebar-content .notm-keyboard-cursor {
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
    #notm-thread-list .notm-thread-row {
        border-radius: 6px;
    }
    #notm-thread-list .notm-thread-row.unread {
        background: alpha(@theme_selected_bg_color, .09);
        box-shadow: inset 4px 0 0 alpha(@theme_selected_bg_color, .92);
    }
    #notm-thread-list row.unread label,
    #notm-thread-list .unread label { font-weight: 700; }
    #notm-thread-list row:selected .notm-thread-row.unread,
    #notm-thread-list .notm-thread-row.unread.notm-visual-selected,
    #notm-thread-list .notm-thread-row.unread.notm-multi-selected {
        background: alpha(@theme_selected_bg_color, .26);
    }
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
        background-color: @theme_bg_color;
    }
    #notm-message-header .notm-message-header-badge,
    #notm-message-header:backdrop .notm-message-header-badge,
    #notm-message-header .notm-message-header-badge:backdrop {
        padding: 2px 8px;
        border-radius: 999px;
        border: 1px solid alpha(currentColor, .16);
        background: alpha(@theme_fg_color, .06);
        color: @theme_fg_color;
        font-weight: 800;
    }
    #notm-message-header .notm-message-header-subject {
        font-size: 1.08em;
        font-weight: 800;
    }
    #notm-message-header .notm-message-header-key,
    #notm-message-header:backdrop .notm-message-header-key,
    #notm-message-header .notm-message-header-key:backdrop {
        color: @theme_selected_bg_color;
        font-weight: 800;
        opacity: 1;
    }
    #notm-message-header .notm-message-header-value,
    #notm-message-header:backdrop .notm-message-header-value,
    #notm-message-header .notm-message-header-value:backdrop {
        color: @theme_fg_color;
        opacity: 1;
    }
    #notm-command-palette {
        padding: 12px;
        border: 1px solid alpha(@theme_selected_bg_color, .70);
        border-radius: 10px;
        background-color: @theme_bg_color;
        color: @theme_fg_color;
        box-shadow: 0 12px 32px alpha(black, .45);
    }
    #notm-command-palette-entry {
        min-height: 32px;
        border: 1px solid alpha(@theme_selected_bg_color, .85);
        background-color: @theme_bg_color;
        color: @theme_fg_color;
        caret-color: @theme_fg_color;
    }
    #notm-command-palette-entry selection {
        background-color: @theme_selected_bg_color;
        color: @theme_selected_fg_color;
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

#[cfg(test)]
mod tests {
    use crate::model::ThemePreference;

    #[test]
    fn provider_color_scheme_mapping_uses_supported_enum_nicks() {
        assert_eq!(
            super::provider_scheme_nick(ThemePreference::System),
            "default"
        );
        assert_eq!(super::provider_scheme_nick(ThemePreference::Light), "light");
        assert_eq!(super::provider_scheme_nick(ThemePreference::Dark), "dark");
    }

    #[test]
    fn command_palette_css_defines_a_readable_surface() {
        let stylesheet = super::css();
        let panel = css_rule(stylesheet, "#notm-command-palette");
        for declaration in [
            "background-color: @theme_bg_color;",
            "color: @theme_fg_color;",
        ] {
            assert!(
                panel.contains(declaration),
                "command-palette panel CSS is missing {declaration:?}"
            );
        }

        let entry = css_rule(stylesheet, "#notm-command-palette-entry");
        for declaration in [
            "background-color: @theme_bg_color;",
            "color: @theme_fg_color;",
            "caret-color: @theme_fg_color;",
        ] {
            assert!(
                entry.contains(declaration),
                "command-palette entry CSS is missing {declaration:?}"
            );
        }

        let selection = css_rule(stylesheet, "#notm-command-palette-entry selection");
        for declaration in [
            "background-color: @theme_selected_bg_color;",
            "color: @theme_selected_fg_color;",
        ] {
            assert!(
                selection.contains(declaration),
                "command-palette selection CSS is missing {declaration:?}"
            );
        }
    }

    fn css_rule<'a>(stylesheet: &'a str, selector: &str) -> &'a str {
        let rule = stylesheet
            .split_once(selector)
            .unwrap_or_else(|| panic!("stylesheet is missing selector {selector:?}"))
            .1;
        rule.split_once('{')
            .unwrap_or_else(|| panic!("selector {selector:?} has no declaration block"))
            .1
            .split_once('}')
            .unwrap_or_else(|| panic!("selector {selector:?} has no closing brace"))
            .0
    }
}
