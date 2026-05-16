# Security Policy

**Last updated:** 2026-05-15

## Reporting Vulnerabilities

Found a security issue in status-monitor? Email us at
**slima4.u8@gmail.com** with subject `[SECURITY]`.

Please **do not** publicly disclose vulnerabilities until we have
acknowledged and addressed them.

## What to Include

- A description of the issue
- Steps to reproduce
- Any proof-of-concept code
- The impact you believe the issue has
- Whether you've disclosed it elsewhere

## Our Response

- **Acknowledgement: within 48 hours**
- **Initial assessment: within 7 days**
- **Fix or mitigation: timeline depends on severity**

We use the following severity definitions:

- **Critical** (RCE, data breach affecting many users): fix within 7 days
- **High** (auth bypass, sensitive data exposure): fix within 30 days
- **Medium** (limited information leak, DoS): fix within 90 days
- **Low** (theoretical risk, minor info disclosure): next release

## Disclosure Coordination

We follow coordinated disclosure. We'll work with you on a public
disclosure timeline, normally:

- 90 days after report for high/critical
- 30 days after fix release for medium/low

You may request earlier disclosure if you have a good reason.

## Acknowledgements

We maintain a public Hall of Fame for reporters in
`SECURITY.md` on our GitHub repository. By default, we credit you with
the name and (optional) link you provide. Anonymous reports are
welcome too.

## What's In Scope

- The source code at https://github.com/slima4/status-monitor
- The hosted Service (`*.status.example.com`, `app.example.com`)

## What's Out of Scope

- Issues in third-party services (Hetzner, Resend, GitHub) — report to them
- Social engineering of users
- Physical attacks against infrastructure
- Self-hosted instances we don't operate
- Findings that require physical access to our servers
- DoS attacks against our infrastructure (please don't)

## Safe Harbor

Good-faith security research consistent with this policy will not result
in legal action. We will not pursue civil or criminal action for security
research that:

- Stays within scope above
- Avoids privacy violations and destruction of data
- Reports issues to us first
- Avoids social engineering of our staff

## Contact

slima4.u8@gmail.com
