//! Svix webhook signature verification (Resend signs its webhooks with the
//! Svix scheme): `HMAC-SHA256(key, "{id}.{timestamp}.{body}")`, key from the
//! base64 tail of the `whsec_…` endpoint secret, signature base64 in the
//! `svix-signature` header as space-separated `v1,<sig>` candidates.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

/// Reject events whose `svix-timestamp` is further than this from now —
/// the replay-protection window Svix itself documents.
pub const TOLERANCE_SECS: i64 = 5 * 60;

/// Verifies one delivery. `secret` is the endpoint secret as configured
/// (`whsec_` prefix optional); `id`/`timestamp`/`signatures` are the
/// `svix-id`/`svix-timestamp`/`svix-signature` header values; `now` is the
/// receiver's unix time.
pub fn verify(
    secret: &str,
    id: &str,
    timestamp: &str,
    signatures: &str,
    body: &[u8],
    now: i64,
) -> bool {
    let Ok(key) = B64.decode(secret.strip_prefix("whsec_").unwrap_or(secret)) else {
        return false;
    };
    let Ok(ts) = timestamp.parse::<i64>() else {
        return false;
    };
    if (now - ts).abs() > TOLERANCE_SECS {
        return false;
    }
    let mut mac = match Hmac::<Sha256>::new_from_slice(&key) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(id.as_bytes());
    mac.update(b".");
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    let expected = mac.finalize().into_bytes();
    signatures
        .split_ascii_whitespace()
        .filter_map(|entry| entry.strip_prefix("v1,"))
        .filter_map(|sig| B64.decode(sig).ok())
        .any(|candidate| candidate.ct_eq(&expected).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw";
    const NOW: i64 = 1_700_000_000;

    fn sign(id: &str, ts: i64, body: &[u8]) -> String {
        let key = B64.decode(SECRET.strip_prefix("whsec_").unwrap()).unwrap();
        let mut mac = Hmac::<Sha256>::new_from_slice(&key).unwrap();
        mac.update(format!("{id}.{ts}.").as_bytes());
        mac.update(body);
        format!("v1,{}", B64.encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn accepts_a_correctly_signed_delivery() {
        let sig = sign("msg_1", NOW, b"{\"type\":\"email.bounced\"}");
        assert!(verify(
            SECRET,
            "msg_1",
            &NOW.to_string(),
            &sig,
            b"{\"type\":\"email.bounced\"}",
            NOW,
        ));
    }

    #[test]
    fn accepts_secret_without_prefix_and_extra_candidates() {
        let sig = format!("v1,AAAA v2,BBBB {}", sign("msg_1", NOW, b"x"));
        assert!(verify(
            SECRET.strip_prefix("whsec_").unwrap(),
            "msg_1",
            &NOW.to_string(),
            &sig,
            b"x",
            NOW,
        ));
    }

    #[test]
    fn rejects_wrong_signature_body_id_and_stale_timestamp() {
        let sig = sign("msg_1", NOW, b"x");
        let ts = NOW.to_string();
        assert!(!verify(SECRET, "msg_1", &ts, "v1,bm9wZQ==", b"x", NOW));
        assert!(!verify(SECRET, "msg_1", &ts, &sig, b"tampered", NOW));
        assert!(!verify(SECRET, "msg_2", &ts, &sig, b"x", NOW));
        assert!(!verify(
            SECRET,
            "msg_1",
            &ts,
            &sig,
            b"x",
            NOW + TOLERANCE_SECS + 1,
        ));
        let old = sign("msg_1", NOW - TOLERANCE_SECS - 1, b"x");
        assert!(!verify(
            SECRET,
            "msg_1",
            &(NOW - TOLERANCE_SECS - 1).to_string(),
            &old,
            b"x",
            NOW,
        ));
    }

    #[test]
    fn rejects_garbage_secret_timestamp_and_header() {
        let sig = sign("msg_1", NOW, b"x");
        let ts = NOW.to_string();
        assert!(!verify("whsec_!!!", "msg_1", &ts, &sig, b"x", NOW));
        assert!(!verify(SECRET, "msg_1", "soon", &sig, b"x", NOW));
        assert!(!verify(SECRET, "msg_1", &ts, "", b"x", NOW));
        assert!(!verify(SECRET, "msg_1", &ts, "v2,abc def", b"x", NOW));
    }
}
