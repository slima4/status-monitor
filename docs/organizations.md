# Organizations

An organization owns everything: monitors, channels, status pages, variables, incidents. Nothing is shared between two of them — separate monitors, separate people, separate plan and quota, separate status pages. Switching orgs changes everything you see.

Manage them under **Settings → Organizations**. That screen lists every org you belong to with your role in each, and is where you create, rename, delete and restore them. [Team](team.md) covers the people inside one.

## Why you might want a second one

Most accounts never need one. The cases that do:

- **A hard boundary between two things you run.** A side project whose alerts should not page the same people as the main product.
- **Staging kept away from production**, so a noisy pre-release environment cannot dent the numbers on a page customers read.
- **Client work you own.** If the client owns their own account, ask them to invite you instead — an invited membership costs you nothing against your limit.

If you are running status pages for several clients out of one account, you probably want several status pages in one org rather than several orgs. Pages carry their own branding and their own subdomain, and one org keeps the billing and the roster in one place.

## Creating one

**Settings → Organizations → new organization.** You give it a display name and a URL slug; the slug is checked as you type and derived from the name until you edit it yourself.

The slug is a public host on our domain, so it is the one field worth thinking about — it becomes your status page address and the value tooling sends as `X-Uptimepage-Org`.

Creating an org makes you its owner and switches you into it. It starts empty.

You can **own** at most three organizations at once. Being invited into one never counts against that, so you can join as many as you like. Deleted orgs stop counting while they sit in the restore window.

## Switching

Your session has one active org at a time. Switch from the org menu in the header, or from the row actions on this screen. The switch applies to the session, so other devices you are signed in on stay where they are until you switch there too.

The header menu only appears once you belong to more than one org.

## Renaming

Owners can change the display name and the slug. The name is cosmetic and safe to change whenever.

Changing the **slug is a hard cutover**. The old slug is freed with no redirect kept, which means the public status page moves address and anything sending the old value as `X-Uptimepage-Org` — API clients, Terraform, CI — breaks until you update it. The confirmation says so before you commit.

## Deleting

Owners only, and never your last one: an account with no org has nothing to sign in to, so the button is not offered and the API refuses it. Create the replacement first if you are trying to start over.

Deleting takes effect at once. Checks stop running, the status pages go offline, and every URL referencing the org returns 404. Anyone whose session was pointed at it is moved to another org they belong to — and a member who had no other org is left signed in with nowhere to go until you restore it.

Nothing is erased yet. The org sits in a restore window for 30 days, then a background job purges it for good — the Postgres rows and the ClickHouse check history together.

## Restoring

Inside the window the org shows under **recently deleted** with the date it will be purged, and one click brings it back: checks resume, status pages return at the same addresses.

Two limits worth knowing. Only the person who deleted it can restore it — not any owner. And the window is firm: past it restore is refused, and the purge that follows is what actually frees the name.

Your slug stays reserved that whole time, so nobody can take it while you decide. It is released when the org is purged rather than the moment the window closes, and the purge runs once a day — so expect the name to stay locked for up to a day after restore stops being offered.

## Related

People, roles and invitations are in [Team](team.md). What each plan allows per org is in [Quotas](quotas.md). The table-level org model, tenant isolation and the org-management API are in [Multi-tenancy](multi-tenancy.md).
