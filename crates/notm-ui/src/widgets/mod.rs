pub mod attachments;
pub mod command_palette;
pub mod composer;
pub mod debug_panel;
pub mod link_hints;
pub mod message_view;
pub mod saved_searches;
pub mod search_bar;
pub mod settings;
pub mod standalone_message;
pub mod status_bar;
pub mod tag_editor;
pub mod thread_list;
pub mod thread_view;

pub(crate) fn vim_viewport_scroll_lines(
    key: gtk4::gdk::Key,
    modifiers: gtk4::gdk::ModifierType,
) -> Option<f64> {
    if !modifiers.contains(gtk4::gdk::ModifierType::CONTROL_MASK) {
        return None;
    }
    if key == gtk4::gdk::Key::e || key == gtk4::gdk::Key::E {
        Some(1.0)
    } else if key == gtk4::gdk::Key::y || key == gtk4::gdk::Key::Y {
        Some(-1.0)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_e_and_y_scroll_the_viewport_without_claiming_plain_keys() {
        let none = gtk4::gdk::ModifierType::empty();
        let control = gtk4::gdk::ModifierType::CONTROL_MASK;
        assert_eq!(
            vim_viewport_scroll_lines(gtk4::gdk::Key::e, control),
            Some(1.0)
        );
        assert_eq!(
            vim_viewport_scroll_lines(gtk4::gdk::Key::E, control),
            Some(1.0)
        );
        assert_eq!(
            vim_viewport_scroll_lines(gtk4::gdk::Key::y, control),
            Some(-1.0)
        );
        assert_eq!(
            vim_viewport_scroll_lines(gtk4::gdk::Key::Y, control),
            Some(-1.0)
        );
        assert_eq!(vim_viewport_scroll_lines(gtk4::gdk::Key::e, none), None);
        assert_eq!(vim_viewport_scroll_lines(gtk4::gdk::Key::j, control), None);
    }
}
