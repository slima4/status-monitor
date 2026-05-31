//! Where a resource was last changed from — surfaced as a "managed by" badge so
//! a UI edit isn't silently overwritten by the next `terraform apply`.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum WriteSource {
    #[default]
    Ui,
    Api,
    Terraform,
}

impl WriteSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ui => "ui",
            Self::Api => "api",
            Self::Terraform => "terraform",
        }
    }

    /// The stored `write_source` text. Unknown values resolve to `Ui`, the safe
    /// human default; the column's CHECK constraint keeps this unreachable.
    pub fn from_db(s: &str) -> Self {
        match s {
            "api" => Self::Api,
            "terraform" => Self::Terraform,
            _ => Self::Ui,
        }
    }

    /// Badge label for externally-managed resources, or `None` for `Ui` —
    /// UI-authored is the norm and carries no chip.
    pub fn managed_label(self) -> Option<&'static str> {
        match self {
            Self::Ui => None,
            Self::Api => Some("api"),
            Self::Terraform => Some("terraform"),
        }
    }
}
