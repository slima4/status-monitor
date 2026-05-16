# Communication Templates

> **Private operator document.** Fill the bracketed placeholders per
> incident. The data-breach template must only be used after coordinating
> with the German supervisory authority.

## A. Incident notification (mid-outage)

Subject: `[Status Update] Service degradation in progress`

> We're investigating reports of slow responses and errors on the
> status-monitor service starting at HH:MM UTC. Our team is working to
> resolve this. We'll update this thread as we learn more.
>
> Affected: [public status pages | operator UI | API | all]
> Status: investigating

## B. Scheduled maintenance announcement

Subject: `Scheduled maintenance — YYYY-MM-DD HH:MM UTC`

> We'll be performing scheduled maintenance from HH:MM UTC to HH:MM UTC
> on [date]. During this window, you may experience:
>
> - Brief interruption to the operator UI
> - 5–15 minute outage during the migration
>
> Public status pages will remain available (read-only during the database
> upgrade).
>
> No action required from you. Thanks for your patience.

## C. Data breach notification (GDPR Article 34)

**Use only if data was actually exposed.** Coordinate with the German
supervisory authority before sending.

Subject: `Important security notice about your status-monitor account`

> We are writing to inform you of a security incident affecting your
> account.
>
> **What happened:** On YYYY-MM-DD, we discovered that [unauthorised
> party / misconfigured system] gained access to [data category]
> belonging to your account.
>
> **What information was involved:** [specific data: email, hashed IPs,
> session info, etc.]
>
> **What we are doing:** We have [contained the issue / patched the
> vulnerability / notified authorities]. We have rotated all sessions
> and recommend you sign in fresh.
>
> **What you can do:** [steps for the user — change password elsewhere,
> revoke API tokens, etc.]
>
> We sincerely apologise. We're committed to protecting your data.
>
> For questions: slima4.u8@gmail.com

## D. Account suspension notice

Subject: `Your status-monitor account has been suspended`

> We have suspended your account ([email]) due to [violation: specific
> description].
>
> The relevant part of our Terms of Service: [name the clause / paste the link].
>
> If you believe this was in error, you can appeal by replying to this
> email within 14 days.
>
> If we don't hear from you within 14 days, the account will be
> permanently deleted.

## E. Service shutdown notice (90+ days notice)

Subject: `Important: status-monitor hosted service will shut down on YYYY-MM-DD`

> We are writing to let you know that the hosted status-monitor service
> will shut down on [date], 90+ days from today.
>
> Your data will remain accessible until that date. To preserve your data:
>
> 1. Export your data: visit /settings/account → "Export My Data"
> 2. Self-host: source code is at https://github.com/slima4/status-monitor —
>    deployment instructions are in the README
>
> We are sorry to discontinue the service. Thank you for using
> status-monitor.
