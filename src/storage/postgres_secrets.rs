use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::security::{Cipher, ENC_KEY, is_envelope, wrap_envelope};

/// JSON pointer paths inside `check_spec` that hold credential material.
/// `CheckSpec` uses `#[serde(tag = "type")]`, so the variant's fields are
/// inlined at the JSON root rather than nested under `"http"`. The pointer
/// lookups silently no-op on non-http checks because the field isn't present.
const CREDENTIAL_PATHS: &[&str] = &["/basic_auth", "/bearer_token"];

pub fn encrypt_in_place(value: &mut Value, cipher: &Cipher) -> anyhow::Result<()> {
    for path in CREDENTIAL_PATHS {
        let Some(slot) = value.pointer_mut(path) else {
            continue;
        };
        if is_already_envelope(slot) || slot.is_null() {
            continue;
        }
        let plaintext = serde_json::to_vec(slot).context("encoding credential for encryption")?;
        let envelope = cipher
            .encrypt(&plaintext)
            .map_err(|e| anyhow::anyhow!("credential encryption failed: {e}"))?;
        *slot = wrap_envelope(envelope);
    }
    Ok(())
}

pub fn decrypt_in_place(value: &mut Value, cipher: &Cipher, target_id: Uuid) -> anyhow::Result<()> {
    for path in CREDENTIAL_PATHS {
        let Some(slot) = value.pointer_mut(path) else {
            continue;
        };
        let Some(envelope) = extract_envelope(slot) else {
            continue;
        };
        let plaintext = cipher
            .decrypt(&envelope)
            .map_err(|e| anyhow::anyhow!("credential decryption failed: {e}"))?;
        *slot =
            serde_json::from_slice(&plaintext).context("decoding decrypted credential payload")?;
        tracing::debug!(
            target_id = %target_id,
            field = %path,
            "credential decrypted"
        );
    }
    Ok(())
}

/// Overwrite every credential slot in a raw `check_spec` JSON value with the
/// sentinel `"***"`. The exact inverse of [`decrypt_in_place`]'s intent: the
/// GDPR data export must hand the user a copy of their config that proves a
/// secret was set without disclosing it. Walks the same [`CREDENTIAL_PATHS`]
/// pointer list (single owner) so a credential field added there is redacted
/// here automatically, and works on either the encrypted envelope or the
/// no-KEK plaintext-at-rest case because it never inspects the slot's shape.
pub fn redact_credentials(value: &mut Value) {
    for path in CREDENTIAL_PATHS {
        let Some(slot) = value.pointer_mut(path) else {
            continue;
        };
        if slot.is_null() {
            continue;
        }
        *slot = Value::String("***".to_string());
    }
}

/// The credential-bearing target columns exactly as they sit at rest. Maps a
/// `targets` row via `sqlx::FromRow`; `check_spec` is whatever is stored — an
/// `$enc` envelope or (no-KEK self-host) plaintext — and is the *only* path
/// into [`RedactedTarget`], so it is structurally impossible to feed the
/// export a decrypted value.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RawTargetRow {
    pub id: Uuid,
    pub name: String,
    pub check_spec: Value,
    pub interval_secs: i32,
    pub enabled: bool,
    pub tags: Vec<String>,
    pub group_name: Option<String>,
    pub owner_user_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A target row whose credentials have already been replaced with `"***"`.
///
/// Distinct from [`crate::domain::Target`] on purpose: the only constructor is
/// [`RedactedTarget::from_row`], which calls [`redact_credentials`] and never
/// has a [`Cipher`] in scope. Handing the GDPR export a decryptable `Target`
/// (the normal `decrypt_in_place` read path) is therefore a *compile* error,
/// not a redaction that someone forgot to apply. `check` stays a raw
/// `serde_json::Value` so no `CheckSpec` deserialize can re-surface a cleared
/// slot, and `deleted_at` is carried through so a still-within-grace target is
/// honestly stamped (soft-deleted-but-not-purged scope).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RedactedTarget {
    #[schema(value_type = String, format = "uuid")]
    pub id: Uuid,
    pub name: String,
    /// `check_spec` with every [`CREDENTIAL_PATHS`] slot forced to `"***"`.
    #[schema(value_type = Object)]
    pub check: Value,
    pub interval_secs: i32,
    pub enabled: bool,
    pub tags: Vec<String>,
    pub group_name: Option<String>,
    pub owner_user_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Non-null while the target is soft-deleted but not yet purged.
    pub deleted_at: Option<DateTime<Utc>>,
}

impl RedactedTarget {
    /// Redact the at-rest credential slots and stamp the target with the
    /// lifecycle state that governs it (its owning org's `deleted_at`, since
    /// targets carry no soft-delete column of their own). `check_spec` is
    /// never decrypted.
    pub fn from_row(row: RawTargetRow, deleted_at: Option<DateTime<Utc>>) -> Self {
        let mut check = row.check_spec;
        redact_credentials(&mut check);
        Self {
            id: row.id,
            name: row.name,
            check,
            interval_secs: row.interval_secs,
            enabled: row.enabled,
            tags: row.tags,
            group_name: row.group_name,
            owner_user_id: row.owner_user_id,
            created_at: row.created_at,
            updated_at: row.updated_at,
            deleted_at,
        }
    }
}

fn is_already_envelope(v: &Value) -> bool {
    v.as_object()
        .and_then(|m| m.get(ENC_KEY))
        .and_then(Value::as_str)
        .is_some_and(is_envelope)
}

