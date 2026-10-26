# Agent Surfaces: Phone

The phone is the web home installed on a phone and woken by push (spec 3.3 §5.4). A wake-up
says only "A decision is waiting — Open the app to see it". **Decisions** carries the waiting
count on its heading, its tab and the app's icon; a decision answered here or elsewhere is one
line, "Send the October invoice · You said yes in Telegram at 07:31 · done once", which a tap
opens to show the exact command again and which answers nothing; a high-risk decision shows
the exact command and asks the person to confirm it is them before it is approved. The app
fills the screen and never scrolls as a page: each screen scrolls inside itself above the
tabs.

The `mobile` feature of `adk-surface` ships the phone's routes, Web Push from the person's own
kernel and "Confirm it's you". It implies `web`.

> **Note:** the phone is Beta. "Confirm it's you" checks passkeys with webauthn-rs, which links
> the system's OpenSSL: on Linux and macOS the system library (`libssl-dev`, Homebrew
> `openssl@3`), on Windows and in cross builds an OpenSSL the build can find
> (`OPENSSL_DIR`, or vcpkg on Windows). A build without `mobile` needs no OpenSSL.

```toml
[dependencies]
adk-surface = { version = "3.0.0", features = ["mobile"] }
```

## Serving the phone with `adk-rust up`

`adk-rust up --home-url` serves the phone beside the web home. The `mobile` feature of
`adk-cli` is opt-in, since it adds the OpenSSL dependency:

```sh
cargo install adk-cli --features mobile
```

The phone needs an https address the person's devices can reach. On a tailnet, Tailscale Serve
gives one, with a certificate, without opening a port to the internet:

```sh
export ADK_SERVE_TOKEN="$(openssl rand -hex 32)"
tailscale serve --bg 8080
adk-rust up --home-url https://kernel.tail1234.ts.net
```

| Flag | What it does |
|---|---|
| `--home-url <https URL>` | The address people open the home at. Pairing links open `<home>/ui/pair#code=…`, the WebAuthn relying party is its host, and the server answers its host and origin beside loopback. Telegram cards link to `<home>/ui/` |
| `--auth-token <token>` | Required with `--home-url` (or `ADK_SERVE_TOKEN`): behind a proxy every request comes from loopback, so the operator signs in to the home with the token and each phone with its device token |
| `--push-key <file>` | The VAPID key file replicas share; `<kernel home>/push.key` by default |

Quiet time comes from `[kernel] quiet_hours` in `harness.toml`. Without `--home-url` the
phone's routes answer `400 surface.phone.needs_home_url` ("start adk-rust up with --home-url").

## What the phone may do

A paired phone acts with its device token, scoped `read`, `decide` and `stop`, expiring and
revocable (spec 3.3 §10 K2, S-I7). It answers as the phone, by touch, never as the web home:

| On the phone | What happens |
|---|---|
| **Allow once**, **Always allow**, **Edit**, **Deny** on a low- or medium-risk call | Answered through `POST /api/surface/decisions/{id}/answer` at the version the card showed (S-I2) |
| **Confirm it's you and approve** on a high-risk call or one that moves money | Face ID, Touch ID or the phone's screen lock, then the answer (§8.2). The plain answer route refuses it: `403 surface.confirm_presence` |
| **Deny** on a high-risk call | Answered at once: denying is always safe |
| **Stop all** | Every agent stops (`POST /api/surface/stop`, S-I10) |
| **Resume all**, send a message, change the mode, pair a device | Not on the phone: commands are closed to devices (`403 surface.not_here`) |

Every answer is recorded as `surface.decide` with `surface: "phone"`, `origin: {"kind":"touch"}`,
the device, and `presence: "confirmed"` when the person confirmed it is them.

A device's kind is set by whoever issued its pairing code. Only a device of kind `phone` reaches
the phone's routes; a remote terminal's token gets `403 surface.not_here` on them.

## Wake-ups

Each wake-up comes from the person's own kernel, encrypted for the phone (aes128gcm) and signed
with the kernel's VAPID key, with `Urgency: high` and a 12-hour TTL. Its body is
`{"v":1,"id":"…"}` and nothing else: no tool, argument, heading or agent (S-I5). The service
worker shows the fixed words, then reads the waiting count for the icon badge and acknowledges
the wake-up, which records its latency.

