#[derive(Debug, Clone)]
pub enum UiMessage {
    Search(String),
    OpenThread(usize),
    TagSelected {
        add: Vec<String>,
        remove: Vec<String>,
    },
    OpenCompose,
    SendCompose,
    ToggleDebug,
}