fn extract_envelope(v: &Value) -> Option<String> {
    v.as_object()?.get(ENC_KEY)?.as_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde_json::json;

    fn cipher() -> Cipher {
        Cipher::from_base64(&STANDARD.encode([42u8; 32])).unwrap()
    }

    #[test]
    fn encrypt_then_decrypt_round_trips_basic_auth() {
        let id = Uuid::nil();
        let c = cipher();
        let mut v = json!({"type":"http","basic_auth":["alice","s3cret"],"bearer_token":null});
        encrypt_in_place(&mut v, &c).unwrap();
        let enc = v.pointer("/basic_auth/$enc").and_then(Value::as_str);
        assert!(enc.is_some_and(|s| s.starts_with("v1:")));
        decrypt_in_place(&mut v, &c, id).unwrap();
        assert_eq!(v["basic_auth"], json!(["alice", "s3cret"]));
    }

    #[test]
    fn encrypt_then_decrypt_round_trips_bearer_token() {
        let id = Uuid::nil();
        let c = cipher();
        let mut v = json!({"type":"http","basic_auth":null,"bearer_token":"abc.def.ghi"});
        encrypt_in_place(&mut v, &c).unwrap();
        assert!(v["bearer_token"]["$enc"].is_string());
        decrypt_in_place(&mut v, &c, id).unwrap();
        assert_eq!(v["bearer_token"], json!("abc.def.ghi"));
    }

    #[test]
    fn legacy_plaintext_passes_through_decrypt() {
        let id = Uuid::nil();
        let c = cipher();
        let mut v = json!({"type":"http","basic_auth":["a","b"]});
        decrypt_in_place(&mut v, &c, id).unwrap();
        assert_eq!(v["basic_auth"], json!(["a", "b"]));
    }

    #[test]
    fn null_credential_skips_encryption() {
        let c = cipher();
        let mut v = json!({"type":"http","basic_auth":null,"bearer_token":null});
        encrypt_in_place(&mut v, &c).unwrap();
        assert!(v["basic_auth"].is_null());
        assert!(v["bearer_token"].is_null());
    }

    #[test]
    fn already_encrypted_does_not_double_wrap() {
        let c = cipher();
        let mut v = json!({"type":"http","basic_auth":["a","b"]});
        encrypt_in_place(&mut v, &c).unwrap();
        let first = v["basic_auth"].clone();
        encrypt_in_place(&mut v, &c).unwrap();
        assert_eq!(v["basic_auth"], first);
    }

    #[test]
    fn other_fields_untouched_by_encrypt() {
        let c = cipher();
        let mut v = json!({
            "type":"http",
            "url":"https://x.test","method":"GET","headers":{"X":"y"}
        });
        encrypt_in_place(&mut v, &c).unwrap();
        assert_eq!(v["url"], json!("https://x.test"));
        assert_eq!(v["headers"]["X"], json!("y"));
    }

    #[test]
    fn tcp_check_no_credentials_path() {
        let c = cipher();
        let mut v = json!({"type":"tcp","host":"x","port":80});
        encrypt_in_place(&mut v, &c).unwrap();
        assert_eq!(v["host"], json!("x"));
    }

    #[test]
    fn redact_replaces_set_credentials_with_sentinel() {
        let mut v = json!({"type":"http","basic_auth":["alice","s3cret"],"bearer_token":"abc.def"});
        redact_credentials(&mut v);
        assert_eq!(v["basic_auth"], json!("***"));
        assert_eq!(v["bearer_token"], json!("***"));
        assert_eq!(v["type"], json!("http"));
    }

    #[test]
    fn redact_leaves_unset_credentials_null() {
        let mut v = json!({"type":"http","basic_auth":null,"bearer_token":null});
        redact_credentials(&mut v);
        assert!(v["basic_auth"].is_null());
        assert!(v["bearer_token"].is_null());
    }

    #[test]
    fn redact_covers_encrypted_envelope_without_decrypting() {
        let c = cipher();
        let mut v = json!({"type":"http","basic_auth":["u","p"],"bearer_token":null});
        encrypt_in_place(&mut v, &c).unwrap();
        assert!(v["basic_auth"]["$enc"].is_string());
        redact_credentials(&mut v);
        // Envelope object is gone; the sentinel proves a value was set without
        // disclosing the ciphertext.
        assert_eq!(v["basic_auth"], json!("***"));
    }

    #[test]
    fn redacted_target_from_row_clears_credentials() {
        let row = RawTargetRow {
            id: Uuid::nil(),
            name: "t".into(),
            check_spec: json!({"type":"http","basic_auth":["u","p"],"bearer_token":"tok"}),
            interval_secs: 60,
            enabled: true,
            tags: vec![],
            group_name: None,
            owner_user_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let rt = RedactedTarget::from_row(row, None);
        assert_eq!(rt.check["basic_auth"], json!("***"));
        assert_eq!(rt.check["bearer_token"], json!("***"));
    }

    #[test]
    fn decrypt_fails_with_wrong_kek() {
        let a = cipher();
        let b = Cipher::from_base64(&STANDARD.encode([99u8; 32])).unwrap();
        let mut v = json!({"type":"http","basic_auth":["u","p"]});
        encrypt_in_place(&mut v, &a).unwrap();
        assert!(decrypt_in_place(&mut v, &b, Uuid::nil()).is_err());
    }
}
