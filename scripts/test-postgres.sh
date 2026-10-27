#!/usr/bin/env bash
# Run the #[ignore]d PostgreSQL suites of the database crates against a live server.
#
# The suites need a PostgreSQL 17 server with the pgvector extension available, such as
# the `pgvector/pgvector:pg17` image. They create their own schemas and databases and
# drop them, so a scratch database is enough. The TLS suite runs when DATABASE_URL_TLS
# and PG_TLS_ROOT_CERT name a server started by scripts/postgres-tls-server.sh; under CI
# it is required.
#
# Usage:
#   docker run -d -p 5432:5432 -e POSTGRES_PASSWORD=postgres pgvector/pgvector:pg17
#   DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres scripts/test-postgres.sh
#
# Environment:
#   DATABASE_URL       server for every suite (required)
#   DATABASE_URL_TLS   TLS server for adk-session's postgres_tls_tests (optional)
#   PG_TLS_ROOT_CERT   CA certificate of the TLS server
#   NEXTEST_PROFILE    nextest profile (default: default; CI passes ci)

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT" || exit 1

: "${DATABASE_URL:?set DATABASE_URL to a PostgreSQL server with the pgvector extension}"
PROFILE="${NEXTEST_PROFILE:-default}"
failed=()

suite() {
    local label="$1"
    shift
    printf '\n==> %s\n' "$label"
    if ! cargo nextest run --profile "$PROFILE" --run-ignored only --no-fail-fast "$@"; then
        failed+=("$label")
    fi
}

# The extension lives in `public`, where every suite's search path finds it. A suite
# that created it inside its own scratch schema would drop it for the others.
printf '==> pgvector extension\n'
if ! command -v psql >/dev/null; then
    printf 'psql not found; the suites create the extension themselves\n'
elif ! psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -qc 'CREATE EXTENSION IF NOT EXISTS vector SCHEMA public'; then
    printf 'error: CREATE EXTENSION vector failed; use a server with pgvector, such as pgvector/pgvector:pg17\n' >&2
    exit 1
fi

suite "adk-session" -p adk-session --features sqlite,postgres \
    --test postgres_state_tests \
    --test postgres_migrate_lock_tests \
    --test store_conformance_postgres \
    --test spend_ledger_postgres \
    --test session_contract_postgres \
    --test sqlx_migration_inputs_postgres \
    --test upgrade_from_sqlx_0_8 \
    --test approval_session_upgrade \
    --test approval_session_property_tests

suite "adk-memory" -p adk-memory --features sqlite-memory,database-memory \
    --test postgres_migrate_lock_tests \
    --test postgres_schema_detection_tests \
    --test postgres_memory_round_trip_tests \
    --test memory_service_conformance_tests \
    --test sqlx_migration_inputs_postgres \
    --test upgrade_from_sqlx_0_8

# The feature set matches the Redis step of CI's postgres job, so both reuse one build.
suite "adk-harness" -p adk-harness --features memory,postgres,redis \
    --test fact_conformance_tests \
    --test shared_store_postgres_tests

suite "adk-graph" -p adk-graph --features postgres --test checkpointer_conformance_postgres

# The feature set matches the kernel's Redis step of CI's postgres job, so both reuse one build.
suite "adk-kernel" -p adk-kernel --features postgres,redis,channels,charter --test process_store_postgres \
    --test agenda_store_postgres \
    --test driver_store_postgres \
    --test ledger_store_postgres \
    --test device_store_postgres \
    --test control_store_postgres \
    --test channel_store_postgres \
    --test partition_store_postgres \
    --test cron_watermark_store_postgres \
    --test agents_store_postgres \
    --test charter_store_postgres

suite "adk-auth" -p adk-auth --features postgres-audit --test audit_postgres_tests

suite "adk-rag" -p adk-rag --features pgvector --test vector_store_contract_pgvector

suite "adk-managed" -p adk-managed --test durable_round_trip_tests -E 'test(/_on_postgres$/)'

# The feature set matches the wallet's Redis step of CI's postgres job, so both reuse one build.
suite "adk-payments" -p adk-payments --features sqlite,postgres,redis --test wallet_store_postgres

# The feature set matches the edge stores' Redis step of CI's postgres job, so both reuse one build.
suite "adk-server" -p adk-server --features awp-sqlite,awp-postgres,awp-redis --test awp_store_postgres

# The spec 3.3 §12 scenarios' PostgreSQL legs. Each `adk-rust up` leg makes a database of its
# own; one feature set builds them all, and the `adk-rust` binary the examples below run.
suite "adk-cli scenarios" -p adk-cli \
    --features scripted-model,audit-sql,postgres,voice,verb-ui,mobile \
    --test hire_a_helper \
    --test voice_up \
    --test verbs_added_agent \
    --test speak_glance_stop \
    -E 'test(/postgres/)'

suite "adk-surface workflow" -p adk-surface --features web,workflow,genui,tui \
    --test workflow_scenario_tests

# `DATABASE_URL` with its database replaced by `$1`, its query kept.
database_url() {
    local base="${DATABASE_URL%%\?*}" query=""
    if [[ "$DATABASE_URL" == *\?* ]]; then
        query="?${DATABASE_URL#*\?}"
    fi
    printf '%s/%s%s' "${base%/*}" "$1" "$query"
}

# The governed form's leg needs an empty database of its own.
genui_database="genui_form_$$"
if ! command -v psql >/dev/null; then
    printf '\nerror: psql not found; the governed form needs an empty database\n' >&2
    failed+=("adk-surface genui")
elif psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -qc "CREATE DATABASE $genui_database"; then
    DATABASE_URL="$(database_url "$genui_database")" suite "adk-surface genui" -p adk-surface \
        --features web,workflow,genui,tui --test genui_scenario_tests -E 'test(/on_postgres/)'
    psql "$DATABASE_URL" -qc "DROP DATABASE IF EXISTS $genui_database WITH (FORCE)"
else
    failed+=("adk-surface genui")
fi

# Phases 1 and 2 on `adk-rust up`, as programs: each makes its database and drops it.
scenario() {
    local example="$1" target="${CARGO_TARGET_DIR:-$ROOT/target}"
    printf '\n==> %s on PostgreSQL\n' "$example"
    if ! CARGO_TARGET_DIR="$target" cargo run --locked \
        --manifest-path "examples/$example/Cargo.toml" -- --store postgres \
        --bin "$target/debug/adk-rust"; then
        failed+=("$example")
    fi
}
scenario surface_approve_tomorrow
scenario surface_morning_after

if [[ -n "${DATABASE_URL_TLS:-}" ]]; then
    suite "adk-session TLS" -p adk-session --features postgres --test postgres_tls_tests
elif [[ -n "${CI:-}" ]]; then
    printf '\nerror: DATABASE_URL_TLS is unset; CI runs the TLS suite\n' >&2
    failed+=("adk-session TLS")
else
    printf '\n==> adk-session TLS skipped: DATABASE_URL_TLS is unset\n'
fi

printf '\npostgres suites: %d failed\n' "${#failed[@]}"
for entry in ${failed[@]+"${failed[@]}"}; do
    printf '  FAILED  %s\n' "$entry"
done
(( ${#failed[@]} == 0 ))
