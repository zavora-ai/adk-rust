#!/usr/bin/env bash
# Installs the apt packages named in APT_PACKAGES on a GitHub-hosted Ubuntu runner.
#
# The runner's Azure mirror at times serves archives at 40-130 kB/s for a whole
# run, so a single apt invocation can outlast the step's timeout. Each
# `apt-get update` and each download attempt is bounded and retried; archives an
# interrupted attempt already fetched stay in apt's cache, so a retry resumes
# instead of starting over. dpkg runs only once every archive is on disk, so a
# timeout never interrupts it.

set -euo pipefail

read -r -d '' -a requested <<<"${APT_PACKAGES:-}" || true
if ((${#requested[@]} == 0)); then
  echo "::error::apt-install: the packages input is empty"
  exit 1
fi

missing=()
for package in "${requested[@]}"; do
  if [[ "$(dpkg-query -W -f="\${Status}" "$package" 2>/dev/null)" != "install ok installed" ]]; then
    missing+=("$package")
  fi
done
if ((${#missing[@]} == 0)); then
  echo "Already installed: ${requested[*]}"
  exit 0
fi

apt_get=(apt-get -o Acquire::Retries=3 -o Acquire::http::Timeout=30
  -o Acquire::https::Timeout=30 -o DPkg::Lock::Timeout=300)

# Usage: attempt <label> <tries> <seconds per try> <command...>
attempt() {
  local label=$1 tries=$2 limit=$3 n status
  shift 3
  for ((n = 1; n <= tries; n++)); do
    status=0
    sudo timeout --kill-after=10 "$limit" "$@" || status=$?
    if ((status == 0)); then
      return 0
    fi
    if ((status == 124 || status == 137)); then
      echo "::warning::$label: attempt $n of $tries exceeded ${limit}s"
    else
      echo "::warning::$label: attempt $n of $tries exited with status $status"
    fi
    if ((n < tries)); then
      sleep 3
    fi
  done
  echo "::error::$label failed after $tries attempts"
  return 1
}

echo "::group::apt-get update"
attempt "apt-get update" 3 45 "${apt_get[@]}" update
echo "::endgroup::"

echo "::group::Download ${missing[*]}"
attempt "Download ${missing[*]}" 4 90 "${apt_get[@]}" install -y --download-only "${missing[@]}"
echo "::endgroup::"

echo "::group::Install ${missing[*]}"
sudo "${apt_get[@]}" install -y --no-download "${missing[@]}"
echo "::endgroup::"
