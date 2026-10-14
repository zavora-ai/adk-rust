#!/usr/bin/env bash
# Start a TLS-only PostgreSQL container with a certificate from a throwaway private CA,
# for adk-session's postgres_tls_tests.
#
# The CA is not in the Mozilla root set, so `sslmode=verify-full` succeeds only when the
# client is given `sslrootcert=<dir>/ca.crt`. The server certificate names `localhost`.
#
# Usage:
#   scripts/postgres-tls-server.sh <cert dir> [port] [image]
#   export DATABASE_URL_TLS=postgres://postgres:postgres@localhost:5433/postgres
#   export PG_TLS_ROOT_CERT=<cert dir>/ca.crt
#
# Defaults: port 5433, image pgvector/pgvector:pg17. The container is named
# adk-postgres-tls; remove it with `docker rm -f adk-postgres-tls`.

set -euo pipefail

DIR="${1:?usage: $0 <cert dir> [port] [image]}"
PORT="${2:-5433}"
IMAGE="${3:-pgvector/pgvector:pg17}"
NAME=adk-postgres-tls

mkdir -p "$DIR"
cd "$DIR"

openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj "/CN=adk-test-ca" \
    -keyout ca.key -out ca.crt \
    -addext "basicConstraints=critical,CA:TRUE" \
    -addext "keyUsage=critical,keyCertSign,cRLSign" 2>/dev/null
openssl req -newkey rsa:2048 -nodes -subj "/CN=localhost" \
    -keyout server.key -out server.csr 2>/dev/null
cat > server.ext <<'EOF'
subjectAltName=DNS:localhost,IP:127.0.0.1
basicConstraints=critical,CA:FALSE
keyUsage=critical,digitalSignature,keyEncipherment
extendedKeyUsage=serverAuth
EOF
openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial -days 2 \
    -extfile server.ext -out server.crt 2>/dev/null

# PostgreSQL refuses a key that the postgres user does not own or that others can read,
# and a bind mount keeps the host's owner, so the entrypoint installs private copies
# before it drops to the postgres user.
docker rm -f "$NAME" >/dev/null 2>&1 || true
docker run -d --name "$NAME" -p "$PORT:5432" -e POSTGRES_PASSWORD=postgres \
    -v "$PWD:/certs:ro" --entrypoint bash "$IMAGE" -c '
        install -o postgres -g postgres -m 0600 /certs/server.key /var/lib/postgresql/server.key
        install -o postgres -g postgres -m 0644 /certs/server.crt /var/lib/postgresql/server.crt
        exec docker-entrypoint.sh postgres -c ssl=on \
            -c ssl_cert_file=/var/lib/postgresql/server.crt \
            -c ssl_key_file=/var/lib/postgresql/server.key' >/dev/null

# The entrypoint's initialisation server listens on a socket only, so a TCP probe
# succeeds once the final server is up.
for _ in $(seq 60); do
    if docker exec "$NAME" pg_isready -q -h localhost -U postgres; then
        printf 'TLS PostgreSQL ready on port %s; CA certificate: %s/ca.crt\n' "$PORT" "$PWD"
        exit 0
    fi
    sleep 2
done
docker logs "$NAME" >&2
exit 1
