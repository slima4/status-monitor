# Probe regions

This page covers the hosted service at `uptimepage.dev`. If you run your own instance, you choose your own regions and add agents yourself: see [Multi-region probes](../multi-region.md).

## Where we check from

| Region | Location |
|---|---|
| `us-east` | Secaucus, United States |
| `eu-helsinki` | Helsinki, Finland |
| `apac-sg` | Singapore |

More regions are rolling out. Existing monitors pick them up as soon as you assign them, with no migration and no change to your history.

## Assigning regions to a monitor

A monitor runs in every region you assign it. Each result is stored with the region that produced it, so the monitor detail page can show per-region latency and you can filter any chart down to one region.

Standard is capped at three regions per monitor. Founding and the paid plans have no cap, so they pick up new regions as they open.

## How a failure is decided

Each monitor carries a region policy that says how many regions have to agree before it counts as down:

- `any` — one failing region is enough
- `majority` — the default, more than half of the regions currently reporting results
- `all` — every assigned region
- a count, for example `{"count": 2}`

`majority` is the default because it absorbs the common case of one probe path having a bad minute without waking anybody. Use `any` when you want the earliest possible signal and can tolerate more noise, and `all` for a monitor that is only meaningful when it is globally unreachable.

Region agreement and the consecutive-failure threshold are two separate gates, applied in that order. A region counts as failing only after `alert_confirmations` failures in a row from that region alone (default 2), and then the region policy decides whether enough failing regions have agreed to open an incident. Recovery works the same way in reverse: the incident closes once the failing regions drop back below the policy and some region has a matching run of passing checks.

Regions push their results to the control plane, and the policy counts the regions that are actually reporting. A region that stops reporting, for example because it lost connectivity, drops out of the vote entirely: it is not counted as down, and the policy applies to the regions still delivering results. Missing data alone never opens an incident and never closes one.

## Practical notes

Checks from different regions leave from different addresses, so an allowlist on your side needs every region you assign. If you allowlist by IP, write to <hello@uptimepage.dev> before you rely on it, because the egress addresses are not guaranteed stable on every region yet.

Browser flow monitors run only in regions that ship a browser engine. A flow monitor's region set is narrowed to those when you save it, so it never silently sits unassigned.

Heartbeat monitors never use regions at all. They are passive: your system pings us, and we evaluate the gap centrally.