| When | What the phone gets |
|---|---|
| A decision starts waiting outside quiet time | One wake-up per paired phone of the person who may answer it |
| A decision starts waiting during quiet time | Nothing until quiet time ends; it is recorded as held |
| Quiet time ends, or the kernel starts | One wake-up per phone whose person has a waiting decision, whatever the number of decisions |
| The same moment on two replicas | One wake-up: each is claimed in the shared store before it is sent |
| The push service answers `404` or `410` | The subscription is removed; the phone subscribes again from **Memory** |

Quiet time is the kernel's (`[kernel] quiet_hours` in `harness.toml`, or a change made in the
home while the kernel runs). A wake-up follows the area of its run's process, and the default
area otherwise; the wake-up at the start and when quiet time ends follows the default area.

A subscription must point at a browser's push service over https: Apple's
(`*.push.apple.com`), Google's (`fcm.googleapis.com`), Mozilla's (`*.push.services.mozilla.com`)
or Microsoft's (`*.notify.windows.com`). Anything else, an IP address or plain http is refused
with `400 surface.phone.push_endpoint`.

Every wake-up writes `surface.notify` with the device, the decisions it covers and its
outcome: `held`, `sent` or `dropped`. Subscribing and unsubscribing write `surface.phone.push`.

### The push key

The VAPID key is a P-256 key kept in the kernel home as `push.key`, readable by its owner only.
Every subscription remembers the id of the key it was made with, and a server sends only with
that key, so replicas share one key file. Deleting the file makes a new key; every phone then
allows notifications again.

## Confirm it's you

"Confirm it's you" uses a passkey: the phone's, set up within 10 minutes of pairing, or one on
the computer for the web home. A passkey is a platform credential with user verification,
attestation `none`, for the home's host as its relying party.

| Step | Route | Bound to |
|---|---|---|
| 1. Start | `POST /api/surface/decisions/{id}/confirm/options` `{version, answer, editedArgs?, expanded}` | The holder (this phone, or the person at the web home), the decision, its version, the answer and the edited arguments; single use, for two minutes |
| 2. The passkey | `navigator.credentials.get` on the device | The challenge of step 1, the home's origin and host |
| 3. Answer | `POST /api/surface/decisions/{id}/confirm` `{challengeId, credential}` | Checked against step 1, then the answer is decided with presence |

A confirmation started for one decision, answer or set of arguments is refused for any other,
used once, and refused once it is two minutes old. An assertion from another device's or
person's passkey, for another origin or host, without user verification, or with a signature
counter that went backwards is refused with `403 surface.phone.passkey_refused`. Presence never
comes from request metadata on these routes.

The two requests of one confirmation reach the same server: behind a load balancer, route them
with sticky sessions.

### In the web home

The person sets up a passkey on the computer with **Use your face or fingerprint on this
computer** in "Where you can say yes from", through the same passkey routes on the operator's
own sign-in. It is listed there as "Face or fingerprint on this computer" with **Remove**, and
recorded as `surface.phone.passkey`. Answering a high-risk call in the home then goes through
the same three steps: the commands route refuses the answer with `403
surface.confirm_presence`, and the home confirms with the computer's passkey. Without one, the
first step answers `403 surface.phone.no_passkey` and says where to set it up.

> **Important:** if a passkey cannot be used inside the installed app on a phone — it has none,
> or the check fails — the phone offers **Deny** only for high-risk calls, with "Approve it in
> the home on your computer". It never approves a high-risk call without presence.

## Installing and pairing

1. In the home on the computer, choose **Add a phone or app** in "Where you can say yes from".
   The home shows a QR code, the link `https://<home>/ui/pair#code=…` and the code.
2. Open the link on the phone.
   - **iPhone:** Safari says "Add to Home Screen first" and shows the code. Add it, open the app
     from the Home Screen and type the code: a Home Screen app keeps its storage apart from
     Safari's.
   - **Android:** Chrome offers **Install the app**; the code is claimed in the app.
3. Tap **Pair this phone**, then **Allow notifications**, then **Use your face or fingerprint**.
   Notifications that cannot be turned on do not stop pairing: **Memory** on the phone offers
   them again.

