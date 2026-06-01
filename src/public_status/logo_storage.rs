//! Logo storage backend for per-org public status pages.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tokio::fs;
use tokio::io::AsyncReadExt;

use crate::domain::OrgId;

/// Closed allow-list of supported logo formats. One source of truth for the
/// MIME ↔ extension pairing — the upload path, the on-disk filename, and the
/// served `Content-Type` all route through this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogoMime {
    Png,
    Jpeg,
    Webp,
}

impl LogoMime {
    pub fn from_content_type(s: &str) -> Option<Self> {
        Some(match s {
            "image/png" => Self::Png,
            "image/jpeg" => Self::Jpeg,
            "image/webp" => Self::Webp,
            _ => return None,
        })
    }

    /// Maps a decoder-sniffed format to the allow-list. The upload path
    /// sniffs bytes with `image::guess_format` and routes the result through
    /// here so format identity stays owned by this one enum.
    pub fn from_image_format(f: image::ImageFormat) -> Option<Self> {
        Some(match f {
            image::ImageFormat::Png => Self::Png,
            image::ImageFormat::Jpeg => Self::Jpeg,
            image::ImageFormat::WebP => Self::Webp,
            _ => return None,
        })
    }

    pub fn from_extension(s: &str) -> Option<Self> {
        Some(match s {
            "png" => Self::Png,
            "jpg" | "jpeg" => Self::Jpeg,
            "webp" => Self::Webp,
            _ => return None,
        })
    }

    pub fn as_content_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
        }
    }

    pub fn as_extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Webp => "webp",
        }
    }
}

#[async_trait]
pub trait LogoStorage: Send + Sync {
    async fn put(&self, org_id: OrgId, content_type: &str, data: &[u8]) -> Result<String>;
    async fn get(&self, path: &str) -> Result<Option<(Vec<u8>, String)>>;
    async fn delete(&self, path: &str) -> Result<()>;
}

#[derive(Debug, Clone)]
pub struct LocalDiskLogoStorage {
    root: PathBuf,
}

impl LocalDiskLogoStorage {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn resolve(&self, name: &str) -> Result<PathBuf> {
        // Stored names are bare `{hash}.{ext}` strings. Separators or
        // `..` segments here mean a caller is trying to escape `root`.
        if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
            return Err(anyhow!("invalid logo path: {name}"));
        }
        Ok(self.root.join(name))
    }
}

#[async_trait]
impl LogoStorage for LocalDiskLogoStorage {
    async fn put(&self, org_id: OrgId, content_type: &str, data: &[u8]) -> Result<String> {
        let mime = LogoMime::from_content_type(content_type)
            .ok_or_else(|| anyhow!("unsupported logo content_type: {content_type}"))?;
        // Opaque hash-based filename, salted with the org id: cacheable
        // forever (new upload → new path, no server bookkeeping), per-org
        // unique (same image in two orgs → distinct files, so a delete in
        // one never strands the other), and the org id stays out of the
        // public URL. 8 bytes is plenty — a collision only shares a file.
        let mut hasher = Sha256::new();
        hasher.update(org_id.0.as_bytes());
        hasher.update(data);
        let hash = hex_lower(&hasher.finalize()[..8]);
        let name = format!("{}.{}", hash, mime.as_extension());
        let path = self.resolve(&name)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("create logo dir {}", parent.display()))?;
        }
        fs::write(&path, data)
            .await
            .with_context(|| format!("write logo {}", path.display()))?;
        Ok(name)
    }

    async fn get(&self, path: &str) -> Result<Option<(Vec<u8>, String)>> {
        let abs = self.resolve(path)?;
        let mut file = match fs::File::open(&abs).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("open logo {}", abs.display())),
        };
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)
            .await
            .with_context(|| format!("read logo {}", abs.display()))?;
        let ext = Path::new(path)
            .extension()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow!("logo path missing extension: {path}"))?;
        let mime = LogoMime::from_extension(ext)
            .ok_or_else(|| anyhow!("logo path has unknown extension: {ext}"))?;
        Ok(Some((buf, mime.as_content_type().to_string())))
    }

    async fn delete(&self, path: &str) -> Result<()> {
        let abs = self.resolve(path)?;
        match fs::remove_file(&abs).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("delete logo {}", abs.display())),
        }
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn temp_root() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[tokio::test]
    async fn put_then_get_roundtrips() {
        let dir = temp_root();
        let store = LocalDiskLogoStorage::new(dir.path());
        let org = OrgId(Uuid::new_v4());
        let bytes = b"\x89PNG\r\n\x1a\nfake";
        let path = store.put(org, "image/png", bytes).await.unwrap();
        let got = store.get(&path).await.unwrap().unwrap();
        assert_eq!(got.0, bytes);
        assert_eq!(got.1, "image/png");
        store.delete(&path).await.unwrap();
        assert!(store.get(&path).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn rejects_unsupported_content_type() {
        let dir = temp_root();
        let store = LocalDiskLogoStorage::new(dir.path());
        let err = store
            .put(OrgId(Uuid::new_v4()), "image/svg+xml", b"<svg/>")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unsupported"));
    }

    #[tokio::test]
    async fn rejects_path_traversal_on_get() {
        let dir = temp_root();
        let store = LocalDiskLogoStorage::new(dir.path());
        for bad in ["../etc/passwd", "..\\windows", "sub/dir/x.png", ""] {
            let err = store.get(bad).await.unwrap_err();
            assert!(err.to_string().contains("invalid"), "{bad}");
        }
    }
}
