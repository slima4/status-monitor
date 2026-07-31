# Plans and limits

This page covers the hosted service at `uptimepage.dev`. A self-hosted instance has no plan limits at all: it ships with the same code, and you edit the `plans` row yourself. See [Quotas and rate limits](../quotas.md) for the mechanism both modes share.

## The plans

| Plan | Who gets it |
|---|---|
| Standard | Every new account, free, no card |
| Founding | The first 1,000 accounts, granted at signup and kept for life, free |
| Pro | Coming |
| Team | Coming |

Prices, monitor counts, check intervals, history windows, seats, and status-page limits are on the [pricing page](https://uptimepage.dev/pricing), which is the number you are actually enforced at.

Browser flow monitors are counted separately from everything else, because each run drives a real browser rather than sending a request:

| Plan | Flow monitors |
|---|---|
| Standard | 1 |
| Founding | 3 |
| Pro | 5 |
| Team | 15 |

They still count toward your monitor total, and they run no faster than every five minutes. Creating one past the cap returns `422 QUOTA_EXCEEDED` naming `max_flow_checks`. See [Flow](../monitor-types.md#flow) for what one can and cannot do.

Founding is granted automatically while spots remain. There is nothing to claim and no code to enter. Once an account holds it, it keeps those limits for as long as the account is open, at no cost, and we do not downgrade it later.

## What happens at a limit

Resource quotas (monitors, seats, status pages, channels, tokens) are enforced at the write, inside the same statement that creates the row, so parallel creates settle exactly at the limit and never above it. Crossing one returns `422` with `QUOTA_EXCEEDED` and a `details` object naming the quota, your current count, and the limit.

Request budgets are per minute, per organization and per user, split into reads, writes, bulk operations, test runs, and check-now. Crossing one returns `429` with a `Retry-After` header. Checks themselves are never rate limited: the scheduler does not pass through that middleware, so a busy API does not slow your monitoring.

Two limits behave slightly differently. Pending invitations return `409 INVITATIONS_LIMIT`. A check interval below your plan floor returns `422 MIN_CHECK_INTERVAL`, and the floor is the higher of your plan's interval and the minimum for that monitor kind (twelve hours for domain expiry, an hour for TLS, five minutes for flow, a minute for heartbeat, ten seconds for the rest).

## Seeing where you stand

`GET /api/v1/orgs/{id}/usage` returns your plan plus current-versus-limit for every quota, the rate budgets, and the feature flags. The same numbers render as progress bars under **Settings → Usage** in the app. The reported limit is the enforced limit by construction: both read the same plan row and the same count query.

## Raising a limit

Delete monitors you no longer need, or write to us at <hello@uptimepage.dev> and say what you are running into. Paid plans are not open for self-service yet, so limit changes today are a conversation rather than a checkout.

If you would rather have no limits at all, run your own instance. It is the same product under AGPL, with no plan, no seat count, and no account with us. Start at [Deployment](../deployment.md).
