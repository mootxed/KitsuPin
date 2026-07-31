mod classification;
mod normalize;
mod self_copy;

pub use classification::{classify, ContentType};
pub use normalize::{content_hash, normalize_content, normalize_domain};
pub use self_copy::OwnCopyGuard;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Category {
    pub id: String,
    pub name: String,
    pub color: String,
    pub created_at: String,
    pub sort_order: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Clip {
    pub id: String,
    pub content: String,
    pub content_type: ContentType,
    pub domain: Option<String>,
    pub page_title: Option<String>,
    pub created_at: String,
    pub last_copied_at: String,
    pub copy_count: i64,
    pub pinned: bool,
    pub categories: Vec<Category>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipQuery {
    pub search: Option<String>,
    pub content_type: Option<ContentType>,
    pub domain: Option<String>,
    pub category_id: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct NewClip<'a> {
    pub content: &'a str,
    pub domain: Option<&'a str>,
    pub page_title: Option<&'a str>,
    pub now: &'a str,
}
