#!/usr/bin/env bash
set -euo pipefail

readonly ARG_JOB="${1:-}"
readonly ARG_STEP="${2:-}"

readonly GIT_REPOSITORY_URL="https://github.com/openhundun/kubernetes-kit"
readonly GIT_REPOSITORY_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

function help() {
    echo "USAGE: ${0} <job__check [step__xxx | all] | job__image [step__xxx | all] | job__chart [step__xxx | all] | job__e2e [step__xxx | all]>"
}

function job__check() {
    function step__fact() {
        local app_name="$(yq -p toml -o yaml '.package.name' source/flector/Cargo.toml)"
        local app_version="$(yq -p toml -o toml '.package.version' source/flector/Cargo.toml)"
        local image_repository="$(yq -p yaml -o yaml '.image.repository' deploy/flector-chart/values.yaml)"
        local image_tag="$(yq -p yaml -o yaml '.image.tag' deploy/flector-chart/values.yaml)"
        local chart_version="$(yq -p yaml -o yaml '.version' deploy/flector-chart/Chart.yaml)"
        if [[ "${app_name}" != "$(basename "${image_repository}")" || "${app_version}" != "${image_tag}" || "${app_version}" != "${chart_version}" ]]; then
            echo "[check][fact]: FAIL" >&2
            return 1
        fi
    }

    function step__fmt() {
        cargo fmt --check
    }

    function step__clippy() {
        cargo clippy --workspace --all-targets -- -D warnings
    }

    function step__crd_diff() {
        local tmp_dir="${GIT_REPOSITORY_DIR}/out/check"
        rm -rf "${tmp_dir}"
        mkdir -p "${tmp_dir}"
        cargo run -q -p flector -- crd | yq -P '.metadata.annotations."helm.sh/resource-policy" = "keep"' > "${tmp_dir}/crd.yaml"
        diff -u deploy/flector-chart/templates/crds.yaml "${tmp_dir}/crd.yaml"
        rm -rf "${tmp_dir}"
    }

    function step__helm_lint() {
        helm lint deploy/flector-chart
        helm template flector deploy/flector-chart > /dev/null
    }

    function all() {
        step__fact
        step__fmt
        step__clippy
        step__crd_diff
        step__helm_lint
    }

    case "${ARG_STEP}" in
    step__fact) step__fact ;;
    step__fmt) step__fmt ;;
    step__clippy) step__clippy ;;
    step__crd_diff) step__crd_diff ;;
    step__helm_lint) step__helm_lint ;;
    *) all ;;
    esac
}

function job__image() {
    readonly IMAGE_REGISTRY="${IMAGE_REGISTRY:-ghcr.io}"
    readonly IMAGE_REPOSITORY_OWNER="${IMAGE_REPOSITORY_OWNER:-openhundun}"

    function step__build() {
        local image_repository="$(yq -p yaml -o yaml '.image.repository' deploy/flector-chart/values.yaml)"
        local image_tag="$(yq -p yaml -o yaml '.image.tag' deploy/flector-chart/values.yaml)"
        docker buildx create --name openhundun --driver docker-container --use > /dev/null 2>&1 || docker buildx use openhundun
        docker buildx build --file deploy/flector-image/Dockerfile --tag "${IMAGE_REGISTRY}/${IMAGE_REPOSITORY_OWNER}/$(basename "${image_repository}"):${image_tag}" --build-arg IMAGE_REGISTRY="${IMAGE_REGISTRY}" --platform linux/amd64,linux/arm64 --provenance false --sbom false --annotation "index:org.opencontainers.image.source=${GIT_REPOSITORY_URL}" --push "${GIT_REPOSITORY_DIR}"
    }

    function all() {
        step__build
    }

    case "${ARG_STEP}" in
    step__build) step__build ;;
    *) all ;;
    esac
}

