# Monitor types

Eight kinds of check, each answering a different question. Picking the right one matters more than tuning it afterwards: a monitor that watches the wrong layer either misses the outage or pages you for something that was never broken.

The exact payload for each is in [REST API](api.md#check-specs). This page is about which to reach for.

## Choosing

| You want to know | Use |
|---|---|
| Is my site or API answering correctly | HTTP |
| Is this port open at all | TCP |
| Is this host reachable on the network | Ping |
| Did my cron job or worker run | Heartbeat |
| Is my certificate about to expire | TLS certificate |
| Is my domain about to expire | Domain expiry |
| Does this hostname still resolve where it should | DNS |
| Can a real user still log in | Flow |

Most orgs run mostly HTTP monitors, one TLS and one domain-expiry per property, a heartbeat per scheduled job, and a flow for the login path that would cost them the most.

## HTTP

The default. Requests a URL and decides up or down from the response.

Beyond status codes you can require a substring in the body, send custom headers, pick the method, post a body, and control redirect following. Expected status can be an exact code, a range, or a set, so an endpoint that legitimately answers 204 or 301 does not need a workaround.

Two behaviours worth knowing, because they prevent false pages:

- A `429` or `503` is recorded as **degraded**, not down. The upstream is answering and asking you to back off, which is not an outage. If you genuinely expect those codes, list them in the expected status and they count as up.
- Checks against the same host and port are throttled per org, so a burst of monitors against one upstream cannot look like a probe. An over-cap tick is dropped rather than recorded, so it never counts as a failure and never alerts.

Point it at a real health endpoint rather than the homepage where you can. A homepage can render fine while the database behind it is gone.

Credentials do not belong in this form. Reference an org secret from a header instead, see [Variables and secrets](variables.md).

## TCP

Opens a connection to a host and port and reports whether it was accepted. No protocol knowledge, no payload.

Right for databases, message brokers, SMTP, SSH, and anything else that speaks a protocol you cannot easily assert on. Wrong as a substitute for HTTP: a listening port proves the process is alive, not that it is serving correct responses.

## Ping

One ICMP echo, timed. Answers reachability and round-trip latency only.

Useful for routers, gateways, and hosts that expose nothing else. Silence for the whole timeout is down, since ICMP has no way to refuse. Plenty of networks and cloud providers drop ICMP entirely, so confirm the target answers a ping at all before relying on it.

Self-hosting note: the probe needs permission to open an ICMP socket. See [Multi-region probes](multi-region.md).

## Heartbeat

The direction is reversed. Instead of us probing you, your system calls a URL we give you, and we alert when the call stops arriving.

This is how you monitor things that have no endpoint to poll: nightly backups, cron jobs, queue workers, data pipelines. Creating one mints a ping URL; call it at the end of each successful run, typically by appending `curl -fsS $URL` to the job.

You set a **period** (how often you expect the ping) and a **grace** (how late it may be before it counts as failed). A job that runs hourly with a ten-minute grace flips to failing ten minutes after the missed run, and the alert follows once the monitor's confirmation count is met (default 2 consecutive failing evaluations). A fresh or re-enabled monitor gets a full period plus grace before it can go down, so creating one at an awkward moment does not page you immediately.

Set the period to how often the job *actually* runs, not how often you would like it to. A period shorter than the real cadence means every run is late by the time the next one arrives, and the monitor flaps all night for a job that is fine.

### Telling us more than "alive"

The bare URL only says the job got to the end. A path segment after it says what the ping means:

```bash
curl -fsS $URL/start     # before the work
./nightly-backup.sh
curl -fsS $URL/$?        # 0 succeeds, anything else fails with that exit code
```

`$URL/fail` does the same as a nonzero exit when you have no status to pass. Either way the monitor goes down immediately rather than waiting out the period, which is the difference between finding out at 03:05 and finding out at 04:15.

Pairing `/start` with a finish also times the run. Setting **max run time** then catches the case a plain heartbeat cannot see: a job that started, hung, and will never ping again. Without it you wait out the whole period before anything is said, which for a daily job is a day.

A POST body is kept as that run's output, so `curl -fsS --data-binary @backup.log $URL/$?` puts the tail of the log next to the failure. The monitor page then shows the exit code and that output on the last failure, which is usually the difference between reading the cause and going to find the machine that ran it. Whatever the job prints is what we store, so do not print secrets to it.

Pick grace with your deployment in mind. A ping sent while the control plane is unreachable is lost, so on a single-node self-host keep grace comfortably above your restart window.

Heartbeats never run on regional probes, and test and check-now do not apply to them: there is nothing on our side to probe.

## TLS certificate

Connects, reads the certificate, and reports how many days remain.

Two thresholds: a **warn** count that marks the monitor degraded, and a **critical** count that marks it down. The form starts at 30 and 7 days. Keep the warn threshold above your renewal automation's window, so a failed renewal surfaces while there is still time to fix it by hand.

Certificates move slowly, so the minimum interval is one hour and the form suggests twice a day. Checking more often tells you nothing new: a certificate changes on renewal, and a wrong one served mid-cycle is already caught by any HTTPS monitor on the same host.

## Domain expiry

Asks the registry how long is left on the domain registration itself, through RDAP.

Different failure from TLS and often worse: an expired certificate breaks HTTPS, an expired domain hands your name back to the market. Same warn and critical day thresholds. Worth one per domain you own, set to warn generously, since registrar transfers and renewal disputes take weeks rather than minutes.

The floor here is twelve hours rather than one, and the form suggests daily. A registration changes about once a year, and RDAP rate-limits by source address, so polling faster risks the answers being refused rather than arriving sooner. That matters most when a script or a Terraform loop creates one monitor per domain.

## DNS

Resolves a name and checks the answer.

Pick a record type (`A`, `AAAA`, `CNAME`, `MX`, `NS`, `TXT`, `SOA`, `PTR`, `CAA`, `SRV`), optionally a specific resolver to query, and optionally a substring the answer must contain.

That last option is the point of the check. Without it, any answer at all counts as up, so you learn only that the name still resolves. With it, you learn that it resolves **to the right place** — which is what catches a hijacked record, a bad registrar change, or a regional misroute that a plain HTTP check from one location would sail straight past.

An empty answer, including NXDOMAIN, is down. So is a mismatch when you have set an expected substring, and so is a resolver that fails outright. Querying a specific resolver is how you verify propagation: point one monitor at your authoritative server and another at a public resolver, and a gap between them is a propagation problem.

The default resolver caches and honours TTL, so checking faster than the record's TTL mostly re-reads the cache. Five minutes suits most records. Naming a resolver explicitly queries it directly, which is where a tighter interval starts to earn its keep.

## Flow

Drives a real headless browser through a scripted sequence, so it verifies that a user can still log in rather than that the login endpoint returns 200.

Steps run in order: navigate, fill a field, click, wait for a selector, assert text, assert URL. At least one assertion is required, so a broken login fails instead of quietly passing. Up to 30 steps, with a whole-run budget and a timeout for the steps that wait on something.

This is the check that catches what nothing else does: an expired OAuth secret, a broken JavaScript bundle, a session cookie that stopped being set. It is also the heaviest, so the minimum interval is five minutes and the number of flow monitors is capped by plan. On the hosted service every plan carries at least one; the counts are on [Plans and limits](hosted/plans-and-limits.md). A self-hosted install starts at zero, which is both the cap and the kill switch: set `flow.enabled` on the process that will run them and raise `max_flow_checks` on the plan the org sits on. See [Configuration](configuration.md#browser-flow-monitors).

Use a dedicated low-privilege account, never a real or admin login. Put the password in an org secret and reference it as `{{key}}` in the fill step, so the stored config holds the reference rather than the credential. Typing `{{` in a fill value lists your variables to pick from. Only the fill value resolves variables, which is why secrets are offered there and nowhere else on a step. See [Variables and secrets](variables.md).

Flows run only where a browser engine is available, so their regions are narrowed to that set rather than every region you have. Quorum needs two such regions to decide anything, so with one the monitor reports what that single region saw.

### What a flow cannot do

The step list is deliberately short, and every step finds its element with `document.querySelector` on the top-level document. That draws a hard line around what a flow can watch:

- **No iframes and no shadow DOM.** `querySelector` does not cross either boundary. A login widget embedded in an iframe, or a component built on a shadow root, is invisible to every step. The import flags recorded steps that came from an iframe rather than letting them fail later.
- **No key presses.** There is no step for pressing Enter. If your form submits that way, click the submit control instead. A recording that used Enter is flagged on import for exactly this reason.
- **No file uploads, no drag, no hover, no scrolling.** A recording that includes them drops those steps.
- **One page at a time.** A link that opens a new tab leads somewhere the flow cannot follow.
- **No screenshot, and no capture of failed network requests.** The engine has no graphical renderer, and its network events are not in a shape the client can read. See [When a step fails](#when-a-step-fails) for what you get instead.

A `fill` sets the field's value and then fires `input` and `change` events, which is what an ordinary form listens for. A framework that tracks its own value setter can ignore a value assigned this way, in which case the fill reports success and the form behaves as though nothing was typed. The assertion at the end is what catches it, which is the other reason a flow without one is refused.

Everything above is a limit of the browser engine, not of the check. When a journey needs something on this list, watch the part you can reach and put a plain HTTP monitor on the rest.

### Importing a recording

Writing selectors by hand is the slow part, so the form imports a recording instead. In Chrome, open DevTools, go to the Recorder panel, record yourself logging in, and export the recording as JSON. In the flow form, press **import a recording** and hand it the file. The import runs in your browser and the file is never uploaded, which matters because a recording contains whatever you typed, password included.

The mapping is direct:

| Recorded | Becomes |
|---|---|
| the first `navigate` | the start URL |
| a later `navigate` | `goto` |
| `change` | `fill` |
| `click`, `doubleClick` | `click` |
| `waitForElement` | `wait_for` |
| a navigation assertion on a step | `assert_url` on the path |
| `setViewport`, `scroll`, `hover`, key presses | dropped |

A recorder captures the deepest element under the cursor, which on an icon button is the icon rather than the button. Clicking that icon directly would fire an event but submit nothing, so a click step always acts on the nearest enclosing button, link, `summary` or `role="button"` when there is one, which is what a real mouse click would have activated. Naming the button yourself lands on the same element. The one case to know about: if you need to click something interactive that sits *inside* a link or button, the click goes to the outer one instead.

Two things get rewritten on the way in. A click on a field immediately followed by typing into it becomes one fill, because replaying the focus click spends a step to do nothing. A value recorded from anything that looks like a password or token is dropped rather than copied, and the row is flagged for you to point at a secret instead.

Then read what landed before you save. The import reports every step it could not carry: a selector Chrome only recorded as XPath or link text, a step inside an iframe, an Enter keypress that submitted the form and now needs an explicit click on the submit control. It also tells you when the recording produced no assertion at all, which the API would reject anyway.

### A worked example

This one runs against [the-internet.herokuapp.com](https://the-internet.herokuapp.com/login), a public practice site that prints its own credentials on the login page, so you can build it yourself and watch it pass. The password goes in an org secret named `login_password`:

```json
{
  "type": "flow",
  "start_url": "https://the-internet.herokuapp.com/login",
  "steps": [
    {"op": "fill",   "selector": "#username", "value": "tomsmith"},
    {"op": "fill",   "selector": "#password", "value": "{{login_password}}"},
    {"op": "click",  "selector": "#login > button"},
    {"op": "assert_url",  "contains": "/secure"},
    {"op": "assert_text", "contains": "secure area"}
  ],
  "timeout": 30000,
  "step_timeout": 10000,
  "verify_tls": true
}
```

Read it as a sequence of claims. The two fills prove the form fields still exist under those selectors, which catches a redesign that renamed them. The click proves the control is still there and still submits. `assert_url` proves the app moved you to the authenticated area, which is what fails when credentials are rejected. `assert_text` proves the page behind the login actually rendered, which catches the case where the URL changes but the app then errors.

The last two are what make the check meaningful. Without an assertion the flow reports up as long as the steps do not error, so a login that silently rejects you would pass. The API rejects a flow that has no assertion for that reason.

The click selector is worth a second look. That page's submit control is `<button class="radius" type="submit"><i class="fa fa-2x fa-sign-in"> Login</i></button>`, so a recorder writes down the `<i>`, not the button. The step targets the button because that is what a real click activates, which is the same rewrite the import performs.

`timeout` caps the whole run at 30 seconds. `step_timeout` caps how long a step will wait for what it is looking for, at 10 seconds. It applies to the steps that wait: `wait_for`, `assert_text` and `assert_url` poll until it runs out, and `goto` gives up on a page that will not load in it. `fill` and `click` do not wait at all, so if a field is rendered late, raising the step timeout will not help. Put a `wait_for` in front of the fill instead. A slow app needs that, not a higher interval.

The two interact. A waiting step never waits past the whole-run budget, so on a long journey the last steps get whatever is left of it rather than a full `step_timeout` each. When the budget is what ran out, the run says so and names the step it was on. When it ran out getting the browser and the first page up, it says that instead, because no step ran.

### When a step fails

A failing step reports which step and why, and a test run adds what the page looked like at that moment: the URL the browser had ended up on, the page title, the visible text, and anything the page logged to the browser console. Any secret the flow typed is scrubbed out of all of it before you are shown it.

The URL is usually the whole answer. Still sitting on `/login` after a submit means the credentials never took; landing somewhere unexpected means a redirect changed. Point the flow above at the wrong password and it comes back like this:

```
DOWN  step 4/5  assert_url: url does not contain "/secure"

URL         https://the-internet.herokuapp.com/login
Title       The Internet
Page text   Your password is invalid!
Console     (nothing logged)
```

Every declared step is recorded, not just the failing one: what it was, whether it passed, and how long it took. Steps after the failure are marked as never reached. That is what tells you a journey is drifting before it breaks, when a wait that used to take 200 ms starts taking four seconds.

The step names the fault, the URL says the submit never took, and the page says why in its own words. The console is where a broken bundle or an expired client-side token announces itself, but plenty of apps log nothing at all, and this one does not need to. The listener also attaches a moment after the first page starts loading, so a message logged during that first load can be missed. Anything logged from a later step is captured.

There is no screenshot. The browser engine has no graphical renderer, so no picture exists to take, and the text above is what stands in for one. Failed network requests are not captured either: this engine's network events are not in a shape the client can read.

### Watching a journey over time

Every run is kept, not just the failing ones, so the monitor page can answer whether a failure is new. The run list shows each run's steps with their outcome and duration, newest first, and a failed run opens to the page it captured. Runs sit under the range you have selected, and a failure stays in the list however many passes were written after it.

Below it, one small chart per step: how long that step took when it passed, over time. Steps the run never reached are left out, so a journey that stops early does not drag the steps behind it down to zero. Failures are left out of the line too, and counted next to it instead: a step that fails waits out its whole step timeout first, so a handful of failures would bury the timings of every run around them and hide the very drift the chart is for. A step that was reached but never passed says so rather than showing an empty chart.

Each step is drawn to its own scale, which is the point — a click that has crept from 11 ms to 44 ms has quadrupled, and on one shared axis beside a step taking a full second it would be a flat line on the floor. Every chart shows the current figure and how far it has moved across the range; anything up by half again is called out.

This is the picture worth watching. A login that still passes but whose redirect wait has climbed from 200 ms to four seconds is a step away from failing, and its chart says so weeks before the first red run does.

Durations are kept longer than the captured pages are — see [Quotas and limits](quotas.md).

## Intervals

Every kind has a floor, and your plan sets its own on top. The effective minimum is whichever is higher. The floor is what the API accepts; the suggestion is what the form opens at, and what most people should run.

| Kind | Floor | Suggested |
|---|---|---|
| HTTP, TCP, Ping | 10 seconds | 60 seconds |
| DNS | 10 seconds | 5 minutes |
| Heartbeat | 60 seconds (evaluation cadence) | n/a, you set period and grace |
| Flow | 5 minutes | 15 minutes |
| TLS certificate | 1 hour | 12 hours |
| Domain expiry | 12 hours | 24 hours |

The API enforces only the floor, so Terraform and the REST API can go faster than the suggestion. They rarely should.

Faster is not better. Interval decides how quickly you detect an outage, but the consecutive-failure setting decides how quickly you are told about one, and that is usually where the real tuning is. See [Notifications](notifications.md).
