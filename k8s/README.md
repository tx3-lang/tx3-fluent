# Kubernetes

The Helm chart for running Tx3 Fluent over Streamable HTTP on Kubernetes,
packaged so that no cluster, hostname, identity provider, bundle or credential
is assumed. This directory is the instance-agnostic deployment unit; an
instance's values file, its Secret manifest and its runbook belong in the
operator's infrastructure repository.

| path | holds |
|---|---|
| `chart/` | the chart: one replica, `Recreate`, a ConfigMap with `fluent.toml` and the registration bundles, a PVC for the SQLite store, a ClusterIP Service and an optional ingress |
| `test.sh`, `tests/values.yaml` | render tests: `helm lint`, the rendered shape, and the values that must refuse to render; CI runs them |

## The chart

`values.yaml` is deliberately unusable as-is: the image tag, the configuration,
the Secret and at least one bundle are empty and `required` in the templates,
so a missing value fails at render rather than deploying a half-configured
server. The values that look arbitrary carry their reason in the file.
What the chart decides for every instance:

- **One replica, `Recreate`.** MCP sessions live in the process's memory and
  the store is one SQLite file on an RWO volume.
- **Configuration and bundles from one ConfigMap**, mounted read-only at
  `/etc/fluent`: `fluent.toml` from `config`, and each bundle from
  `registrations.<slug>.{registration,skill}` at
  `/etc/fluent/registrations/<slug>/{registration.toml,SKILL.md}`. Supply the
  bundle files with `--set-file` so the reviewed files are what ship. Both are
  read only at start; the Deployment carries their checksum, so changing
  either rolls the pod.
- **The chart owns the port and the layout.** It sets
  `FLUENT_SERVER__LISTEN=0.0.0.0:8080` and
  `FLUENT_REGISTRATIONS__DIR=/etc/fluent/registrations`, overriding the
  configuration file.
- **A read-only bundle directory.** The registry cache cannot be written, so a
  registry-sourced bundle's artifact is fetched and verified by digest on every
  start; the server logs a warning, not an error.
- **The store on the volume.** Point `[store] sqlite_path` into
  `persistence.mountPath` (`/data`). The claim survives `helm uninstall`.
- **Non-root, read-only root filesystem.** uid and gid 1000 with
  `fsGroup: 1000`, no capabilities, and an `emptyDir` at `/tmp`.
- **No service links.** Kubernetes would otherwise inject variables such as
  `FLUENT_PORT_8080_TCP` for a release named `fluent`.

Secrets are referenced, never templated: `existingSecret` names a Secret whose
keys become the container's environment, and they must be the variables that
the configuration's `*_env` keys name (see
[the hosted deployment guide](../docs/hosted-deployment.md#environment)).
Kubernetes does not restart a pod when a referenced Secret changes; restart
the Deployment after rotating one.

The ingress routes the whole host. `/mcp` answers only requests whose `Host`
is `public_url`'s host, so the controller must pass the original `Host`
header, and Streamable HTTP holds event streams open, so give the controller
an idle timeout above `limits.global_cutoff_secs` and no response buffering
through `service.annotations` and `ingress.annotations`.

Render and install are the operator's responsibility. `templates/NOTES.txt`
prints the smoke test.

```sh
helm upgrade --install fluent k8s/chart --namespace fluent \
  --values values.yaml \
  --set-file registrations.transfer_preprod.registration=deploy/hosted/registrations/transfer_preprod/registration.toml \
  --set-file registrations.transfer_preprod.skill=deploy/hosted/registrations/transfer_preprod/SKILL.md
```
