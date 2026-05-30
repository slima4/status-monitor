# Legal Review Checklist

Before publishing **any** legal document under `docs/legal/`, work
through this checklist:

- [ ] Replace every `status-monitor` with your finalised project name
- [ ] Replace every `slima4.u8@gmail.com` with your real contact email(s)
- [ ] Verify the data inventory matches what your deployment actually
      stores (don't claim you don't store something that you do)
- [ ] Verify the third-party processor list matches your actual
      dependencies — adding any new processor is a Privacy Policy update
- [ ] Verify retention periods in the Privacy Policy match the
      `[retention]` config values
- [ ] Verify the hosting location claim matches reality (Hetzner DC region)
- [ ] Remove any clause claiming certifications you don't have (SOC 2,
      ISO 27001, HIPAA, etc.). **Critical.** Falsely claiming compliance
      is fraud
- [ ] If you operate as an individual: confirm the Impressum is correct
      with real legal name and address
- [ ] If you operate as a company: replace placeholder operator
      information with your registered company details, VAT ID,
      register number
- [ ] Confirm governing-law clause matches your jurisdiction (Germany if
      Hetzner-hosted; or your home country if you prefer)
- [ ] Have at least one friend who is not technical read the entire
      Privacy Policy — clarity matters
- [ ] Confirm the abuse contact and security contact are monitored
      (auto-forward to a mailbox you read)
- [ ] Set up calendar reminder to re-review docs annually
- [ ] Set up calendar reminder to update the `Expires` date in
      `static/.well-known/security.txt` (currently 2027-12-31)

When you complete the checklist, sign off in a commit message:
`legal: reviewed and approved for publication on YYYY-MM-DD`.

---

## Deployment status (status-monitor-inc)

These values were filled in from the deployment configuration. Items
marked **ACTION** still need a human decision or real data before the
docs go live:

| Item | Status |
|---|---|
| Project name | brand `uptimepage`; Rust crate/binary kept `status-monitor` |
| Contact / abuse / security email | `slima4.u8@gmail.com` — **ACTION:** confirm this mailbox is actively monitored (consider role aliases `abuse@`, `security@` that forward to it) |
| Operator (legal entity) | `status-monitor-inc`, Nicosia, Cyprus — **ACTION:** add registered company number / VAT ID if the entity is incorporated |
| Hosting location | Hetzner, Nürnberg, Germany — stated in Privacy Policy §6 and Impressum |
| Governing law | Germany (Hetzner-hosted). **ACTION:** confirm — the operating entity is in Cyprus; decide whether German or Cypriot law should govern |
| Public domain | `status-monitor.example.com` placeholder in `security.txt` (Canonical/Policy) and the security policy scope. **ACTION:** replace with the real production domain before publishing |
| Retention periods | Privacy Policy table matches the documented retention windows; re-verify against `[retention]` config at deploy time |
| Certifications | None claimed (DPA / SCC references describe processor safeguards, not our own certification) — OK |
| Annual re-review reminder | **ACTION:** create the calendar reminder (docs review + `security.txt` `Expires`) |
