# GDPR Email-Channel Requests Log

> **Private operator document.** The self-service tools (`/settings/account`
> → export / delete / recover) cover the common cases. This log is the
> workflow + record for requests that arrive **by email** instead — locked-out
> users, requests on behalf of a deceased user, or anything beyond
> self-service. The Privacy Policy promises: acknowledge within 7 days,
> verify identity, fulfil within 30 days.

## Workflow per request

1. **Acknowledge** within 7 days. Reply from the contact mailbox.
2. **Verify identity.** Default check: the requester emails from the address
   on the account. For third-party / deceased-user requests, ask for
   documentation proportionate to the request.
3. **Classify** the right (Access / Erasure / Rectification / Portability /
   Restriction / Objection — GDPR Articles 15–21).
4. **Fulfil** within 30 days of receipt:
   - Access / Portability → run the data export for the account, send the
     JSON over a secure channel.
   - Erasure → trigger account deletion; confirm the 30-day purge will
     complete it; tell the requester the effective date.
   - Rectification / Restriction / Objection → action in the operator UI or
     DB as appropriate; record what changed.
5. **Record** the row in the table below. Keep it minimal — no more reporter
   PII than needed; this log is itself subject to retention.

## Log

| Received | Requester (masked) | Right | Identity verified | Action taken | Fulfilled | Notes |
|---|---|---|---|---|---|---|
| _YYYY-MM-DD_ | _j\*\*\*@example.com_ | _Erasure_ | _yes (account email)_ | _account deleted; purge T+30_ | _YYYY-MM-DD_ | |

(Append one row per request. Purge closed rows once the statutory and any
dispute window has elapsed.)
