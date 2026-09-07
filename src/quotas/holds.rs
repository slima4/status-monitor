//! What an account keeps when its plan no longer covers everything it has.
//!
//! A plan can shrink under an account that is already using more than the new
//! tier sells. Deleting the excess would destroy work the customer paid for
//! once and may pay for again, so the excess is *held* instead: the row stays
//! exactly as it is and stops being served. Restoring the plan releases it
//! with nothing to repair, which is the same promise the read-time interval
//! and region clamps make.
//!
//! Two rules shape everything here:
//!
//! - **A hold is not a delete and not a pause.** `enabled` is the customer's
//!   own switch and is never touched, so a monitor they had paused comes back
//!   paused.
//! - **A held row still counts against the cap.** It occupies the slot it is
//!   waiting for; if it did not, an account could hold twenty monitors and
//!   then create twenty more on top of them.

use anyhow::Context;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::{AccountId, OrgId, Plan};
use crate::error::{AppError, Result};
use crate::storage::accounts::live_orgs;
use crate::storage::locks::{account_lock_key, advisory_xact_lock};
use crate::storage::orgs::record_audit_tx;

/// What one reconcile changed. Both counts are zero on the common path, where
/// the account fits its plan and the statement matches nothing.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Reconciled {
    pub held: usize,
    pub released: usize,
}

impl Reconciled {
    pub fn changed(self) -> bool {
        self.held > 0 || self.released > 0
    }
}

/// One row the reconcile moved, carried out of the statement so the audit
/// trail can name what stopped being watched.
#[derive(sqlx::FromRow)]
struct Moved {
    id: Uuid,
    org_id: Uuid,
    name: String,
    held: bool,
}

/// Cap on named rows in one audit entry: past this the count answers "what did
/// they lose" as well as a full list would. Matches the pause audit's bound.
const AUDIT_SAMPLE: usize = 100;

/// Brings one account's holds in line with its plan.
///
/// Idempotent, and deliberately not incremental: each statement recomputes the
/// whole held set from the current plan rather than diffing against what is
/// already held. Converging in one shot is what makes a repeat run a no-op and
/// makes release fall out of the same code as hold, with no second path to
/// keep in step.
///
/// The customer's own pick reaches this through the stored `plan_keep` flag
/// rather than an argument, which is what makes it survive: recomputing from
/// scratch means a pick held only in the request that set it would be undone
/// by the next sweep, putting back on hold the very row they chose to keep.
/// Set it with [`set_keep`].
///
/// Failing a pick, rows rank live-before-paused and then oldest-first: a
/// monitor that is switched off is the cheapest one to hold, and the oldest
/// are what the account was built around.
pub async fn reconcile_account(
    pool: &PgPool,
    account: AccountId,
    plan: &Plan,
    actor: Option<crate::domain::UserId>,
) -> Result<Reconciled> {
    let mut tx = pool.begin().await.context("reconcile_account: begin")?;
    // The same lock every pooled create takes. Without it a create racing a
    // reconcile can slip a monitor in against a count the reconcile has
    // already read, leaving the account one over its cap until the next sweep.
    advisory_xact_lock(&mut *tx, &account_lock_key(account))
        .await
        .context("reconcile_account: lock")?;

    let targets = reconcile_targets(&mut tx, account, plan).await?;
    let pages = reconcile_status_pages(&mut tx, account, plan).await?;

    let mut out = Reconciled::default();
    for (moved, held_action, release_action) in [
        (targets, "target.plan_hold", "target.plan_release"),
        (pages, "status_page.plan_hold", "status_page.plan_release"),
    ] {
        for row in &moved {
            if row.held {
                out.held += 1;
            } else {
                out.released += 1;
            }
        }
        audit_moves(&mut tx, moved, held_action, release_action, actor).await?;
    }

    tx.commit().await.context("reconcile_account: commit")?;
    Ok(out)
}

