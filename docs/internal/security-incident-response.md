# Security Incident Response

> **Private operator document.** This is the playbook for a *security*
> incident (compromise, data exposure) — distinct from the availability
> runbook. When in doubt about whether something is a security incident,
> treat it as one.

When you become aware of a possible security incident:

## Step 1 — Contain (immediate, ~minutes)

Identify the threat vector and stop further damage:

- Revoke compromised credentials (API tokens, OAuth secrets, DB passwords)
- Block the attacker IP at Caddy
- If there is an active database compromise: take ClickHouse / Postgres
  offline if you can survive the downtime
- Snapshot affected systems for forensics **before** restarting them

## Step 2 — Investigate (~hours)

- Pull the relevant logs (Caddy access, app errors, login attempts)
- Identify what was accessed and by whom
- Document findings in a private incident note

## Step 3 — Notify (within 72 hours, GDPR Article 33)

If personal data was breached:

- Notify the German supervisory authority (Bundesbeauftragte für den
  Datenschutz, https://www.bfdi.bund.de/) — they have a web form
- Notify affected users by email if there is a "high risk to their rights
  and freedoms" (GDPR Article 34) — typically: passwords, financial data
  or identifiers leaked
- Use the data-breach template in `communication-templates.md` for the
  user notification
- If you are unsure whether to notify: notify. Over-notifying is a
  reputation risk; under-notifying is a legal one.

## Step 4 — Remediate (~days)

- Deploy patches and hardening
- Force credential rotation for affected users
- Update detection (add an alert for the pattern you missed)

## Step 5 — Post-mortem (~a week after)

Document privately:

- Timeline of events
- Root cause
- What worked / what didn't
- What you changed

Save the post-mortem privately — useful for future incidents, and it may
help if the incident leads to litigation.
