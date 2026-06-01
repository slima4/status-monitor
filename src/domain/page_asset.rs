//! Per-status-page asset slots. One enum owns the slug ↔ slot mapping and the
//! per-slot upload policy (allowed MIME types + max byte size), so adding a
//! slot is a one-spot change.

/// A named asset attached to a status page. Reserved future slots:
/// `Background`, `Favicon`, `OgImage`, `Font`, `CustomCss` — add the variant,
/// its `as_str`/`parse` arm, and a `policy` arm to wire one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetSlot {
    Logo,
}

/// Per-slot upload constraints. The configurable hook for "what may this slot
/// hold" — handlers gate uploads on it before the bytes ever reach the store.
#[derive(Debug, Clone)]
pub struct SlotPolicy {
    pub allowed_content_types: &'static [&'static str],
    pub max_byte_size: u64,
}

impl AssetSlot {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Logo => "logo",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "logo" => Some(Self::Logo),
            _ => None,
        }
    }

    pub fn policy(self) -> SlotPolicy {
        match self {
            Self::Logo => SlotPolicy {
                allowed_content_types: &["image/png", "image/jpeg", "image/webp"],
                // Mirrors the default `max_logo_size_bytes` (1 MiB).
                max_byte_size: 1_048_576,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_str_round_trips() {
        assert_eq!(
            AssetSlot::parse(AssetSlot::Logo.as_str()),
            Some(AssetSlot::Logo)
        );
        assert_eq!(AssetSlot::parse("nope"), None);
    }

    #[test]
    fn logo_policy_allows_images() {
        let p = AssetSlot::Logo.policy();
        assert!(p.allowed_content_types.contains(&"image/png"));
        assert!(p.max_byte_size > 0);
    }
}