/// Monitors, under two caps at once. A flow monitor spends both a flow slot
/// and a monitor slot, so flows are ranked first and the monitors a flow cap
/// already held are out of the running for the monitor cap. Doing it the other
/// way would hold an ordinary monitor to make room for a flow the flow cap is
/// about to hold anyway.
async fn reconcile_targets(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account: AccountId,
    plan: &Plan,
) -> Result<Vec<Moved>> {
    let sql = format!(
        r#"WITH pool AS (
               SELECT id, org_id, name, created_at, enabled,
                      kind = 'flow' AS is_flow,
                      plan_keep AS kept
               FROM targets
               WHERE org_id IN ({orgs})
           ),
           flow_ranked AS (
               SELECT *,
                      CASE WHEN is_flow THEN row_number() OVER (
                          PARTITION BY is_flow
                          ORDER BY kept DESC, enabled DESC, created_at ASC, id ASC
                      ) END AS flow_rank
               FROM pool
           ),
           after_flow AS (
               SELECT *, coalesce(flow_rank > $2, false) AS flow_held FROM flow_ranked
           ),
           ranked AS (
               SELECT id, org_id, name, flow_held,
                      row_number() OVER (
                          PARTITION BY flow_held
                          ORDER BY kept DESC, enabled DESC, created_at ASC, id ASC
                      ) AS rank
               FROM after_flow
           ),
           want AS (
               SELECT id, org_id, name, (flow_held OR rank > $3) AS hold FROM ranked
           )
           UPDATE targets t
           SET plan_hold_at = CASE WHEN w.hold THEN now() ELSE NULL END,
               updated_at   = now()
           FROM want w
           WHERE t.id = w.id AND w.hold <> (t.plan_hold_at IS NOT NULL)
           RETURNING t.id, t.org_id, w.name, w.hold AS held"#,
        orgs = live_orgs("$1"),
    );
    sqlx::query_as(&sql)
        .bind(account.0)
        .bind(i64::from(plan.max_flow_checks))
        .bind(i64::from(plan.max_targets))
        .fetch_all(&mut **tx)
        .await
        .context("reconcile_account: targets")
        .map_err(AppError::from)
}

async fn reconcile_status_pages(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account: AccountId,
    plan: &Plan,
) -> Result<Vec<Moved>> {
    let sql = format!(
        r#"WITH ranked AS (
               SELECT id, org_id, name,
                      row_number() OVER (
                          ORDER BY plan_keep DESC, enabled DESC, created_at ASC, id ASC
                      ) AS rank
               FROM status_pages
               WHERE org_id IN ({orgs})
           ),
           want AS (
               SELECT id, org_id, name, rank > $2 AS hold FROM ranked
           )
           UPDATE status_pages sp
           SET plan_hold_at = CASE WHEN w.hold THEN now() ELSE NULL END,
               updated_at   = now()
           FROM want w
           WHERE sp.id = w.id AND w.hold <> (sp.plan_hold_at IS NOT NULL)
           RETURNING sp.id, sp.org_id, w.name, w.hold AS held"#,
        orgs = live_orgs("$1"),
    );
    sqlx::query_as(&sql)
        .bind(account.0)
        .bind(i64::from(plan.max_status_pages))
        .fetch_all(&mut **tx)
        .await
        .context("reconcile_account: status pages")
        .map_err(AppError::from)
}

/// One audit row per org per direction. An account's orgs are audited
/// separately because `org_audit_log` is org-scoped, and whoever reads one
/// org's trail should see what left that org, not the whole account's total.
async fn audit_moves(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    moved: Vec<Moved>,
    held_action: &str,
    release_action: &str,
    actor: Option<crate::domain::UserId>,
) -> Result<()> {
    let mut by_org: std::collections::BTreeMap<(Uuid, bool), Vec<Moved>> = Default::default();
    for row in moved {
        by_org.entry((row.org_id, row.held)).or_default().push(row);
    }
    for ((org, held), rows) in by_org {
        let sample: Vec<_> = rows
            .iter()
            .take(AUDIT_SAMPLE)
            .map(|r| json!({ "id": r.id, "name": r.name }))
            .collect();
        let action = if held { held_action } else { release_action };
        record_audit_tx(
            tx,
            OrgId(org),
            actor,
            action,
            json!({
                "count": rows.len(),
                "items": sample,
                "truncated": rows.len() > AUDIT_SAMPLE,
            }),
        )
        .await
        .context("reconcile_account: audit")?;
    }
    Ok(())
}

