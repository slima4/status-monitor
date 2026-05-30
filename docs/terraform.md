# Terraform

Manage your monitors and notification channels as code with the official
Terraform provider,
**[`uptimepage/uptimepage`](https://registry.terraform.io/providers/uptimepage/uptimepage)**.

The Terraform Registry page is the **full reference** — every resource,
attribute, and data source, regenerated from the provider on each release. This
page is a quick start; it links out rather than duplicating that reference.

## Quick start

```hcl
terraform {
  required_providers {
    uptimepage = {
      source = "uptimepage/uptimepage"
    }
  }
}

provider "uptimepage" {
  token = var.uptimepage_token # or set UPTIMEPAGE_TOKEN
  org   = "your-org-slug"      # or set UPTIMEPAGE_ORG
  # endpoint defaults to https://app.uptimepage.dev; set it for a self-hosted instance
}

resource "uptimepage_target" "api" {
  name     = "api prod"
  interval = 60
  check = {
    type = "http"
    http = {
      url             = "https://example.com/healthz"
      expected_status = { kind = "exact", exact = 200 }
    }
  }
}
```

## Credentials

- **Token** — create one at **Settings → API tokens** (`/settings/api-tokens`;
  requires a verified email). Supply it via the `token` attribute or the
  `UPTIMEPAGE_TOKEN` environment variable. The full token is shown **once**.
- **Org** — API tokens are user-scoped, so every request must name an
  organization. Set `org` (the org **slug**) or `UPTIMEPAGE_ORG`; it is sent as
  the `X-Uptimepage-Org` header. Without it the API returns `400 ORG_REQUIRED`.
  Find your slug from `GET /api/v1/orgs` or your dashboard URL.
- **Endpoint** — defaults to the hosted API at `https://app.uptimepage.dev`. For
  a self-hosted instance, set `endpoint` to your host (the apex marketing domain
  does not serve `/api/v1`).

## Resources & data sources

| Name | Kind | Manages |
|---|---|---|
| `uptimepage_target` | resource | Monitors — `http`, `tcp`, `tls_cert`, `domain_expiry`, `dns` checks |
| `uptimepage_notification_channel` | resource | Alert destinations — `webhook`, `slack`, `telegram` |
| `uptimepage_target` | data source | Look up an existing target by id |

For the full attribute reference and an example per check type, see the
[provider docs on the Terraform Registry](https://registry.terraform.io/providers/uptimepage/uptimepage/latest/docs).

## Source

Provider source and issue tracker:
<https://github.com/uptimepage/terraform-provider-uptimepage>.
