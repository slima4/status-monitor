//! API-token permission scopes.
//!
//! A scope is a `resource:action` capability (e.g. `targets:write`) stored in
//! the `api_tokens.scopes` JSONB column and checked per request by the
//! `Authorized<R>` extractor. `write` implies `read` for the same resource;
//! `full_access` is a superset of everything. Sessions are not scoped — only
//! API tokens carry a [`ScopeSet`].

use std::collections::HashSet;

/// One permission. Stored/serialized as its `as_str()` form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    TargetsRead,
    TargetsWrite,
    ChannelsRead,
    ChannelsWrite,
    IncidentsRead,
    IncidentsWrite,
    MaintenanceRead,
    MaintenanceWrite,
    StatusPageRead,
    StatusPageWrite,
    /// Superset of every other scope. The historical default.
    FullAccess,
}

impl Scope {
    /// Canonical wire/storage string.
    pub const fn as_str(self) -> &'static str {
        match self {
            Scope::TargetsRead => "targets:read",
            Scope::TargetsWrite => "targets:write",
            Scope::ChannelsRead => "channels:read",
            Scope::ChannelsWrite => "channels:write",
            Scope::IncidentsRead => "incidents:read",
            Scope::IncidentsWrite => "incidents:write",
            Scope::MaintenanceRead => "maintenance:read",
            Scope::MaintenanceWrite => "maintenance:write",
            Scope::StatusPageRead => "status_page:read",
            Scope::StatusPageWrite => "status_page:write",
            Scope::FullAccess => "full_access",
        }
    }

    /// Parse a stored/submitted string. Unknown strings yield `None`.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "targets:read" => Scope::TargetsRead,
            "targets:write" => Scope::TargetsWrite,
            "channels:read" => Scope::ChannelsRead,
            "channels:write" => Scope::ChannelsWrite,
            "incidents:read" => Scope::IncidentsRead,
            "incidents:write" => Scope::IncidentsWrite,
            "maintenance:read" => Scope::MaintenanceRead,
            "maintenance:write" => Scope::MaintenanceWrite,
            "status_page:read" => Scope::StatusPageRead,
            "status_page:write" => Scope::StatusPageWrite,
            "full_access" => Scope::FullAccess,
            _ => return None,
        })
    }

    /// The write scope that subsumes this read scope (`targets:read` →
    /// `targets:write`). Returns `None` for write scopes and `full_access`.
    const fn implied_by_write(self) -> Option<Scope> {
        Some(match self {
            Scope::TargetsRead => Scope::TargetsWrite,
            Scope::ChannelsRead => Scope::ChannelsWrite,
            Scope::IncidentsRead => Scope::IncidentsWrite,
            Scope::MaintenanceRead => Scope::MaintenanceWrite,
            Scope::StatusPageRead => Scope::StatusPageWrite,
            _ => return None,
        })
    }
}

/// The set of scopes a token carries. Empty means no capability (denies all);
/// in practice tokens always carry at least one scope.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeSet(HashSet<Scope>);

impl ScopeSet {
    /// The historical default — full access to everything.
    pub fn full_access() -> Self {
        ScopeSet(HashSet::from([Scope::FullAccess]))
    }

    /// Build from stored/submitted strings, silently dropping unknown entries.
    /// Forward-compatible: a token written by a newer version is downgraded to
    /// the scopes this version understands rather than failing the request.
    pub fn from_strs<'a>(it: impl IntoIterator<Item = &'a str>) -> Self {
        ScopeSet(it.into_iter().filter_map(Scope::parse).collect())
    }

    /// Does this set grant `required`? `full_access` grants everything; an
    /// exact match grants it; and a `*:write` scope grants the matching
    /// `*:read`.
    pub fn allows(&self, required: Scope) -> bool {
        self.0.contains(&Scope::FullAccess)
            || self.0.contains(&required)
            || required
                .implied_by_write()
                .is_some_and(|w| self.0.contains(&w))
    }

    /// Canonical, de-duplicated string forms (sorted for stable output).
    pub fn to_strings(&self) -> Vec<String> {
        let mut v: Vec<String> = self.0.iter().map(|s| s.as_str().to_owned()).collect();
        v.sort();
        v
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_access_allows_everything() {
        let s = ScopeSet::full_access();
        assert!(s.allows(Scope::TargetsWrite));
        assert!(s.allows(Scope::StatusPageRead));
    }

    #[test]
    fn write_implies_read_same_resource_only() {
        let s = ScopeSet::from_strs(["targets:write"]);
        assert!(s.allows(Scope::TargetsWrite));
        assert!(s.allows(Scope::TargetsRead)); // write ⇒ read
        assert!(!s.allows(Scope::ChannelsRead)); // not cross-resource
        assert!(!s.allows(Scope::ChannelsWrite));
    }

    #[test]
    fn read_only_denies_write() {
        let s = ScopeSet::from_strs(["targets:read", "channels:read"]);
        assert!(s.allows(Scope::TargetsRead));
        assert!(!s.allows(Scope::TargetsWrite));
        assert!(s.allows(Scope::ChannelsRead));
    }

    #[test]
    fn unknown_scopes_dropped_round_trip() {
        let s = ScopeSet::from_strs(["targets:read", "future:scope", "garbage"]);
        assert_eq!(s.to_strings(), vec!["targets:read"]);
        assert!(!s.allows(Scope::TargetsWrite));
    }

    #[test]
    fn parse_as_str_round_trip() {
        for raw in [
            "targets:read",
            "targets:write",
            "channels:read",
            "channels:write",
            "incidents:read",
            "incidents:write",
            "maintenance:read",
            "maintenance:write",
            "status_page:read",
            "status_page:write",
            "full_access",
        ] {
            assert_eq!(Scope::parse(raw).unwrap().as_str(), raw);
        }
        assert!(Scope::parse("nope").is_none());
    }
}