/// Accounts whose holds may be out of step with their plan, each paired with
/// one of its live orgs so the caller can resolve the plan through the same
/// cached path every request uses (overrides and add-ons included, since both
/// are per-account and any of its orgs resolves the identical plan).
///
/// Two ways in: the account already holds something, which the partial indexes
/// answer for nothing on an install that holds nothing, or it is over one of
/// the three caps a hold can act on. Anything else has no work to do, and
/// reconciling it would take a lock and write an audit row for a no-op.
pub async fn accounts_needing_reconcile(pool: &PgPool) -> Result<Vec<(AccountId, OrgId)>> {
    // Built here rather than inline so the three caps cannot drift apart.
    //
    // CASE, not `guard AND cast`. Postgres does not promise to evaluate the
    // arms of an AND left to right, so a guard beside the cast can be reordered
    // behind it and the malformed value reaches the cast anyway. Only CASE
    // orders the test before the branch. A test can pass on this either way,
    // since whether it breaks depends on the plan the planner happens to pick.
    let lowered = ["max_targets", "max_flow_checks", "max_status_pages"]
        .map(|f| {
            format!(
                "CASE WHEN jsonb_typeof(po.override_json->'{f}') = 'number' \
                      THEN (po.override_json->>'{f}')::numeric < p.{f} \
                      ELSE false END"
            )
        })
        .join(" OR ");
    let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(&format!(
        r#"/* SAFE: the sweep is account-wide by definition; it reconciles every
              tenant's holds against its own plan and returns no tenant data */
           WITH live AS (
               SELECT id, account_id FROM organizations WHERE deleted_at IS NULL
           ),
           acct AS (
               SELECT account_id, min(id::text)::uuid AS any_org
               FROM live GROUP BY account_id
           )
           SELECT a.id, acct.any_org
           FROM acct
           JOIN accounts a ON a.id = acct.account_id
           JOIN plans p ON p.id = a.plan_id
           -- Counted in separate laterals on purpose: joining targets and
           -- status pages in one pass multiplies each by the other's row
           -- count, which reads as "over cap" for any account with two pages.
           CROSS JOIN LATERAL (
               SELECT count(*) AS n,
                      count(*) FILTER (WHERE t.kind = 'flow') AS flows,
                      bool_or(t.plan_hold_at IS NOT NULL) AS holds
               FROM targets t
               JOIN live lt ON lt.id = t.org_id AND lt.account_id = a.id
           ) tc
           CROSS JOIN LATERAL (
               SELECT count(*) AS n, bool_or(sp.plan_hold_at IS NOT NULL) AS holds
               FROM status_pages sp
               JOIN live lp ON lp.id = sp.org_id AND lp.account_id = a.id
           ) pc
           WHERE coalesce(tc.holds, false) OR coalesce(pc.holds, false)
              OR tc.n > p.max_targets
              OR tc.flows > p.max_flow_checks
              OR pc.n > p.max_status_pages
              -- An override *replaces* a named cap, so it can put the effective
              -- ceiling below the plans row this query reads, and the reconcile
              -- that follows resolves the effective plan. Comparing only
              -- against the raw row would skip exactly those accounts.
              --
              -- Only a lowering override matters. Admitting every overridden
              -- account instead would sweep the whole fleet, since an override
              -- is also how capacity is granted. Add-ons need no test at all:
              -- they only ever add, so they can raise an account out of scope
              -- but never hide one that belongs in it.
              --
              -- The value is hand-written operator JSON with no write path to
              -- validate it, and a bad cast aborts the whole statement, not one
              -- row: a single mistyped override would stop holds *and releases*
              -- for every account until someone found it. Hence the CASE built
              -- above, and numeric rather than int so a value too large is out
              -- of scope rather than an error.
              OR EXISTS (
                  SELECT 1 FROM plan_overrides po
                  WHERE po.account_id = a.id
                    AND (po.expires_at IS NULL OR po.expires_at > now())
                    AND ({lowered})
              )"#
    ))
    .fetch_all(pool)
    .await
    .context("accounts_needing_reconcile")?;
    Ok(rows
        .into_iter()
        .map(|(a, o)| (AccountId(a), OrgId(o)))
        .collect())
}

/// One sweep over every account whose holds may have drifted from its plan.
///
/// This is not only the safety net the daily cadence suggests: until a plan
/// change has a write path of its own, a plan moves by an operator's `UPDATE`,
/// which notifies nothing. The sweep is what turns that into holds, so a
/// downgrade lands within a day whatever route it arrived by.
///
/// One account failing does not stop the others: a plan that will not resolve
/// leaves that account exactly as it is and logs, in the same spirit as the
/// read-time clamp's infallible `govern`.
pub async fn sweep(pool: &PgPool, quotas: &crate::quotas::QuotaService) -> Result<u64> {
    let mut moved = 0u64;
    for (account, org) in accounts_needing_reconcile(pool).await? {
        let plan = match quotas.limit_for_org(org).await {
            Ok(p) => p,
            Err(err) => {
                tracing::warn!(account = %account.0, error = %err, "plan holds: plan lookup failed");
                continue;
            }
        };
        match reconcile_account(pool, account, &plan, None).await {
            Ok(r) => {
                if r.changed() {
                    tracing::info!(
                        account = %account.0, plan = %plan.id,
                        held = r.held, released = r.released,
                        "plan holds reconciled"
                    );
                    moved += (r.held + r.released) as u64;
                }
            }
            Err(err) => {
                tracing::warn!(account = %account.0, error = %err, "plan holds: reconcile failed")
            }
        }
    }
    Ok(moved)
}

