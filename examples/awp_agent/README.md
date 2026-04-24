# AWP Agent Example

An AWP-compliant agent server that demonstrates the full Agentic Web Protocol integration with ADK-Rust.

## What This Shows

- Loading a `business.toml` with the full AWP schema (business identity, brand voice, products, channels, payments, support, outreach)
- Serving all 7 AWP protocol endpoints with version negotiation middleware
- Running an LLM agent whose instructions are derived from the business context
- Exercising every endpoint with an HTTP client to verify compliance

## AWP Endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/.well-known/awp.json` | Discovery document |
| GET | `/awp/manifest` | Capability manifest (JSON-LD) |
| GET | `/awp/health` | Health state |
| POST | `/awp/events/subscribe` | Create event subscription |
| GET | `/awp/events/subscriptions` | List subscriptions |
| DELETE | `/awp/events/subscriptions/{id}` | Delete subscription |
| POST | `/awp/a2a` | A2A message handler |

## Custom Endpoints

| Method | Path | Description |
|--------|------|-------------|
| POST | `/ask` | Ask the LLM agent a question |
| GET | `/api/products` | Product catalog |

## Prerequisites

- `GOOGLE_API_KEY` environment variable set

## Run

```bash
cd examples/awp_agent
cp .env.example .env   # add your GOOGLE_API_KEY
cargo run
```

## How It Works

1. **business.toml** defines the site: name, products, policies, brand voice, payment config
2. **BusinessContextLoader** parses and validates the TOML file
3. **LLM agent** receives the business context as system instructions
4. **awp_routes()** registers all AWP protocol endpoints with version negotiation
5. The example exercises every endpoint and prints the results

## AWP Compliance Verified

- ✓ Discovery document at `/.well-known/awp.json`
- ✓ Capability manifest with JSON-LD `@context` and `@type`
- ✓ Version negotiation (accept compatible, reject incompatible)
- ✓ Health state machine (Healthy/Degrading/Degraded)
- ✓ Event subscription CRUD with HMAC-SHA256 signing
- ✓ A2A message handling
- ✓ LLM agent with business context instructions
