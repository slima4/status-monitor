//! Deterministic initials + colour for member/owner avatars, shared by the
//! monitors list and the incident console so a person looks the same everywhere.

use uuid::Uuid;

/// Up to two uppercase alphanumerics from the local part of an email (or the
/// whole string), `—` when there's nothing usable.
pub fn initials_from(s: &str) -> String {
    let head = s.split('@').next().unwrap_or(s);
    let mut chars = head.chars().filter(|c| c.is_alphanumeric());
    let a = chars.next().map(|c| c.to_ascii_uppercase());
    let b = chars.next().map(|c| c.to_ascii_uppercase());
    match (a, b) {
        (Some(a), Some(b)) => format!("{a}{b}"),
        (Some(a), None) => format!("{a}"),
        _ => "—".into(),
    }
}

/// Stable avatar background: hue derived from the id, fixed lightness/chroma.
pub fn avatar_color(id: Uuid) -> String {
    let bytes = id.as_bytes();
    let hue = ((bytes[0] as u16) << 8 | bytes[1] as u16) as f32 % 360.0;
    format!("oklch(0.62 0.12 {hue:.0})")
}
