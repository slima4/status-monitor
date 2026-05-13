use serde::Serialize;
use utoipa::ToSchema;

use crate::api::redaction::RedactInPlace;
use crate::api::types::TagCount;
use crate::domain::{CheckResult, Incident, PublicIncident, Target};

/// Standard envelope returned by every paginated list endpoint.
///
/// utoipa registers a distinct named schema per concrete instantiation. The
/// type aliases below pin each variant so `ApiDoc` can name them stably.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PageEnvelope<T>
where
    T: ToSchema,
{
    pub items: Vec<T>,
    pub total: u64,
    pub limit: u32,
    pub offset: u32,
}

impl<T> PageEnvelope<T>
where
    T: ToSchema,
{
    pub fn new(items: Vec<T>, total: u64, limit: u32, offset: u32) -> Self {
        Self {
            items,
            total,
            limit,
            offset,
        }
    }
}

impl<T> RedactInPlace for PageEnvelope<T>
where
    T: ToSchema + RedactInPlace,
{
    fn redact_in_place(&mut self) {
        for item in &mut self.items {
            item.redact_in_place();
        }
    }
}

pub type PageOfTarget = PageEnvelope<Target>;
pub type PageOfCheckResult = PageEnvelope<CheckResult>;
pub type PageOfIncident = PageEnvelope<Incident>;
pub type PageOfTagCount = PageEnvelope<TagCount>;
pub type PageOfPublicIncident = PageEnvelope<PublicIncident>;