The home lists the phone as "Your phone · asks for your face or fingerprint" once its passkey
is set up.

The device token is kept in the app's IndexedDB, so it survives a relaunch; a script running in
the page can read it, as it can the operator's token in the web home. **Sign out** in **Memory**
forgets it; **Remove** in the home revokes it, ends its open feeds and removes its push
subscription and passkey. If the system clears the app's storage, the phone pairs again.

## Mounting the phone

```rust
use std::sync::Arc;

use adk_core::{InMemoryApprovalStore, StoreApprovalService};
use adk_kernel::pairing::{InMemoryDeviceStore, Pairing};
use adk_session::InMemorySessionService;
use adk_surface::mobile::{Phone, PushDoorbell, PushKey};
use adk_surface::{LocalSource, SurfaceEngine};

fn phone_routes(home: &std::path::Path) -> Result<axum::Router, Box<dyn std::error::Error>> {
    // The doorbell hears about every decision; add it to the approval service first.
    let doorbell = Arc::new(PushDoorbell::new());
    let approvals = Arc::new(
        StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
            .with_notifier(doorbell.clone()),
    );
    let source = LocalSource::builder("adk-rust")
        .sessions(Arc::new(InMemorySessionService::new()))
        .approvals(approvals.clone())
        .build()?;
    let engine = SurfaceEngine::builder(Arc::new(source)).stops_need("operator").build();
    let pairing = Pairing::new(Arc::new(InMemoryDeviceStore::new()));
    let phone = Phone::builder(engine.clone(), pairing)
        .home_url("https://home.example.ts.net")
        .push_key(PushKey::load_or_create(&home.join(PushKey::FILE))?)
        .doorbell(doorbell)
        .approvals(approvals)
        .build()?;
    // Wake-ups start once a Tokio runtime runs.
    let _push = phone.start_push()?;
    Ok(adk_surface::web::router(engine.clone())
        .merge(adk_surface::web::devices_router(engine))
        .merge(adk_surface::mobile::router(phone)))
}
```

Mount the merged router with `ServerBuilder::add_api_routes` and the page with
`ServerBuilder::add_root_routes(adk_surface::web::home())`. For devices to reach these routes
with their tokens, the pairing opens them with
`Pairing::with_device_routes(Arc::new(adk_surface::web::device_scope_for))`, the server's
extractor is a `DeviceTokenExtractor`, the engine's routes come from
`adk_surface::web::router_with_devices`, and `adk_kernel::control::guard_devices` wraps the
built app. The `scripted_phone` example of `adk-surface` is a complete server: a scripted kernel
whose held calls are all high risk, which the phone's end-to-end test pairs, confirms on and
approves from:

```sh
cargo run -p adk-surface --features mobile --example scripted_phone -- 8799 /tmp/phone-workspace
```

Open `http://localhost:8799/ui/`, not `127.0.0.1`: a passkey's relying party is a domain. The page serves the service worker
(`/ui/sw.js`), the manifest (`/ui/manifest.webmanifest`) and the icons with `Cache-Control:
no-cache`.

Without a home URL, the phone's routes answer `400 surface.phone.needs_home_url`. The home URL
must be https, or `http://localhost` for tests: WebAuthn needs a secure context and refuses IP
addresses.

## When a native app follows

A native wrapper (Capacitor, or Tauri once it has first-party remote push) follows only when one
of these triggers fires (spec 3.3 §5.4):

- iOS web push misses decisions in dogfooding;
- voice must keep working with the screen off;
- or the project funds a push relay and app-store accounts.

A fourth is recorded for this package: passkeys do not work inside an installed iOS Home Screen
app.

## Tested devices

| Device | System | Browser | Push | Passkey in the installed app |
|---|---|---|---|---|
| — | — | — | Not yet run | Not yet run |

## Push delivery

| Period | Wake-ups | Median latency | Missed |
|---|---|---|---|
| — | — | — | — |

## Related

- [The Engine](index.md) — feed, projection, commands and what each surface may decide
- [The Web Home](web-home.md) — the page the phone installs
- [Pairing](../kernel/pairing.md) — device codes, tokens and scopes
