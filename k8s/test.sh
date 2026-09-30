#!/bin/sh
# Render tests for the Helm chart: lint, the rendered shape, and the values
# that must refuse to render. Needs helm only.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
CHART="$ROOT/k8s/chart"
VALUES="$ROOT/k8s/tests/values.yaml"
BUNDLE="$ROOT/deploy/hosted/registrations/transfer_preprod"
OUT=$(mktemp -d)
trap 'rm -rf "$OUT"' EXIT

render() {
    helm template fluent "$CHART" --namespace fluent-test --values "$VALUES" \
        --set-file "registrations.transfer_preprod.registration=$BUNDLE/registration.toml" \
        --set-file "registrations.transfer_preprod.skill=$BUNDLE/SKILL.md" "$@"
}

fail() {
    echo "$1" >&2
    exit 1
}

helm lint "$CHART" --values "$VALUES" \
    --set-file "registrations.transfer_preprod.registration=$BUNDLE/registration.toml" \
    --set-file "registrations.transfer_preprod.skill=$BUNDLE/SKILL.md"

render >"$OUT/default.yaml"
grep -F 'image: ghcr.io/tx3-lang/tx3-fluent:sha-deadbee' "$OUT/default.yaml"
grep -F 'args: ["serve", "--http", "--config", "/etc/fluent/fluent.toml"]' "$OUT/default.yaml"
grep -Fx '  replicas: 1' "$OUT/default.yaml"
grep -Fx '    type: Recreate' "$OUT/default.yaml"
grep -Fx '      enableServiceLinks: false' "$OUT/default.yaml"
grep -Fx '        runAsUser: 1000' "$OUT/default.yaml"
grep -Fx '        fsGroup: 1000' "$OUT/default.yaml"
grep -Fx '            readOnlyRootFilesystem: true' "$OUT/default.yaml"
grep -Fx '                name: fluent-test' "$OUT/default.yaml"
grep -F -A1 'name: FLUENT_SERVER__LISTEN' "$OUT/default.yaml" | grep -F '"0.0.0.0:8080"'
grep -F -A1 'name: FLUENT_REGISTRATIONS__DIR' "$OUT/default.yaml" | grep -F '/etc/fluent/registrations'
grep -Fx '                path: registrations/transfer_preprod/registration.toml' "$OUT/default.yaml"
grep -Fx '                path: registrations/transfer_preprod/SKILL.md' "$OUT/default.yaml"
grep -Fx '  transfer_preprod.registration.toml: |' "$OUT/default.yaml"
grep -F 'slug = "transfer_preprod"' "$OUT/default.yaml"
grep -F 'name: transfer-preprod' "$OUT/default.yaml"
grep -Fx '    helm.sh/resource-policy: keep' "$OUT/default.yaml"
grep -F 'checksum/config:' "$OUT/default.yaml"
if grep -Fq 'kind: Ingress' "$OUT/default.yaml"; then
    fail "the ingress rendered while disabled"
fi

render --set image.digest=sha256:0123 >"$OUT/digest.yaml"
grep -F 'image: ghcr.io/tx3-lang/tx3-fluent:sha-deadbee@sha256:0123' "$OUT/digest.yaml"

render --set ingress.enabled=true --set ingress.className=nginx \
    --set ingress.clusterIssuer=letsencrypt --set ingress.host=fluent.example.org \
    --set ingress.tlsSecretName=fluent-tls >"$OUT/ingress.yaml"
grep -Fx '  ingressClassName: nginx' "$OUT/ingress.yaml"
grep -Fx '    cert-manager.io/cluster-issuer: letsencrypt' "$OUT/ingress.yaml"
grep -Fx '    - host: fluent.example.org' "$OUT/ingress.yaml"
grep -Fx '      secretName: fluent-tls' "$OUT/ingress.yaml"

# The checksum follows the bundles: a changed skill must roll the pod.
printf 'changed\n' >"$OUT/SKILL.md"
render --set-file "registrations.transfer_preprod.skill=$OUT/SKILL.md" >"$OUT/changed.yaml"
if [ "$(grep -F 'checksum/config:' "$OUT/default.yaml")" = "$(grep -F 'checksum/config:' "$OUT/changed.yaml")" ]; then
    fail "a changed bundle did not change the config checksum"
fi

for invalid in image.tag=latest image.tag= config= existingSecret= \
    ingress.enabled=true; do
    if render --set-string "$invalid" >"$OUT/invalid.yaml" 2>&1; then
        fail "invalid configuration accepted: $invalid"
    fi
done

# --set-file applies after --set-string, so an empty bundle file is an empty
# file, not an empty string.
: >"$OUT/empty"
if render --set-file "registrations.transfer_preprod.skill=$OUT/empty" >"$OUT/invalid.yaml" 2>&1; then
    fail "a bundle without its skill rendered"
fi
if helm template fluent "$CHART" --values "$VALUES" >"$OUT/invalid.yaml" 2>&1; then
    fail "a release without bundles rendered"
fi
if render --set-string 'registrations.Bad.registration=x' \
    --set-string 'registrations.Bad.skill=x' >"$OUT/invalid.yaml" 2>&1; then
    fail "an invalid slug rendered"
fi

echo "chart tests passed"
