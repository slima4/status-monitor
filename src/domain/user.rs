use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Type;
use utoipa::ToSchema;
use uuid::Uuid;

/// Strongly-typed user id. Wrapping `Uuid` prevents accidentally passing a
/// `UserId` where an `OrgId` is expected (or vice versa).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type, ToSchema)]
#[serde(transparent)]
#[sqlx(transparent)]
#[schema(value_type = String, format = "uuid")]
pub struct UserId(pub Uuid);

impl std::fmt::Display for UserId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: UserId,
    pub email: String,
    pub display_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum AppTheme {
    #[default]
    Default,
    Terminal,
    Winter,
}

impl AppTheme {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Terminal => "terminal",
            Self::Winter => "winter",
        }
    }

    pub fn from_db(s: &str) -> Self {
        match s {
            "default" => Self::Default,
            "terminal" => Self::Terminal,
            "winter" => Self::Winter,
            other => {
                tracing::warn!(
                    value = other,
                    "unknown user.theme in DB, falling back to default"
                );
                Self::Default
            }
        }
    }

    pub const ALL: &'static [&'static str] = &["default", "terminal", "winter"];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_theme_matches_users_table_check_constraint() {
        let migration = include_str!("../../migrations/postgres/004_organizations.up.sql");
        for s in AppTheme::ALL {
            let needle = format!("'{}'", s);
            assert!(
                migration.contains(&needle),
                "AppTheme variant {:?} missing from users.theme CHECK",
                s
            );
        }
    }

    #[test]
    fn app_theme_from_db_round_trips_known_values() {
        for s in AppTheme::ALL {
            assert_eq!(AppTheme::from_db(s).as_str(), *s);
        }
        assert_eq!(AppTheme::from_db("garbage").as_str(), "default");
    }
}
