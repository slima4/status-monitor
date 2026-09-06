//! Plan ceilings applied to handed-out work. Never written back to the row.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::domain::{OrgId, Plan, Target, min_interval_secs_for_kind};
use crate::error::Result;
use crate::quotas::QuotaService;
use crate::storage::admin::{EnabledTargetSource, EnabledTargetStream, PublicTargetCursor};

/// The same floor the write path enforces: a row stored below its kind's
/// minimum predates that minimum, and handing it out unclamped would probe at
/// a rate the API now refuses to create.
pub fn governed_interval(requested: Duration, plan: &Plan, kind: &str) -> Duration {
    let plan_floor = u64::try_from(plan.min_check_interval_secs).unwrap_or(0);
    let floor = Duration::from_secs(plan_floor.max(min_interval_secs_for_kind(kind)));
    requested.max(floor)
}

/// Infallible: one org whose plan will not resolve must not stop the batch,
/// which spans every tenant. A monitor running at its stored rate is a far
/// smaller fault than one that stops.
pub async fn resolve_plans(
    quotas: &QuotaService,
    orgs: impl IntoIterator<Item = OrgId>,
) -> HashMap<OrgId, Option<Arc<Plan>>> {
    let mut plans: HashMap<OrgId, Option<Arc<Plan>>> = HashMap::new();
    for org in orgs {
        if plans.contains_key(&org) {
            continue;
        }
        let plan = match quotas.limit_for_org(org).await {
            Ok(plan) => Some(plan),
            Err(err) => {
                tracing::warn!(
                    org_id = %org.0,
                    error = %err,
                    "plan lookup failed; running this org's monitors as stored"
                );
                None
            }
        };
        plans.insert(org, plan);
    }
    plans
}

pub async fn govern(quotas: &QuotaService, targets: &mut [(OrgId, Target)]) {
    let orgs: Vec<OrgId> = targets.iter().map(|(org, _)| *org).collect();
    let plans = resolve_plans(quotas, orgs).await;
    govern_with(&plans, targets);
}

/// For a caller that already resolved the plans, so the same resolution can
/// also decide whether the work needed sending at all.
pub fn govern_with(plans: &HashMap<OrgId, Option<Arc<Plan>>>, targets: &mut [(OrgId, Target)]) {
    for (org, target) in targets.iter_mut() {
        // A heartbeat's interval is the schedule its sender promised, not a
        // probe rate we pay for; slowing it only delays the missed-ping alarm.
        if target.check.is_passive() {
            continue;
        }
        if let Some(Some(plan)) = plans.get(org) {
            target.interval = governed_interval(target.interval, plan, target.check.kind());
        }
    }
}

/// Stable over the inputs that move an interval, so a tier change invalidates a
/// cached pull that no target row has touched.
pub fn plan_digest(plans: &HashMap<OrgId, Option<Arc<Plan>>>) -> String {
    let mut parts: Vec<String> = plans
        .iter()
        .map(|(org, plan)| match plan {
            Some(p) => format!("{}:{}:{}", org.0, p.id, p.min_check_interval_secs),
            None => format!("{}:?", org.0),
        })
        .collect();
    parts.sort_unstable();
    crate::auth::sha256_hex(&parts.join(","))
}

/// Applies the plan floor to every hand-out of enabled targets, whether the
/// consumer takes the full snapshot (scheduler) or pages through it (incident
/// writer). Both must see the same interval: the writer sizes its lookback
/// window from it, and a window sized to the stored rate cannot hold enough
/// results from a monitor the plan has slowed down.
pub struct PlanGoverned<S: ?Sized> {
    inner: Arc<S>,
    quotas: Arc<QuotaService>,
}

impl<S: ?Sized> PlanGoverned<S> {
    pub fn new(inner: Arc<S>, quotas: Arc<QuotaService>) -> Self {
        Self { inner, quotas }
    }
}

#[async_trait]
impl<S: EnabledTargetSource + ?Sized> EnabledTargetSource for PlanGoverned<S> {
    async fn list_all_enabled_targets(&self) -> Result<Vec<(OrgId, Target)>> {
        let mut targets = self.inner.list_all_enabled_targets().await?;
        govern(&self.quotas, &mut targets).await;
        Ok(targets)
    }
}

#[async_trait]
impl<S: EnabledTargetStream + ?Sized> EnabledTargetStream for PlanGoverned<S> {
    async fn next_enabled_target_page(
        &self,
        after: Option<PublicTargetCursor>,
        limit: usize,
    ) -> Result<Vec<(OrgId, Target)>> {
        let mut targets = self.inner.next_enabled_target_page(after, limit).await?;
        govern(&self.quotas, &mut targets).await;
        Ok(targets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quotas::service::unlimited_plan;

    fn plan_with_floor(secs: i32) -> Plan {
        Plan {
            min_check_interval_secs: secs,
            ..unlimited_plan()
        }
    }

    #[test]
    fn a_monitor_slower_than_the_floor_keeps_its_own_interval() {
        let plan = plan_with_floor(60);
        let got = governed_interval(Duration::from_secs(300), &plan, "http");
        assert_eq!(got, Duration::from_secs(300));
    }

    #[test]
    fn a_monitor_faster_than_the_floor_is_slowed_to_it() {
        let plan = plan_with_floor(180);
        let got = governed_interval(Duration::from_secs(60), &plan, "http");
        assert_eq!(got, Duration::from_secs(180));
    }

    #[test]
    fn a_floor_equal_to_the_interval_changes_nothing() {
        let plan = plan_with_floor(60);
        let got = governed_interval(Duration::from_secs(60), &plan, "http");
        assert_eq!(got, Duration::from_secs(60));
    }

    #[test]
    fn the_unlimited_plan_imposes_no_floor() {
        let got = governed_interval(Duration::from_secs(10), &unlimited_plan(), "http");
        assert_eq!(got, Duration::from_secs(10));
    }

    #[test]
    fn a_kind_floor_above_the_plan_floor_wins() {
        let plan = plan_with_floor(30);
        let got = governed_interval(Duration::from_secs(3600), &plan, "domain_expiry");
        assert_eq!(got, Duration::from_secs(43_200));
    }

    #[test]
    fn a_plan_floor_above_the_kind_floor_still_wins() {
        let plan = plan_with_floor(180);
        let got = governed_interval(Duration::from_secs(60), &plan, "http");
        assert_eq!(got, Duration::from_secs(180));
    }

    #[test]
    fn a_negative_floor_is_treated_as_no_floor() {
        let plan = plan_with_floor(-30);
        let got = governed_interval(Duration::from_secs(45), &plan, "http");
        assert_eq!(got, Duration::from_secs(45));
    }
}
