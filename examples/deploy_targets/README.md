# Deploy Targets

`support-desk` is an agent that runs unchanged on every `adk-deploy` target. Its
`adk-deploy.toml` is the one manifest every target plans from, and `src/main.rs` reads its
configuration from the deploy contract the target sets on the container
(`adk_deploy::targets::contract`):

| Variable | Used for |
|----------|----------|
| `BIND_HOST`, `PORT` | The listen address |
| `ADK_ALLOWED_HOSTS` | `SecurityConfig::allowed_hosts`, so the public host name is answered |
| `ADK_SERVICE_SESSIONS_URL` | The SQLite session store of the `sessions` binding, on a persistent volume |
| `ADK_AUTH_MODE`, `ADK_AUTH_REQUIRED_SCOPES` | Bearer authentication of every caller |
| `ADK_API_TOKEN`, `ANTHROPIC_API_KEY` | Secrets, injected from the provider's secret store |

The token holder gets the `admin` scope, so the kill switch (`POST /api/admin/freeze`) works for
the operator and refuses everyone else.

## Plan

```bash
cargo adk deploy --target container --plan
```

`.adk-deploy/container/` then holds the `Dockerfile`, `Dockerfile.dockerignore`, `compose.yaml`
and `kubernetes.yaml`. The plan names the secrets the provider must hold —
`ADK_API_TOKEN`, `ANTHROPIC_API_KEY`, `SESSIONS_DATABASE_URL` — and none of the files holds a
value. The build context is the repository root (`context = "../.."`), since the example's path
dependencies reach back into it.

## Run on Docker Compose

Copy `.env.example` to `.env` and fill it in, then:

```bash
docker compose --env-file .env -f .adk-deploy/container/compose.yaml up --detach --build
```

`compose.yaml` refuses to start when a secret is missing from the environment, publishes on
`127.0.0.1:8080`, mounts the `support-desk-data` volume at `/data`, and runs the agent as UID
65532 with a read-only root filesystem.

## Check it

```bash
curl -s localhost:8080/api/health                                  # 200
curl -s -o /dev/null -w '%{http_code}\n' localhost:8080/api/apps   # 401: callers authenticate
curl -s -X POST localhost:8080/api/sessions \
  -H "Authorization: Bearer $ADK_API_TOKEN" -H 'Content-Type: application/json' \
  -d '{"appName":"support_desk","userId":"operator","sessionId":"s1"}'
curl -s -X POST localhost:8080/api/run \
  -H "Authorization: Bearer $ADK_API_TOKEN" -H 'Content-Type: application/json' \
  -d '{"appName":"support_desk","userId":"operator","sessionId":"s1",
       "newMessage":{"role":"user","parts":[{"text":"What does a health check do?"}]}}'
curl -s -X POST localhost:8080/api/admin/freeze \
  -H "Authorization: Bearer $ADK_API_TOKEN" -H 'Content-Type: application/json' \
  -d '{"reason":"maintenance"}'                                    # runs now answer 403
```

The session survives `docker compose restart`: it is in SQLite on the volume.

## Run without a container

The binary reads the same variables, so the contract runs directly:

```bash
set -a; . ./.env; set +a
BIND_HOST=127.0.0.1 ADK_AUTH_MODE=bearer ADK_AUTH_REQUIRED_SCOPES=agent:invoke \
  ADK_SERVICE_SESSIONS_URL="sqlite://./sessions.db?mode=rwc" cargo run
```

## Kubernetes

Create the `support-desk-secrets` `Secret` with the three keys from your secret store, then
`kubectl apply -f .adk-deploy/container/kubernetes.yaml`. The `Deployment` probes
`/api/health`, and the `PersistentVolumeClaim` holds the session store.
