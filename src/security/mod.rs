pub mod abuse;
pub mod abuse_reload;
pub mod cert_probe;
pub mod crypto;
pub mod email_policy;
pub mod outbound_connector;
pub mod ssrf;

pub use abuse::{AbuseGuard, AbuseHit, AbuseKind};
pub use cert_probe::{CertFacts, CertProbeError};
pub use crypto::{
    Cipher, CryptoError, ENC_KEY, envelope_str, is_envelope, open_str, seal_str, wrap_envelope,
};
pub use email_policy::{Admission, EmailPolicy, EmailRisk};
pub use outbound_connector::SsrfHttpConnector;
pub use ssrf::{SsrfError, SsrfGuard, is_blocked_ip};

/// Strip the surrounding `[ ]` of a bracketed IPv6 literal host. The SSRF IP
/// check and the abuse domain check MUST normalise a host identically — a
/// single shared definition keeps them from drifting apart.
pub(crate) fn unbracket(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(host)
}
