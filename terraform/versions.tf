# Terraform settings: HCP Terraform Cloud state backend + pinned
# providers. State + secrets live in the TFC workspace, never the repo.
#
# ONE-TIME: replace organization below with your HCP Terraform org
# name (it cannot be a variable — Terraform parses this block before
# variables exist). Everything else is variable-driven.
terraform {
  required_version = ">= 1.9.0"

  cloud {
    organization = "status-monitor"

    workspaces {
      name = "status-monitor-grafana"
    }
  }

  required_providers {
    grafana = {
      source  = "grafana/grafana"
      version = "~> 3.0"
    }
  }
}