function job__chart() {
    readonly CHART_REGISTRY="${CHART_REGISTRY:-ghcr.io}"
    readonly CHART_REPOSITORY_OWNER="${CHART_REPOSITORY_OWNER:-openhundun}"
    readonly TMP_DIR="${GIT_REPOSITORY_DIR}/out/chart"

    function step__package() {
        rm -rf "${TMP_DIR}"
        mkdir -p "${TMP_DIR}"
        helm package deploy/flector-chart --destination "${TMP_DIR}"
    }

    function step__push() {
        local chart_name="$(yq -p yaml -o yaml '.name' deploy/flector-chart/Chart.yaml)"
        local chart_version="$(yq -p yaml -o yaml '.version' deploy/flector-chart/Chart.yaml)"
        helm push "${TMP_DIR}/${chart_name}-${chart_version}.tgz" "oci://${CHART_REGISTRY}/${CHART_REPOSITORY_OWNER}/charts"
        rm -rf "${TMP_DIR}"
    }

    function all() {
        step__package
        step__push
    }

    case "${ARG_STEP}" in
    step__package) step__package ;;
    step__push) step__push ;;
    *) all ;;
    esac
}

function job__e2e() {
    function step__install() {
        helm upgrade --install flector deploy/flector-chart --namespace flector-system --create-namespace
        kubectl -n flector-system rollout status deploy/flector --timeout=120s
    }

    function step__test__configmap_created() {
        kubectl delete namespace flector-configmap-created-source flector-configmap-created-destination --ignore-not-found > /dev/null
        kubectl create namespace flector-configmap-created-source > /dev/null
        kubectl create namespace flector-configmap-created-destination > /dev/null
        kubectl -n flector-configmap-created-source create configmap configmap-created --from-literal=greeting=hello > /dev/null
        kubectl -n flector-configmap-created-destination apply -f - > /dev/null << EOF
apiVersion: flector.io/v1alpha1
kind: Flect
metadata: { name: configmap-created }
spec:
  source: { kind: ConfigMap, namespace: flector-configmap-created-source, name: configmap-created }
  destinations:
    - { namespace: flector-configmap-created-destination }
EOF
        local found=""
        for _ in {1..100}; do
            if [[ "$(kubectl -n flector-configmap-created-destination get configmap/configmap-created -o jsonpath='{.data.greeting}' 2> /dev/null)" == "hello" ]]; then
                found="1"
                break
            fi
            sleep 1
        done
        if [[ -z "${found}" ]]; then
            echo "[e2e][configmap_created]: FAIL" >&2
            return 1
        fi
        kubectl delete namespace flector-configmap-created-source flector-configmap-created-destination --ignore-not-found --wait=false > /dev/null
    }

    function step__test__configmap_updated() {
        kubectl delete namespace flector-configmap-updated-source flector-configmap-updated-destination --ignore-not-found > /dev/null
        kubectl create namespace flector-configmap-updated-source > /dev/null
        kubectl create namespace flector-configmap-updated-destination > /dev/null
        kubectl -n flector-configmap-updated-source create configmap configmap-updated --from-literal=greeting=hello > /dev/null
        kubectl -n flector-configmap-updated-destination apply -f - > /dev/null << EOF
apiVersion: flector.io/v1alpha1
kind: Flect
metadata: { name: configmap-updated }
spec:
  source: { kind: ConfigMap, namespace: flector-configmap-updated-source, name: configmap-updated }
  destinations:
    - { namespace: flector-configmap-updated-destination }
EOF
        local found=""
        for _ in {1..100}; do
            if [[ "$(kubectl -n flector-configmap-updated-destination get configmap/configmap-updated -o jsonpath='{.data.greeting}' 2> /dev/null)" == "hello" ]]; then
                found="1"
                break
            fi
            sleep 1
        done
        if [[ -z "${found}" ]]; then
            echo "[e2e][configmap_updated]: FAIL" >&2
            return 1
        fi
        kubectl -n flector-configmap-updated-source patch configmap configmap-updated --type merge -p '{"data":{"greeting":"bonjour"}}' > /dev/null
        local found=""
        for _ in {1..100}; do
            if [[ "$(kubectl -n flector-configmap-updated-destination get configmap/configmap-updated -o jsonpath='{.data.greeting}' 2> /dev/null)" == "bonjour" ]]; then
                found="1"
                break
            fi
            sleep 1
        done
        if [[ -z "${found}" ]]; then
            echo "[e2e][configmap_updated]: FAIL" >&2
            return 1
        fi
        kubectl delete namespace flector-configmap-updated-source flector-configmap-updated-destination --ignore-not-found --wait=false > /dev/null
    }

    function step__test__configmap_deleted() {
        kubectl delete namespace flector-configmap-deleted-source flector-configmap-deleted-destination --ignore-not-found > /dev/null
        kubectl create namespace flector-configmap-deleted-source > /dev/null
        kubectl create namespace flector-configmap-deleted-destination > /dev/null
        kubectl -n flector-configmap-deleted-source create configmap configmap-deleted --from-literal=greeting=hello > /dev/null
        kubectl -n flector-configmap-deleted-destination apply -f - > /dev/null << EOF
apiVersion: flector.io/v1alpha1
kind: Flect
metadata: { name: configmap-deleted }
spec:
  source: { kind: ConfigMap, namespace: flector-configmap-deleted-source, name: configmap-deleted }
  destinations:
    - { namespace: flector-configmap-deleted-destination }
EOF
        local found=""
        for _ in {1..100}; do
            if [[ "$(kubectl -n flector-configmap-deleted-destination get configmap/configmap-deleted -o jsonpath='{.data.greeting}' 2> /dev/null)" == "hello" ]]; then
                found="1"
                break
            fi
            sleep 1
        done
        if [[ -z "${found}" ]]; then
            echo "[e2e][configmap_deleted]: FAIL" >&2
            return 1
        fi
        kubectl -n flector-configmap-deleted-source delete configmap configmap-deleted > /dev/null
        local found=""
        for _ in {1..100}; do
            if [[ "$(kubectl -n flector-configmap-deleted-destination get configmap/configmap-deleted 2> /dev/null)" == "" ]]; then
                found="1"
                break
            fi
            sleep 1
        done
        if [[ -z "${found}" ]]; then
            echo "[e2e][configmap_deleted]: FAIL" >&2
            return 1
        fi
        kubectl delete namespace flector-configmap-deleted-source flector-configmap-deleted-destination --ignore-not-found --wait=false > /dev/null
    }

    function step__test__secret_created() {
        kubectl delete namespace flector-secret-created-source flector-secret-created-destination --ignore-not-found > /dev/null
        kubectl create namespace flector-secret-created-source > /dev/null
        kubectl create namespace flector-secret-created-destination > /dev/null
        kubectl -n flector-secret-created-source create secret generic secret-created --from-literal=password=s3cret > /dev/null
        kubectl -n flector-secret-created-destination apply -f - > /dev/null << EOF
apiVersion: flector.io/v1alpha1
kind: Flect
metadata: { name: secret-created }
spec:
  source: { kind: Secret, namespace: flector-secret-created-source, name: secret-created }
  destinations:
    - { namespace: flector-secret-created-destination }
EOF
        local found=""
        for _ in {1..100}; do
            if [[ "$(kubectl -n flector-secret-created-destination get secret/secret-created -o jsonpath='{.data.password}' 2> /dev/null)" == "$(printf s3cret | base64)" ]]; then
                found="1"
                break
            fi
            sleep 1
        done
        if [[ -z "${found}" ]]; then
            echo "[e2e][secret_created]: FAIL" >&2
            return 1
        fi
        kubectl delete namespace flector-secret-created-source flector-secret-created-destination --ignore-not-found --wait=false > /dev/null
    }

    function step__test__secret_updated() {
        kubectl delete namespace flector-secret-updated-source flector-secret-updated-destination --ignore-not-found > /dev/null
        kubectl create namespace flector-secret-updated-source > /dev/null
        kubectl create namespace flector-secret-updated-destination > /dev/null
        kubectl -n flector-secret-updated-source create secret generic secret-updated --from-literal=password=s3cret > /dev/null
        kubectl -n flector-secret-updated-destination apply -f - > /dev/null << EOF
apiVersion: flector.io/v1alpha1
kind: Flect
metadata: { name: secret-updated }
spec:
  source: { kind: Secret, namespace: flector-secret-updated-source, name: secret-updated }
  destinations:
    - { namespace: flector-secret-updated-destination }
EOF
        local found=""
        for _ in {1..100}; do
            if [[ "$(kubectl -n flector-secret-updated-destination get secret/secret-updated -o jsonpath='{.data.password}' 2> /dev/null)" == "$(printf s3cret | base64)" ]]; then
                found="1"
                break
            fi
            sleep 1
        done
        if [[ -z "${found}" ]]; then
            echo "[e2e][secret_updated]: FAIL" >&2
            return 1
        fi
        kubectl -n flector-secret-updated-source patch secret secret-updated --type merge -p "{\"data\":{\"password\":\"$(printf news3cret | base64)\"}}" > /dev/null
        local found=""
        for _ in {1..100}; do
            if [[ "$(kubectl -n flector-secret-updated-destination get secret/secret-updated -o jsonpath='{.data.password}' 2> /dev/null)" == "$(printf news3cret | base64)" ]]; then
                found="1"
                break
            fi
            sleep 1
        done
        if [[ -z "${found}" ]]; then
            echo "[e2e][secret_updated]: FAIL" >&2
            return 1
        fi
        kubectl delete namespace flector-secret-updated-source flector-secret-updated-destination --ignore-not-found --wait=false > /dev/null
    }

    function step__test__secret_deleted() {
        kubectl delete namespace flector-secret-deleted-source flector-secret-deleted-destination --ignore-not-found > /dev/null
        kubectl create namespace flector-secret-deleted-source > /dev/null
        kubectl create namespace flector-secret-deleted-destination > /dev/null
        kubectl -n flector-secret-deleted-source create secret generic secret-deleted --from-literal=password=s3cret > /dev/null
        kubectl -n flector-secret-deleted-destination apply -f - > /dev/null << EOF
apiVersion: flector.io/v1alpha1
kind: Flect
metadata: { name: secret-deleted }
spec:
  source: { kind: Secret, namespace: flector-secret-deleted-source, name: secret-deleted }
  destinations:
    - { namespace: flector-secret-deleted-destination }
EOF
        local found=""
        for _ in {1..100}; do
            if [[ "$(kubectl -n flector-secret-deleted-destination get secret/secret-deleted -o jsonpath='{.data.password}' 2> /dev/null)" == "$(printf s3cret | base64)" ]]; then
                found="1"
                break
            fi
            sleep 1
        done
        if [[ -z "${found}" ]]; then
            echo "[e2e][secret_deleted]: FAIL" >&2
            return 1
        fi
        kubectl -n flector-secret-deleted-source delete secret secret-deleted > /dev/null
        local found=""
        for _ in {1..100}; do
            if [[ "$(kubectl -n flector-secret-deleted-destination get secret/secret-deleted 2> /dev/null)" == "" ]]; then
                found="1"
                break
            fi
            sleep 1
        done
        if [[ -z "${found}" ]]; then
            echo "[e2e][secret_deleted]: FAIL" >&2
            return 1
        fi
        kubectl delete namespace flector-secret-deleted-source flector-secret-deleted-destination --ignore-not-found --wait=false > /dev/null
    }

    function step__cleanup() {
        local unit=""
        for fn in $(compgen -A function | grep '^step__test__' | sort); do
            unit="${fn#step__test__}"
            unit="${unit//_/-}"
            kubectl -n "flector-${unit}-destination" delete flect --all --ignore-not-found || true
            kubectl delete namespace "flector-${unit}-source" "flector-${unit}-destination" --ignore-not-found --wait=false || true
        done
    }

    function all() {
        step__install
        step__test__configmap_created
        step__test__configmap_updated
        step__test__configmap_deleted
        step__test__secret_created
        step__test__secret_updated
        step__test__secret_deleted
        step__cleanup
    }

    case "${ARG_STEP}" in
    step__install) step__install ;;
    step__cleanup) step__cleanup ;;
    step__test__configmap_created) step__test__configmap_created ;;
    step__test__configmap_updated) step__test__configmap_updated ;;
    step__test__configmap_deleted) step__test__configmap_deleted ;;
    step__test__secret_created) step__test__secret_created ;;
    step__test__secret_updated) step__test__secret_updated ;;
    step__test__secret_deleted) step__test__secret_deleted ;;
    *) all ;;
    esac
}

case "${ARG_JOB}" in
job__check) job__check ;;
job__image) job__image ;;
job__chart) job__chart ;;
job__e2e) job__e2e ;;
*) help ;;
esac
