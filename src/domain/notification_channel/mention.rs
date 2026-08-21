//! What every chat transport's ping field has in common. Only the vendor's
//! token syntax differs, so that is all a transport supplies.

/// Enough for a group plus a fallback, too few for a pasted member list to
/// turn one alert into a server-wide ping.
pub(super) const MAX_MENTIONS: usize = 5;

pub(super) fn tokens(raw: &str) -> impl Iterator<Item = &str> {
    raw.split([',', ' ', '\t', '\n', '\r'])
        .filter(|t| !t.is_empty())
}

/// An API client clears an optional string by sending it empty, so an emptied
/// ping means "stop pinging" rather than a blank one validation should refuse.
pub(super) fn cleared_when_empty(mention: Option<String>) -> Option<String> {
    mention
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
}

/// Refuse at save time what the operator would otherwise discover from an
/// alert that woke nobody.
pub(super) fn validate(
    raw: Option<&str>,
    pingable: impl Fn(&str) -> bool,
    explain: impl Fn(&str) -> String,
) -> Result<(), String> {
    let Some(raw) = raw else {
        return Ok(());
    };
    let tokens: Vec<&str> = tokens(raw).collect();
    if tokens.is_empty() {
        return Err("mention is blank — leave it unset to ping nobody".into());
    }
    if tokens.len() > MAX_MENTIONS {
        return Err(format!("mention takes at most {MAX_MENTIONS} entries"));
    }
    match tokens.into_iter().find(|t| !pingable(t)) {
        Some(bad) => Err(explain(bad)),
        None => Ok(()),
    }
}

/// A config test should prove the routing, not wake the whole room.
pub(super) fn without_broadcast(
    raw: Option<&str>,
    broadcast: impl Fn(&str) -> bool,
) -> Option<String> {
    raw.map(|raw| {
        tokens(raw)
            .filter(|t| !broadcast(t))
            .collect::<Vec<_>>()
            .join(" ")
    })
    .filter(|m| !m.is_empty())
}
