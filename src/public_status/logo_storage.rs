//! Logo MIME allow-list for per-page status-page logos.

/// Closed allow-list of supported logo formats. One source of truth for the
/// MIME ↔ extension pairing — the upload path and the served `Content-Type`
/// both route through this enum.
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
