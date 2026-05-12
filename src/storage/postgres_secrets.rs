use anyhow::Context;
use serde_json::Value;
use uuid::Uuid;

use crate::security::{Cipher, is_envelope};

/// JSON pointer paths inside `check_spec` that hold credential material. Walked
/// at the storage boundary to wrap/unwrap an envelope around plaintext values.
const CREDENTIAL_PATHS: &[&str] = &["/http/basic_auth", "/http/bearer_token"];

const ENC_KEY: &str = "$enc";

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
        *slot = Value::Object(
            [(ENC_KEY.to_string(), Value::String(envelope))]
                .into_iter()
                .collect(),
        );
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
        let mut v =
            json!({"type":"http","http":{"basic_auth":["alice","s3cret"],"bearer_token":null}});
        encrypt_in_place(&mut v, &c).unwrap();
        let enc = v.pointer("/http/basic_auth/$enc").and_then(Value::as_str);
        assert!(enc.is_some_and(|s| s.starts_with("v1:")));
        decrypt_in_place(&mut v, &c, id).unwrap();
        assert_eq!(v["http"]["basic_auth"], json!(["alice", "s3cret"]));
    }

    #[test]
    fn encrypt_then_decrypt_round_trips_bearer_token() {
        let id = Uuid::nil();
        let c = cipher();
        let mut v = json!({"type":"http","http":{"basic_auth":null,"bearer_token":"abc.def.ghi"}});
        encrypt_in_place(&mut v, &c).unwrap();
        assert!(v["http"]["bearer_token"]["$enc"].is_string());
        decrypt_in_place(&mut v, &c, id).unwrap();
        assert_eq!(v["http"]["bearer_token"], json!("abc.def.ghi"));
    }

    #[test]
    fn legacy_plaintext_passes_through_decrypt() {
        let id = Uuid::nil();
        let c = cipher();
        let mut v = json!({"type":"http","http":{"basic_auth":["a","b"]}});
        decrypt_in_place(&mut v, &c, id).unwrap();
        assert_eq!(v["http"]["basic_auth"], json!(["a", "b"]));
    }

    #[test]
    fn null_credential_skips_encryption() {
        let c = cipher();
        let mut v = json!({"type":"http","http":{"basic_auth":null,"bearer_token":null}});
        encrypt_in_place(&mut v, &c).unwrap();
        assert!(v["http"]["basic_auth"].is_null());
        assert!(v["http"]["bearer_token"].is_null());
    }

    #[test]
    fn already_encrypted_does_not_double_wrap() {
        let c = cipher();
        let mut v = json!({"type":"http","http":{"basic_auth":["a","b"]}});
        encrypt_in_place(&mut v, &c).unwrap();
        let first = v["http"]["basic_auth"].clone();
        encrypt_in_place(&mut v, &c).unwrap();
        assert_eq!(v["http"]["basic_auth"], first);
    }

    #[test]
    fn other_fields_untouched_by_encrypt() {
        let c = cipher();
        let mut v = json!({
            "type":"http",
            "http":{"url":"https://x.test","method":"GET","headers":{"X":"y"}}
        });
        encrypt_in_place(&mut v, &c).unwrap();
        assert_eq!(v["http"]["url"], json!("https://x.test"));
        assert_eq!(v["http"]["headers"]["X"], json!("y"));
    }

    #[test]
    fn tcp_check_no_credentials_path() {
        let c = cipher();
        let mut v = json!({"type":"tcp","tcp":{"host":"x","port":80}});
        encrypt_in_place(&mut v, &c).unwrap();
        assert_eq!(v["tcp"]["host"], json!("x"));
    }

    #[test]
    fn decrypt_fails_with_wrong_kek() {
        let a = cipher();
        let b = Cipher::from_base64(&STANDARD.encode([99u8; 32])).unwrap();
        let mut v = json!({"type":"http","http":{"basic_auth":["u","p"]}});
        encrypt_in_place(&mut v, &a).unwrap();
        assert!(decrypt_in_place(&mut v, &b, Uuid::nil()).is_err());
    }
}
