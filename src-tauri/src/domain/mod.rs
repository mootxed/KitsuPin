mod classification;
mod normalize;
mod self_copy;

pub use classification::{classify, ContentType};
pub use normalize::{content_hash, normalize_content, normalize_domain};
pub use self_copy::{ClipboardEventOrigin, OwnCopyGuard};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PayloadKind {
    Text,
    Image,
}

impl PayloadKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Image => "image",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CapturedImageSource {
    #[default]
    ClipboardImage,
    CopiedImageFile,
}

#[derive(Debug, Clone)]
pub struct ImagePayload {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub source_mime: Option<String>,
    pub source_bytes: Option<Vec<u8>>,
    pub image_source: CapturedImageSource,
}

impl ImagePayload {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.width > 0 && self.height > 0,
            "изображение имеет нулевой размер"
        );
        anyhow::ensure!(
            self.width <= 32_768 && self.height <= 32_768,
            "размеры изображения слишком велики"
        );
        let expected = (self.width as usize)
            .checked_mul(self.height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| anyhow::anyhow!("размер изображения переполнен"))?;
        anyhow::ensure!(
            expected == self.rgba.len(),
            "повреждённые RGBA-данные изображения"
        );
        anyhow::ensure!(
            expected <= 256 * 1024 * 1024,
            "декодированное изображение превышает 256 МБ"
        );
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub enum ClipboardPayload {
    Text(String),
    Image(ImagePayload),
}

impl ClipboardPayload {
    pub fn match_key(&self) -> Option<(String, usize)> {
        match self {
            Self::Text(content) => {
                let normalized = normalize_content(content);
                if normalized.is_empty() {
                    None
                } else {
                    Some((content_hash(&normalized), normalized.len()))
                }
            }
            Self::Image(image) => {
                use sha2::{Digest, Sha256};
                let mut digest = Sha256::new();
                digest.update(image.width.to_le_bytes());
                digest.update(image.height.to_le_bytes());
                digest.update(&image.rgba);
                Some((hex::encode(digest.finalize()), image.rgba.len()))
            }
        }
    }

    pub fn fingerprint(&self) -> String {
        let prefix = match self {
            Self::Text(_) => "text",
            Self::Image(_) => "image",
        };
        self.match_key()
            .map(|(hash, _)| format!("{prefix}:{hash}"))
            .unwrap_or_else(|| format!("{prefix}:empty"))
    }
}

#[derive(Debug, Clone)]
pub struct ClipboardCopy {
    pub payload: ClipboardPayload,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageMetadata {
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    pub size_bytes: u64,
    pub thumbnail_data_url: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageStats {
    pub image_count: u64,
    pub image_bytes: u64,
    pub orphan_files_removed: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Category {
    pub id: String,
    pub name: String,
    pub color: String,
    pub created_at: i64,
    pub sort_order: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipSummary {
    pub id: String,
    pub preview: String,
    pub content_length: usize,
    /// True when preview does not contain the full content (content_length > preview chars).
    pub is_truncated: bool,
    pub content_type: ContentType,
    pub payload_kind: PayloadKind,
    pub image: Option<ImageMetadata>,
    pub domain: Option<String>,
    pub page_title: Option<String>,
    pub created_at: i64,
    pub last_copied_at: i64,
    pub copy_count: i64,
    pub pinned: bool,
    pub categories: Vec<Category>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct Clip {
    pub id: String,
    pub content: String,
    pub content_type: ContentType,
    pub domain: Option<String>,
    pub page_title: Option<String>,
    pub created_at: i64,
    pub last_copied_at: i64,
    pub copy_count: i64,
    pub pinned: bool,
    pub categories: Vec<Category>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct ClipDetails {
    pub id: String,
    pub content: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipQuery {
    pub search: Option<String>,
    pub content_type: Option<ContentType>,
    pub payload_kind: Option<PayloadKind>,
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
    pub now: i64,
}

#[derive(Debug, Clone)]
pub struct NewImageClip<'a> {
    pub image: &'a ImagePayload,
    pub domain: Option<&'a str>,
    pub page_title: Option<&'a str>,
    pub now: i64,
    pub max_image_bytes: u64,
    pub max_storage_bytes: u64,
}
