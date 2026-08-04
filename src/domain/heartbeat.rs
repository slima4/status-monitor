//! What a job reports about its own run, as opposed to what the scheduler
//! concludes from silence. `/ping/{token}` on its own is a [`PingSignal::Success`];
//! the trailing segment forms carry the rest.

use serde::{Deserialize, Serialize};

/// One inbound signal on a heartbeat's ping URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PingSignal {
    Start,
    Success,
    Fail,
}

impl PingSignal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Success => "success",
            Self::Fail => "fail",
        }
    }

    pub fn as_enum8(self) -> i8 {
        match self {
            Self::Start => 1,
            Self::Success => 2,
            Self::Fail => 3,
        }
    }

    /// Whether this signal ends a run, and so closes an open `start`.
    pub fn is_finish(self) -> bool {
        matches!(self, Self::Success | Self::Fail)
    }
}

/// A parsed ping: the signal plus the exit status it carried, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ping {
    pub signal: PingSignal,
    pub exit_code: Option<u8>,
}

impl Ping {
    pub const SUCCESS: Self = Self {
        signal: PingSignal::Success,
        exit_code: None,
    };

    /// Parse the segment after the token. `curl $URL/$?` is the intended shape
    /// of the numeric form, so 0 succeeds and anything else fails. Unknown
    /// words are rejected rather than treated as success — a typo must not
    /// silently keep a broken job's monitor green.
    pub fn parse(segment: &str) -> Option<Self> {
        match segment {
            "start" => Some(Self {
                signal: PingSignal::Start,
                exit_code: None,
            }),
            "fail" => Some(Self {
                signal: PingSignal::Fail,
                exit_code: None,
            }),
            // The bare URL already means this; spelling it out reads better
            // beside `/start` and `/fail` in a script.
            "success" => Some(Self::SUCCESS),
            _ => segment.parse::<u8>().ok().map(|code| Self {
                signal: if code == 0 {
                    PingSignal::Success
                } else {
                    PingSignal::Fail
                },
                exit_code: Some(code),
            }),
        }
    }
}

/// One accepted signal, as history. Unlike a `CheckResult` this is the job's
/// own account of itself, so it is kept whether or not it changed the verdict.
#[derive(Debug, Clone)]
pub struct HeartbeatPingRecord {
    pub org_id: uuid::Uuid,
    pub target_id: uuid::Uuid,
    pub received_at: chrono::DateTime<chrono::Utc>,
    pub signal: PingSignal,
    pub exit_code: Option<u8>,
    /// `/start`→finish of the run this signal closed.
    pub duration_ms: Option<u32>,
    /// Job output as POSTed, already truncated to what is kept.
    pub body: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_and_exit_codes_parse() {
        assert_eq!(Ping::parse("start").unwrap().signal, PingSignal::Start);
        assert_eq!(Ping::parse("fail").unwrap().signal, PingSignal::Fail);
        assert_eq!(Ping::parse("success").unwrap(), Ping::SUCCESS);
        assert_eq!(
            Ping::parse("0").unwrap(),
            Ping {
                signal: PingSignal::Success,
                exit_code: Some(0)
            }
        );
        assert_eq!(
            Ping::parse("137").unwrap(),
            Ping {
                signal: PingSignal::Fail,
                exit_code: Some(137)
            }
        );
    }

    #[test]
    fn unknown_segments_are_rejected() {
        for s in ["", "ok", "done", "-1", "256", "1 "] {
            assert!(Ping::parse(s).is_none(), "{s} must not parse");
        }
    }
}
