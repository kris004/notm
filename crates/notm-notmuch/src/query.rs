use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub enum SortOrder {
    OldestFirst,
    #[default]
    NewestFirst,
    MessageId,
    Unsorted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryOptions {
    pub limit: usize,
    pub offset: usize,
    pub sort: SortOrder,
    pub excluded_tags: Vec<String>,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            limit: 100,
            offset: 0,
            sort: SortOrder::NewestFirst,
            excluded_tags: vec!["deleted".to_string(), "spam".to_string()],
        }
    }
}
