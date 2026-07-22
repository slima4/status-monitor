# Team

An organization is the unit everything belongs to: monitors, channels, status pages, variables, incidents. Adding someone to your org gives them the same view of all of it.

Manage people under **Settings → Team**. Any member can open it and see who is in the org; only owners get the invite form and the row actions.

## Roles

Two roles, and the split is worth understanding before you invite anyone.

| | Member | Owner |
|---|---|---|
| See monitors, incidents, and results | yes | yes |
| Create and edit monitors | yes | yes |
| Create and edit notification channels | yes | yes |
| Manage variables and secrets | yes | yes |
| Acknowledge and resolve incidents | yes | yes |
| Create and edit status pages, branding, components | no | yes |
| Invite, remove, or change the role of a person | no | yes |
| Rename or delete the organization | no | yes |

The rough line is that a member operates the monitoring and an owner controls the org itself: who is in it, and the branded surface customers see. The status page is an owner-level asset because it carries your brand.

Publishing is not the same as owning the page. A member can narrate an incident and publish it to the status page, which is what you want when the person handling an outage at 3am is not the person who owns the account. What a member cannot do is create pages, change branding, or decide which monitors appear on them.

A contractor who should watch and fix monitors is a member. Someone who needs to add teammates or reshape the public page is an owner.

## Inviting someone

**Settings → Team → invite**, enter an email address and pick a role. They get an email with accept and decline links.

What happens on their side depends on what they already have:

- **They already have an account.** Clicking accept redeems the invitation straight away. Clicking the link is the consent, so there is no second confirmation step. The email address has to match the one you invited.
- **They are signed out.** The link sends them to sign-in and carries the invitation through whichever method they use, accepting it automatically once they are in.
- **They have no account at all.** Requesting an email sign-in link for the invited address bootstraps the account and joins your org directly, without creating a personal org for them first.

Either way they land on the dashboard with a welcome banner naming the org they joined, and their active org is switched to yours.

If an invitation cannot be redeemed — the email does not match, it was revoked, or the last seat was taken while they were signing in — the sign-in still succeeds and the dashboard explains that the invitation could not be applied. A failed sign-in would be a bad way to learn about a seat limit.

## Pending invitations

The team screen lists invitations that have not been answered yet. Each can be **resent** if the mail was lost or landed in spam, or **revoked**, which invalidates the link immediately.

Invitation tokens are single-use and stored hashed, so a resend issues a fresh link and the old one stops working.

## Removing someone and leaving

Removing a member revokes their access to the org immediately. Anything they created stays: their monitors keep running, their channels keep delivering.

An owner's own row shows **leave** instead of remove and carries no role toggle, so you cannot accidentally demote or delete yourself while managing everyone else. Leaving switches your session to another org you belong to.

Members have no self-service way out today: the row actions are owner-only, so a member who wants to leave has to ask an owner to remove them.

An org always keeps at least one owner. Removing or demoting the last one is refused rather than leaving the org unadministered. To hand an org over, promote the new owner first, then leave.

## Seats

The number of active members is capped by your plan, and pending invitations have their own separate cap so a pile of unanswered invites cannot occupy the whole roster.

At the cap, new invitations are refused with a clear error. Removing a member frees the seat immediately. If two people accept the last seat at the same moment, one wins and the other's invitation is left unconsumed rather than burned, so they can accept again once a seat frees.

Current usage against both caps is on **Settings → Usage**. See [Plans and limits](hosted/plans-and-limits.md).

## Belonging to several orgs

You can be a member of any number of organizations, but you can **own** at most three at once. Being invited into an org never counts against that limit, so consultants and contractors can join as many client orgs as they like.

Your session has one active org at a time, and everything you see belongs to it. Soft-deleted orgs stop counting against the owner limit.

## Related

The token mechanics behind invitations — expiry, hashing, how each sign-in method carries an invitation — are in [Authentication](authentication.md#invitations). The org model and tenant isolation are in [Multi-tenancy](multi-tenancy.md).
