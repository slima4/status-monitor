# Publishing the Helm charts

The charts live in `charts/uptimepage` and `charts/uptimepage-agent` and are distributed as OCI artifacts from `ghcr.io/uptimepage/charts`. Nothing publishes on a push. Publishing is always a deliberate `workflow_dispatch`.

## Cutting a release

Bump `version:` in the chart's `Chart.yaml`. CI fails a chart whose files changed without a bump, because a re-push would otherwise serve different templates under a version people already pulled.

Update every reference to that version. The `cosign verify` examples in `docs/kubernetes.md` and the chart READMEs pin an exact tag, and CI fails when they name anything other than the chart's current version. The same check covers the `artifacthub.io/images` annotation, which has to name the current `appVersion`.

Push and wait for the `charts` workflow to go green. It lints, renders, and validates both charts against the oldest and newest Kubernetes releases they claim to support, then checks that the render-time guards still refuse a bad install.

Run the workflow again from the Actions tab, this time with `publish` checked:

```bash
gh workflow run charts.yml -f publish=true
```

That packages each chart, pushes it, signs the digest with cosign keyless signing, and verifies the signature with the same flags the docs hand to users. A broken signing identity fails there rather than for the first person who checks.

Confirm the result the way a stranger would, with no credentials in play:

```bash
helm pull oci://ghcr.io/uptimepage/charts/uptimepage --version <version>
cosign verify ghcr.io/uptimepage/charts/uptimepage:<version> \
  --certificate-identity-regexp '^https://github.com/uptimepage/uptimepage/' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

Helm tarballs embed a build timestamp, so re-running a publish for an unchanged chart produces a new digest and leaves the previous one untagged in the registry. Delete the orphan rather than leaving an unsigned artifact in a public package:

```bash
gh api '/orgs/uptimepage/packages/container/charts%2Fuptimepage/versions' \
  --jq '.[] | select(.metadata.container.tags == []) | .id'
```

## Listing on Artifact Hub

One time, and only from a maintainer account. Each chart is a separate repository on Artifact Hub because each is its own OCI repository.

Add the repository at https://artifacthub.io/control-panel/repositories with kind `Helm charts (OCI)` and URL `oci://ghcr.io/uptimepage/charts/uptimepage`, then repeat for `uptimepage-agent`. Artifact Hub reads the `artifacthub.io/*` annotations out of `Chart.yaml`, so the listing content is already in the repo and needs no separate file.

Claiming ownership is what earns the verified publisher badge. Copy the repository ID that Artifact Hub shows, write it to a scratch file, and push that file to the same OCI repository under the `artifacthub.io` tag:

```yaml
# artifacthub-repo.yml
repositoryID: <id from the Artifact Hub control panel>
owners:
  - name: uptimepage
    email: hello@uptimepage.dev
```

```bash
oras push ghcr.io/uptimepage/charts/uptimepage:artifacthub.io \
  --config /dev/null:application/vnd.cncf.artifacthub.config.v1+yaml \
  artifacthub-repo.yml:application/vnd.cncf.artifacthub.repository-metadata.layer.v1.yaml
```

Do not commit that file into the chart directory. It is registry metadata, not part of the chart, and `helm package` would ship it to every user.

Both charts carry `artifacthub.io/prerelease: "true"`. Drop it once the charts have been installed by someone outside this repo.
