# Probe regions

This page covers the hosted service at `uptimepage.dev`. If you run your own instance, you choose your own regions and add agents yourself: see [Multi-region probes](../multi-region.md).

## Where we check from

| Region | Location |
|---|---|
| `us-east` | North Virginia, United States |
| `eu-helsinki` | Helsinki, Finland |
| `apac-sg` | Singapore |

More regions are rolling out. Existing monitors pick them up as soon as you assign them, with no migration and no change to your history.

## Assigning regions to a monitor

A monitor runs in every region you assign it. Each result is stored with the region that produced it, so the monitor detail page can show per-region latency and you can filter any chart down to one region.

Standard is capped at three regions per monitor, which is every region we run today. Founding and the paid plans have no cap, so they pick up new regions as they open.

## How a failure is decided

Each monitor carries a region policy that says how many regions have to agree before it counts as down:

- `any` — one failing region is enough
- `majority` — the default, more than half of the assigned regions
- `all` — every assigned region
- a count, for example `{"count": 2}`

`majority` is the default because it absorbs the common case of one probe path having a bad minute without waking anybody. Use `any` when you want the earliest possible signal and can tolerate more noise, and `all` for a monitor that is only meaningful when it is globally unreachable.

Region agreement is separate from the consecutive-failure threshold. A monitor with `alert_confirmations = 2` still needs two consecutive failing rounds before it alerts, and each of those rounds is judged by the region policy.

## Practical notes

Checks from different regions leave from different addresses, so an allowlist on your side needs all three. If you allowlist by IP, write to <hello@uptimepage.dev> before you rely on it, because the egress addresses are not guaranteed stable on every region yet.

Browser flow monitors run only in regions that ship a browser engine. A flow monitor's region set is narrowed to those when you save it, so it never silently sits unassigned.

Heartbeat monitors never use regions at all. They are passive: your system pings us, and we evaluate the gap centrally.
