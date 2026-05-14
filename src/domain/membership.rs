use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::domain::org::OrgId;
use crate::domain::user::UserId;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Owner,
    Member,
}

impl Role {
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Member => "member",
        }
    }

    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "owner" => Some(Self::Owner),
            "member" => Some(Self::Member),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Membership {
    pub user_id: UserId,
    pub org_id: OrgId,
    pub role: Role,
    pub created_at: DateTime<Utc>,
}
