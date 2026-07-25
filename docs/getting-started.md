# Getting started

From nothing to a monitored endpoint, an alert that reaches you, and a public status page. About ten minutes.

This walks through the hosted service at <https://app.uptimepage.dev>. Running your own instance instead? Start at [Deployment](deployment.md), then come back here — everything below works the same once you are signed in.

## 1. Sign in

Sign in with GitHub or Google. There is no password to choose and no email opt-in.

The email sign-in link on the login page is for accounts that already exist or have been invited, so a brand new account starts with GitHub or Google.

Signing in for the first time creates your organization, makes you its owner, and gives you a slug based on your name. Everything you create from here belongs to that org, and teammates you invite later share it.

## 2. Add a monitor

**Monitors → add monitor.** Paste a URL, leave the defaults, save. That is genuinely all that is required; the rest of the form has sensible defaults you can revisit.

Two things are worth setting deliberately even on the first one:

- **Interval.** How often to check. For something customer-facing, pick the fastest interval your plan allows; the form shows your plan's floor.
- **Regions.** Where to check from. Picking more than one means a single network path having a bad minute does not read as an outage, because the default policy needs most regions to agree. See [Probe regions](hosted/regions.md).

Point it at a health endpoint rather than the homepage if you have one. A homepage can render perfectly while the database behind it is gone.

An HTTP check is the right default, but it is not the only kind. Cron jobs, certificates, DNS records, and login flows each have their own, see [Monitor types](monitor-types.md).

The monitor starts checking immediately. Give it a couple of intervals and its detail page fills in with response times and a status history.

## 3. Get alerted

A monitor with no channel opens incidents that nobody hears about. This is the step people skip and regret.

**Settings → Notifications → add channel.** Slack and Discord are one click if your operator has connected them; otherwise paste an incoming webhook URL. Email, SMS, PagerDuty, Telegram, ntfy, Pushover, and plain webhooks are all there too.

Hit **test now** before saving. It sends a real synthetic alert through the real transport, so a wrong URL or an expired token surfaces now rather than during your first outage.

Then bind it: open your monitor, find the **Notifications** section, tick the channel. That section only appears once the org has at least one channel, which is why the channel comes first.

While you are there, look at **consecutive failures**. The default of two means a single blip will not page you, and two consecutive failures will. See [Notifications](notifications.md).

## 4. Publish a status page

**Settings → Status pages → create.** The page is live from the moment it is created, at a temporary address like `page-k3f9x2m1qp.uptimepage.dev`, and it drops you straight into the editor.

Set the real slug first, since renaming is a hard cutover: the old address stops working immediately. Then add the monitors customers should see, give each a public name, and set your logo and colour.

Two things to keep in mind:

- Only monitors you explicitly add appear. A new page is live but empty, so nothing is published by accident, and a monitor's URL, headers, and credentials never reach the public page. Take a page offline any time by disabling it.
- The public name is separate from the internal one. `prod-api-lb-01` can appear as "API".

When a published monitor goes down, an incident opens on the page automatically. You can narrate it as you learn more, and customers can subscribe for updates. See [Public status page](public-status.md) and [Per-org status pages](per-org-status.md).

## 5. Where to go next

| If you want to | Read |
|---|---|
| Manage all of this as code | [Terraform](terraform.md) |
| Drive it from a script or CI | [REST API](api.md) |
| Let an AI assistant query your monitors | [MCP server](mcp.md) |
| Share one monitor without an account | [Share links](share-links.md) |
| Keep credentials out of monitor configs | [Variables and secrets](variables.md) |
| Run a real incident response process | [Incident management](incidents.md) |
| Know what your plan allows | [Plans and limits](hosted/plans-and-limits.md) |

## If something looks wrong

A monitor stuck on "no data" usually means it has not completed its first check yet; give it one interval.

A monitor that is down while the site loads fine in your browser is usually the check being stricter than you expected: an expected status code that does not match, a body assertion that no longer holds, or TLS verification against a self-signed certificate. The monitor detail page shows the actual response that failed.

Alerts that never arrive are almost always an unbound channel or, for email, an address that was never verified. The channel list shows verification state.
