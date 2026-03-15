# Spatial OS Sample Agent Apps

This sample pack provides manifest-driven apps you can import directly into `adk-spatial-os`.

Included apps:

- `sre-war-room` (`manifest.json`)
- `support-triage-desk` (`manifest.json`)
- `release-manager` (`deploy_manifest.json`, Studio-style)
- `customer-comms` (`manifest.json`)
- `exec-briefing` (`manifest.json`)

## Run Spatial OS

```bash
cargo run -p adk-spatial-os
```

Default URL: `http://127.0.0.1:8199`

## Import All Sample Apps

```bash
./examples/spatial_os_apps/import_all.sh
```

Optional custom URL:

```bash
ADK_SPATIAL_OS_URL=http://127.0.0.1:8200 ./examples/spatial_os_apps/import_all.sh
```

## Import One App Manually

```bash
curl -sS -X POST http://127.0.0.1:8199/api/os/apps/import \
  -H 'content-type: application/json' \
  -d '{
    "path": "examples/spatial_os_apps/sre-war-room",
    "source": "sample_pack",
    "on_conflict": "upsert"
  }'
```

## Quick Prompts

- `Identify top degraded services and propose safest remediation order.`
- `Draft an external customer incident update from current state.`
- `Generate a one-page executive risk memo with go/no-go recommendation.`
