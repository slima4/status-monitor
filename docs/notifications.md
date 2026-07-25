# Notifications

A monitor going down is only useful if it reaches someone. A **notification channel** is a place alerts are delivered: a Slack channel, a phone number, an on-call rotation in PagerDuty, your own webhook. Channels belong to the org, and each monitor binds the ones that should hear about it.

Manage them under **Settings → Notifications**. The endpoint contract for everything below is in [REST API](api.md#notification-channels).

## The model

Three pieces, deliberately separate:

- **The channel** is the destination and its credentials. Created once, reused by any number of monitors.
- **The binding** attaches a channel to a monitor. A monitor with no bindings still opens incidents and still shows on a status page; it just pages nobody.
- **The policy** lives on the monitor, not the channel: how many failures before alerting, how often to remind, whether recovery is announced.

The point of the split is blast radius. A noisy marketing-site monitor and your payment API can share one Slack channel while alerting on completely different thresholds, and neither has to duplicate the webhook URL.

## Choosing a channel type

| Type | What you provide | Notes |
|---|---|---|
| Slack, Discord, Teams, Google Chat | An incoming webhook URL | Discord, Teams and Google Chat URLs are host-checked, so a wrong-vendor paste is refused up front. A Slack URL is only checked for `https`, so verify it with a test send |
| Telegram | One-tap link, or your own bot token and chat id | The one-tap flow is available where the platform runs a central bot |
| WhatsApp | One-tap link, or Business Cloud API credentials and a template | Bring-your-own needs an approved one-parameter template |
| SMS | Credentials for your own gateway: Twilio, Vonage, Telnyx, Plivo, or Sinch | One message per alert, trimmed to bound per-segment cost |
| Email | One address | Must be verified before anything is delivered, see below |
| PagerDuty | An Events API v2 routing key | The only type that drives the destination's own incident lifecycle |
| ntfy, Pushover | Server and topic, or app and user key | Urgency maps to the service's own priority levels |
| Webhook | An HTTPS URL, optional headers, optional signing secret | The escape hatch for anything not listed above |

PagerDuty is worth calling out: opens send a `trigger` and resolutions send a `resolve`, correlated by the incident id, so one incident here maps to exactly one PagerDuty alert that opens and closes with it rather than a pile of unrelated pages.

If you use the generic webhook with a signing secret, every delivery carries a timestamp and an HMAC-SHA256 signature so your receiver can verify the payload came from us and reject replays.

## Creating one

Add the channel, then **test now** before saving. The test sends one clearly labelled synthetic alert through the real transport, so a wrong webhook URL or an expired token surfaces immediately instead of during your first outage. Testing works on a saved channel too, including a disabled one.

Two types need a second step:

- **Email** is verification-gated. The channel is created unverified and a single-use link, valid 24 hours, goes to the address. Until it is confirmed, every delivery fails. Changing the address resets the gate and sends a new mail. There is a daily cap on resends.
- **Slack and Discord** can be connected with a one-tap button where the operator has configured it: their consent screen picks the destination, and you land back on the ready-made channel. Otherwise paste a webhook URL.

Secrets are sealed at rest and never shown again. On edit they stay masked behind a replace toggle, and leaving the toggle off keeps the stored value untouched.

### Delegating the connect step

When the credentials belong to someone outside the org, say the Slack workspace admin or the person who owns the shared inbox, you do not need to chase them for secrets. **Settings → Notifications → delegate the connect step** mints a single-use `/c/{code}` link. Whoever opens it can connect exactly one channel to your workspace and nothing else, with no account needed; the link expires after 7 days and can be revoked before use. The same flow is scriptable through the delegate endpoints in the [REST API](api.md#notification-channels).

## Binding a monitor

The monitor form has a **Notifications** section listing your channels with a checkbox each. It only appears once the org has at least one channel, so create the channel first.

Alongside the bindings sit the controls that decide when they fire:

| Setting | Default | What it does |
|---|---|---|
| Consecutive failures | 2 | Failing checks before an incident opens. The same count of passing checks closes it, which is what damps flapping. |
| Region agreement | majority | How many probe regions must agree before it counts as down. See [Probe regions](hosted/regions.md). |
| Remind while down | every hour | How often to re-notify while an outage stays unacknowledged. Acknowledging or resolving stops the reminders; set it to off for none. |
| Announce recovery | on | Whether the "back up" message is sent. |

The reminder interval is the setting most worth tuning. An hour is right for a customer-facing API, and far too often for a nightly batch endpoint you already know is flaky.

## What gets delivered

One notification when an incident opens, reminders on your interval while it stays unacknowledged, and one on recovery if announcements are on. Alerts are driven by the incident engine, not by individual check failures, so a monitor failing sixty times in an hour produces one incident and one alert, not sixty.

Failed deliveries retry with exponential backoff and are dead-lettered after the attempt cap. Per-incident delivery state is visible through the API if you need to prove whether something was sent.

Two more messages exist that are not incident alerts: **monitoring stopped** and **monitoring resumed**. They fire when every probe covering a monitor has gone silent longer than expected and when results start flowing again. They mean the service is unwatched, not that it is down: no incident opens, and the webhook payload carries the distinct reasons `nodata` and `dataresumed`. A platform-wide probe outage on our side is suppressed rather than fanned out as a flood of these.

Scheduled maintenance windows do **not** silence channel alerting. They repaint the public status page and notify status-page subscribers, but a monitor that fails during a window still opens an incident and still pages its channels. To stay quiet through planned work, disable the monitor or unbind its channels for the duration.

## On-call

Where on-call schedules are enabled, paging works differently: an incident targets a person or a rotation, that resolves to whoever is on shift, and they are reached through the channels **they** opted into on the on-call page. A member who has chosen no channels resolves as on-call but cannot be paged. See [Incident management](incidents.md#paging-and-escalation).

## Deleting a channel

Deleting removes it from every monitor bound to it. The edit page lists those monitors so the blast radius is visible before you confirm. Monitors that lose their last channel keep running and keep opening incidents; they simply stop telling anyone.

A channel linked through a central Telegram bot can also be disabled from the other side. If the bot is removed from the chat or the chat sends `/stop`, the channel is disabled with a note explaining why, and re-enabling it clears the note.

Email channels have the same property through the one-click stop link (RFC 8058) that every alert mail carries. **Anyone who receives the mail can use it, and it disables that email channel for the whole org**, not just for the person who clicked; the channel shows a "recipient stopped delivery" note, and re-enabling clears it. Worth knowing before you forward alert mail around: a recipient tired of the noise can switch the channel off for everyone.

## Limits

The number of channels an org can hold is capped by its plan (`max_notification_channels`). Channel names must be unique within the org. Test sends count against the same per-minute budget as other test operations. See [Quotas and rate limits](quotas.md).

## Managing them as code

Notification channels are a Terraform resource, so a channel and the monitors bound to it can live in the same config as the rest of your infrastructure. The one-tap linked types are excluded, since their credentials belong to the platform's bot rather than to your org. See [Terraform](terraform.md).
