//! One reminder for a heartbeat nobody finished wiring up.
//!
//! A heartbeat is not evaluated until its first ping, so an unwired one is
//! silent. That is the right answer to "has this job failed" and the wrong
//! answer to "did you forget about me".

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::email::{EmailAddress, EmailTemplate, TransactionalEmail};
use crate::notifier::EmailDelivery;

/// Long enough that a monitor created ahead of a deploy is not nagged mid-work,
/// short enough that the reminder still lands while the intent is fresh. Dead
/// Man's Snitch settled on the same three days.
const NUDGE_AFTER_DAYS: i32 = 3;

/// Runaway guard, not a page size: the qualifying population is tiny, and
/// anything over the cap is picked up next tick.
const BATCH: i64 = 200;

/// Everything one reminder needs, resolved in a single statement so the sweep
/// does not fan out a query per monitor.
#[derive(sqlx::FromRow)]
struct Candidate {
    org_id: Uuid,
    target_id: Uuid,
    monitor_name: String,
    org_name: Option<String>,
    created_at: DateTime<Utc>,
    recipient: String,
}

pub struct NudgeConfig {
    pub email: EmailDelivery,
    /// App base URL. Empty disables the deep link rather than mailing a
    /// half-formed one.
    pub public_base_url: String,
    pub docs_url: Option<String>,
}

/// Sends at most one reminder per unwired heartbeat and returns how many went
/// out. Idempotent: `nudged_at` is stamped only after the provider accepts the
/// mail, so a transport outage retries on the next tick instead of losing the
/// only reminder a monitor ever gets.
pub async fn nudge_unwired_heartbeats(
    pool: &PgPool,
    cfg: &NudgeConfig,
) -> Result<u64, sqlx::Error> {
    let candidates: Vec<Candidate> = sqlx::query_as(
        // The monitor's own owner first; the org's owner only when the monitor
        // names nobody, so exactly one person is told either way. A monitor
        // whose org has no reachable owner is skipped rather than broadcast.
        "SELECT hm.org_id, hm.target_id, t.name AS monitor_name, o.name AS org_name, hm.created_at, \
                COALESCE(mon_owner.email, org_owner.email) AS recipient \
           FROM heartbeat_monitors hm \
           JOIN targets t ON t.id = hm.target_id \
           JOIN organizations o ON o.id = hm.org_id AND o.deleted_at IS NULL \
           LEFT JOIN users mon_owner \
                  ON mon_owner.id = t.owner_user_id AND mon_owner.deleted_at IS NULL \
           LEFT JOIN LATERAL ( \
                SELECT u.email FROM memberships m \
                  JOIN users u ON u.id = m.user_id \
                 WHERE m.org_id = hm.org_id AND m.role = 'owner' AND u.deleted_at IS NULL \
                 ORDER BY u.email LIMIT 1 \
           ) org_owner ON true \
          WHERE hm.first_ping_at IS NULL \
            AND hm.nudged_at IS NULL \
            AND t.enabled \
            AND hm.created_at < now() - make_interval(days => $1) \
            AND COALESCE(mon_owner.email, org_owner.email) IS NOT NULL \
          ORDER BY hm.created_at \
          LIMIT $2",
    )
    .bind(NUDGE_AFTER_DAYS)
    .bind(BATCH)
    .fetch_all(pool)
    .await?;

    let mut sent = 0;
    for c in candidates {
        if send_one(&c, cfg).await {
            sqlx::query(
                "UPDATE heartbeat_monitors SET nudged_at = now() \
                 WHERE org_id = $1 AND target_id = $2",
            )
            .bind(c.org_id)
            .bind(c.target_id)
            .execute(pool)
            .await?;
            sent += 1;
        }
    }
    Ok(sent)
}

async fn send_one(c: &Candidate, cfg: &NudgeConfig) -> bool {
    let base = cfg.public_base_url.trim_end_matches('/');
    let template = EmailTemplate::HeartbeatNeverPinged {
        monitor_name: c.monitor_name.clone(),
        waiting_secs: (Utc::now() - c.created_at).num_seconds().max(0),
        monitor_url: (!base.is_empty()).then(|| format!("{base}/targets/{}", c.target_id)),
        docs_url: cfg.docs_url.clone(),
        org_name: c.org_name.clone(),
    };
    let outgoing = TransactionalEmail {
        from: EmailAddress::new(cfg.email.from_address.clone(), cfg.email.from_name.clone()),
        to: EmailAddress::new(c.recipient.clone(), c.recipient.clone()),
        template,
    };
    match cfg.email.sender.send(outgoing).await {
        Ok(_) => true,
        Err(err) => {
            tracing::warn!(
                target_id = %c.target_id,
                error = %err,
                "heartbeat nudge not sent, retrying next tick"
            );
            false
        }
    }
}