/// One row a plan is holding, for the customer to look at before choosing
/// differently.
#[derive(sqlx::FromRow)]
pub struct Held {
    pub id: Uuid,
    pub org_id: Uuid,
    pub name: String,
    pub held_at: chrono::DateTime<chrono::Utc>,
}

/// Everything one account currently has held, monitors then status pages,
/// oldest hold first so the list reads in the order the plan gave them up.
pub async fn list_held(pool: &PgPool, account: AccountId) -> Result<(Vec<Held>, Vec<Held>)> {
    let orgs = live_orgs("$1");
    let targets = sqlx::query_as(&format!(
        "SELECT id, org_id, name, plan_hold_at AS held_at FROM targets \
         WHERE org_id IN ({orgs}) AND plan_hold_at IS NOT NULL \
         ORDER BY plan_hold_at ASC, created_at ASC, id ASC"
    ))
    .bind(account.0)
    .fetch_all(pool)
    .await
    .context("list_held: targets")?;
    let pages = sqlx::query_as(&format!(
        "SELECT id, org_id, name, plan_hold_at AS held_at FROM status_pages \
         WHERE org_id IN ({orgs}) AND plan_hold_at IS NOT NULL \
         ORDER BY plan_hold_at ASC, created_at ASC, id ASC"
    ))
    .bind(account.0)
    .fetch_all(pool)
    .await
    .context("list_held: status pages")?;
    Ok((targets, pages))
}

/// Records which of the account's rows the customer wants kept, replacing any
/// previous answer. Ids that are not the account's own are ignored rather than
/// rejected, so a stale page listing a since-deleted monitor still saves.
///
/// Stored, not applied: the caller reconciles afterwards, and every later run
/// reads the same flag, which is what stops the daily sweep undoing the choice.
pub async fn set_keep(pool: &PgPool, account: AccountId, keep: &[Uuid]) -> Result<()> {
    let orgs = live_orgs("$1");
    for table in ["targets", "status_pages"] {
        sqlx::query(&format!(
            "UPDATE {table} SET plan_keep = (id = ANY($2)) \
             WHERE org_id IN ({orgs}) AND plan_keep <> (id = ANY($2))"
        ))
        .bind(account.0)
        .bind(keep)
        .execute(pool)
        .await
        .context("set_keep")?;
    }
    Ok(())
}

/// Whether this account is holding anything at all. Answered from the partial
/// indexes, so the overwhelmingly common "nothing held" case costs an index
/// probe rather than a scan.
pub async fn holds_anything(pool: &PgPool, account: AccountId) -> Result<bool> {
    let orgs = live_orgs("$1");
    let (any,): (bool,) = sqlx::query_as(&format!(
        "SELECT EXISTS (SELECT 1 FROM targets \
                        WHERE org_id IN ({orgs}) AND plan_hold_at IS NOT NULL) \
             OR EXISTS (SELECT 1 FROM status_pages \
                        WHERE org_id IN ({orgs}) AND plan_hold_at IS NOT NULL)"
    ))
    .bind(account.0)
    .fetch_one(pool)
    .await
    .context("holds_anything")?;
    Ok(any)
}

/// Gives back what a freed slot can now cover, right after a delete.
///
/// The daily sweep would find this anyway, but a customer who deletes a monitor
/// precisely to get another one running should not wait a day to see it. Does
/// nothing, and takes no lock, for an account holding nothing.
///
/// Failure is logged and swallowed: the delete the caller just made is done and
/// must not be reported as failed because a release could not be computed, and
/// the sweep is the backstop.
pub async fn release_after_delete(pool: &PgPool, quotas: &crate::quotas::QuotaService, org: OrgId) {
    let done = async {
        let account = crate::storage::accounts::account_for_org(pool, org).await?;
        if !holds_anything(pool, account).await? {
            return Ok(Reconciled::default());
        }
        let plan = quotas.limit_for_org(org).await?;
        reconcile_account(pool, account, &plan, None).await
    }
    .await;
    match done {
        Ok(r) if r.changed() => {
            tracing::info!(org = %org.0, released = r.released, "plan holds released after delete")
        }
        Ok(_) => {}
        Err(err) => tracing::warn!(org = %org.0, error = %err, "plan holds: release after delete"),
    }
}
