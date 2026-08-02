use crate::domain::{CapturedImageSource, ImagePayload};
use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    io::{BufWriter, Write},
    path::{Component, Path, PathBuf},
};

const THUMBNAIL_MAX_WIDTH: u32 = 320;
const THUMBNAIL_MAX_HEIGHT: u32 = 180;

#[derive(Debug)]
pub struct PreparedImage {
    pub hash: String,
    pub relative_path: String,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    pub size_bytes: u64,
    pub thumbnail_data_url: String,
    bytes: Vec<u8>,
}

pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    pub fn new(root: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&root).context("не удалось создать blob-хранилище")?;
        secure_directory(&root)?;
        Ok(Self { root })
    }

    pub fn prepare(&self, image: &ImagePayload, max_image_bytes: u64) -> Result<PreparedImage> {
        image.validate()?;
        let (bytes, mime_type, extension) = match (&image.source_mime, &image.source_bytes) {
            (Some(mime), Some(bytes)) if mime == "image/png" => {
                (bytes.clone(), mime.clone(), "png")
            }
            (Some(mime), Some(bytes)) if mime == "image/jpeg" => {
                (bytes.clone(), mime.clone(), "jpg")
            }
            (Some(mime), Some(bytes)) if mime == "image/webp" => {
                (bytes.clone(), mime.clone(), "webp")
            }
            _ => (
                encode_png(image.width, image.height, &image.rgba)?,
                "image/png".to_owned(),
                "png",
            ),
        };
        anyhow::ensure!(
            bytes.len() as u64 <= max_image_bytes,
            "изображение превышает лимит {} МБ",
            max_image_bytes / (1024 * 1024)
        );
        let hash = hex::encode(Sha256::digest(&bytes));
        let relative_path = format!("{}/{}.{}", &hash[..2], hash, extension);
        let thumbnail = make_thumbnail(image)?;
        let thumbnail_png = encode_png(thumbnail.width, thumbnail.height, &thumbnail.rgba)?;
        Ok(PreparedImage {
            hash,
            relative_path,
            mime_type,
            width: image.width,
            height: image.height,
            size_bytes: bytes.len() as u64,
            thumbnail_data_url: format!("data:image/png;base64,{}", STANDARD.encode(thumbnail_png)),
            bytes,
        })
    }

    /// Persists a prepared image and returns true when a new file was created.
    pub fn persist(&self, image: &PreparedImage) -> Result<bool> {
        let target = self.resolve_relative(&image.relative_path)?;
        if target.exists() {
            return Ok(false);
        }
        let parent = target.parent().context("некорректный blob-путь")?;
        std::fs::create_dir_all(parent)?;
        secure_directory(parent)?;
        let temp = parent.join(format!(".{}.{}.tmp", image.hash, uuid::Uuid::new_v4()));
        let result = (|| -> Result<()> {
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            let mut writer = BufWriter::new(file);
            writer.write_all(&image.bytes)?;
            writer.flush()?;
            writer.get_ref().sync_all()?;
            secure_file(&temp)?;
            match std::fs::rename(&temp, &target) {
                Ok(()) => {}
                Err(_error) if target.exists() => {
                    let _ = std::fs::remove_file(&temp);
                    return Ok(());
                }
                Err(error) => return Err(error.into()),
            }
            secure_file(&target)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result.map(|_| true)
    }

    pub fn load(&self, relative_path: &str) -> Result<ImagePayload> {
        let path = self.resolve_relative(relative_path)?;
        let mut reader = image::ImageReader::open(&path)?
            .with_guessed_format()
            .context("формат blob не распознан")?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(32_768);
        limits.max_image_height = Some(32_768);
        limits.max_alloc = Some(256 * 1024 * 1024);
        reader.limits(limits);
        let decoded = reader
            .decode()
            .context("повреждённый image blob")?
            .into_rgba8();
        let (width, height) = decoded.dimensions();
        let image = ImagePayload {
            width,
            height,
            rgba: decoded.into_raw(),
            source_mime: None,
            source_bytes: None,
            image_source: CapturedImageSource::ClipboardImage,
        };
        image.validate()?;
        Ok(image)
    }

    pub fn read_data_url(&self, relative_path: &str, mime_type: &str) -> Result<String> {
        let path = self.resolve_relative(relative_path)?;
        let bytes = std::fs::read(path)?;
        anyhow::ensure!(
            bytes.len() <= 50 * 1024 * 1024,
            "blob превышает лимит просмотра"
        );
        Ok(format!(
            "data:{mime_type};base64,{}",
            STANDARD.encode(bytes)
        ))
    }

    pub fn absolute_path(&self, relative_path: &str) -> Result<PathBuf> {
        self.resolve_relative(relative_path)
    }

    pub fn remove(&self, relative_path: &str) -> Result<()> {
        let path = self.resolve_relative(relative_path)?;
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir(parent);
        }
        Ok(())
    }

    pub fn cleanup_orphans(&self, referenced: &std::collections::HashSet<String>) -> Result<usize> {
        let mut removed = 0;
        for prefix in std::fs::read_dir(&self.root)? {
            let prefix = prefix?;
            if !prefix.file_type()?.is_dir() {
                continue;
            }
            for entry in std::fs::read_dir(prefix.path())? {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    continue;
                }
                let relative = entry
                    .path()
                    .strip_prefix(&self.root)?
                    .to_string_lossy()
                    .replace('\\', "/");
                if !referenced.contains(&relative) {
                    std::fs::remove_file(entry.path())?;
                    removed += 1;
                }
            }
            let _ = std::fs::remove_dir(prefix.path());
        }
        Ok(removed)
    }

    fn resolve_relative(&self, relative_path: &str) -> Result<PathBuf> {
        let relative = Path::new(relative_path);
        anyhow::ensure!(
            !relative.is_absolute()
                && relative
                    .components()
                    .all(|part| matches!(part, Component::Normal(_))),
            "небезопасный относительный blob-путь"
        );
        Ok(self.root.join(relative))
    }
}

fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>> {
    let mut encoded = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut encoded, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(rgba)?;
    }
    Ok(encoded)
}

fn make_thumbnail(image: &ImagePayload) -> Result<ImagePayload> {
    let scale = (THUMBNAIL_MAX_WIDTH as f64 / image.width as f64)
        .min(THUMBNAIL_MAX_HEIGHT as f64 / image.height as f64)
        .min(1.0);
    let width = ((image.width as f64 * scale).round() as u32).max(1);
    let height = ((image.height as f64 * scale).round() as u32).max(1);
    let mut rgba = vec![0; width as usize * height as usize * 4];
    for y in 0..height {
        let source_y = ((y as u64 * image.height as u64) / height as u64) as u32;
        for x in 0..width {
            let source_x = ((x as u64 * image.width as u64) / width as u64) as u32;
            let source = ((source_y * image.width + source_x) * 4) as usize;
            let target = ((y * width + x) * 4) as usize;
            rgba[target..target + 4].copy_from_slice(&image.rgba[source..source + 4]);
        }
    }
    Ok(ImagePayload {
        width,
        height,
        rgba,
        source_mime: None,
        source_bytes: None,
        image_source: image.image_source,
    })
}

#[cfg(unix)]
fn secure_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn secure_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn secure_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn secure_file(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_deduplicates_and_loads_canonical_png() {
        let temp = tempfile::tempdir().unwrap();
        let store = BlobStore::new(temp.path().join("blobs")).unwrap();
        let payload = ImagePayload {
            width: 2,
            height: 1,
            rgba: vec![255, 0, 0, 255, 0, 255, 0, 255],
            source_mime: None,
            source_bytes: None,
            image_source: CapturedImageSource::ClipboardImage,
        };
        let prepared = store.prepare(&payload, 1024 * 1024).unwrap();
        assert!(store.persist(&prepared).unwrap());
        assert!(!store.persist(&prepared).unwrap());
        let loaded = store.load(&prepared.relative_path).unwrap();
        assert_eq!(loaded.rgba, payload.rgba);
        assert!(prepared
            .thumbnail_data_url
            .starts_with("data:image/png;base64,"));
    }

    #[test]
    fn rejects_unsafe_relative_paths() {
        let temp = tempfile::tempdir().unwrap();
        let store = BlobStore::new(temp.path().join("blobs")).unwrap();
        assert!(store.load("../outside.png").is_err());
    }
}
