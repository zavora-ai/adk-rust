# The Website's Views

`adk-surface`'s `website` feature puts the owner's side of a prebuilt website
([`adk-website`](../deployment/adk-website.md)) in the web home, at `/ui/website` (spec 3.5
§7.1, §8.2): the setup code, onboarding, the charter, the site view, the weekly review and the
web editor. The views read and act only through `adk_website::ui::WebsiteDesk`, under
`/api/surface/website`; the **Website** tab appears in the header only when the server serves
one.

> **Note:** the `website` feature is **Experimental**, as `adk-website` is (see `STABILITY.md`).
> It implies `web`.

![The site view a week after launch](../images/surfaces/website/10-site-view-light.png)

## Try it

The `scripted_website` example serves a bakery's site over a scripted pipeline, fresh or a
week after launch.

```bash
# A fresh site: prints a setup code and writes it to /tmp/website/setup-code
cargo run -p adk-surface --features website --example scripted_website -- 8803 /tmp/website

# The same site a week after launch: releases, KPIs, jobs, signals, a wallet, held updates
cargo run -p adk-surface --features website --example scripted_website -- 8804 /tmp/website-live --live
```

Open `http://127.0.0.1:8803/ui/website`. A fresh site asks for its setup code and takes no
onboarding input until a device claims it (spec 3.5 A-I20).

## Mount it on your own server

`adk_surface::web::website::router(desk)` serves the desk's API, relative to the `/api` prefix;
`setup_router(desk)` serves the setup code's two routes as root routes, because the person
claiming a fresh site has no credentials yet: the code is the credential. Both go on
`ServerBuilder` beside the engine's API and the page.

```rust
use adk_server::ServerBuilder;
use adk_surface::SurfaceEngine;
use adk_website::ui::WebsiteDesk;

fn mount(server: ServerBuilder, engine: SurfaceEngine, desk: WebsiteDesk) -> ServerBuilder {
    server
        .add_api_routes(adk_surface::web::router(engine))
        .add_api_routes(adk_surface::web::website::router(desk.clone()))
        .add_root_routes(adk_surface::web::website::setup_router(desk))
        .add_root_routes(adk_surface::web::home())
}
```

`WebsiteDesk::builder(site, name, owner, runtime)` takes the site, its name on screen, the
owner who holds every role until the first charter names its people, and the site's
`SiteRuntime`; the builder adds the setup gate, the fact store, the autonomy ladder, signals,
the wallet, the owner's channel, discovery, connectors and the media pipeline.
`adk-surface/examples/scripted_website/site.rs` builds one over every part.

## The views

| View | Address | What it shows | What a person decides |
|---|---|---|---|
| Setup code | First, while the site needs one | The site's name, where the code is, what visitors see meanwhile | The code pairs this device as the owner's |
| Onboarding | `/ui/website/setup` | The nine stages, the one to do now: where to start, what discovery found with its sources, the questions with a live preview, the charter, the looks, the launch check with every page, connections and going live, then the first 90 days | Each found fact (**That's right**, **Fix it**, **Forget it**), the kind, the freeze, each answer, each charter section, the look, each connection, going live |
| Charter | Onboarding's fifth stage | Each section as a card, who signs, the version in force | **Looks right** or **Change it** per section; **Sign as** for each role the person holds |
| Site view | `/ui/website` | Status, the live release, releases with the pages each changed, KPIs with trends, jobs with their levels, alarms, held updates and batches, the wallet, signals | Promote or lower a job; publish or discard a held item; apply or discard a held feed delivery whole; acknowledge an alarm |
| Weekly review | `/ui/website/review` | Measures against baselines and targets, what each agent did, money, updates from feeds, experiments, leads, incidents, freedom earned and lost, alarms that waited, questions; earlier reviews | Compose this week's review; send its summary to the owner's channel; accept an earned promotion |
| Web editor | `/ui/website/write` | A form per content kind, Markdown with a preview, images, the schedule, the checks, every page the change touches | Save, preview, publish or schedule, approve or refuse under a chain, correct or withdraw what is live, discard |

The editor has no page builder (spec 3.5 N4): each kind has its form, and the theme sets the
layout. An image needs a confirmed description before the accessibility check passes.
Connections never ask for a password: each opens the service's own page or shows a code.

| Light | Dark |
|---|---|
| ![Setup code](../images/surfaces/website/01-setup-code-light.png) | ![Setup code, dark](../images/surfaces/website/01-setup-code-dark.png) |
| ![Discovery](../images/surfaces/website/03-discovery-light.png) | ![Discovery, dark](../images/surfaces/website/03-discovery-dark.png) |
| ![The interview with its live preview](../images/surfaces/website/04-interview-light.png) | ![The interview, dark](../images/surfaces/website/04-interview-dark.png) |
| ![The charter](../images/surfaces/website/05-charter-light.png) | ![The charter, dark](../images/surfaces/website/05-charter-dark.png) |
| ![The launch check](../images/surfaces/website/07-launch-check-light.png) | ![The launch check, dark](../images/surfaces/website/07-launch-check-dark.png) |
| ![Connect and go live](../images/surfaces/website/08-connect-light.png) | ![Connect and go live, dark](../images/surfaces/website/08-connect-dark.png) |
| ![The weekly review](../images/surfaces/website/11-weekly-review-light.png) | ![The weekly review, dark](../images/surfaces/website/11-weekly-review-dark.png) |
| ![The web editor](../images/surfaces/website/12-editor-light.png) | ![The web editor, dark](../images/surfaces/website/12-editor-dark.png) |

## Who may do what

Every action names the person who takes it, as the server's extractor authenticated them;
every route of `router` answers `401` without a caller. Roles come from the charter in force.

| Action | Who | Spec 3.5 |
|---|---|---|
| Onboarding input | The device the setup code paired: its token, kept in the tab, goes as `Authorization: Bearer` | A-I20 |
| Signing the charter | Each person the approval chain names, in their role, from any device; each a distinct person | A-I14, A-I23 |
| Promoting or lowering a job | An owner; the site lowers a job on its own too, and never raises one | A-I13 |
| Publishing or discarding a held item | An owner who did not propose the change | A-I23, A-I25 |
| Applying or discarding a held feed delivery | An owner, whole | A-I27 |
| Writing | People holding the editor, publisher or owner role | A-I24 |
| Approving a release under a chain | Each role a distinct person who did not propose it | A-I23 |

No agent acts here: an agent principal holds no role. Before the first charter, the owner the
desk was built for holds the owner, editor and publisher roles.

## Routes

| Route | Answer |
|---|---|
| `GET /api/surface/website/setup` (root) | The setup screen |
| `POST /api/surface/website/setup/claim` (root) | The paired device's token, shown once |
| `GET /api/surface/website` | The site's name, whether it is live, the caller's roles |
| `GET`, `POST /api/surface/website/onboarding` | Onboarding and its commands; both need the paired device's token |
| `GET`, `POST /api/surface/website/charter` | The charter under review and its commands |
| `GET`, `POST /api/surface/website/site` | The site view and the owner's decisions on it |
| `GET`, `POST /api/surface/website/review` | The weekly review (`?week=`), composing and sending it |
| `GET`, `POST /api/surface/website/editor` | The editor's content and drafts, and its commands |
| `GET /api/surface/website/editor/drafts/{id}` | One draft |
| `POST /api/surface/website/editor/drafts/{id}/media` | One file through the media pipeline, at most 12 MiB |
| `GET /api/surface/website/openapi.json` | This API's OpenAPI document |

Errors are problem documents with the desk's `website.*` code.

## Building the front end

The views live in `adk-surface/web/src/website/`, and their TypeScript types in
`src/generated/website.ts` are generated from `adk-website`'s view types:
`bash scripts/check-surface-api.sh` fails when they drift, and `--write` regenerates them.

```bash
cd adk-surface/web
npm ci
npm run verify                                   # type check, component tests, build
npx playwright test --project website            # a fresh site, claimed and onboarded to release 1
npx playwright test --project website-live       # the site view, the review and the editor
WEBSITE_E2E_SCREENSHOTS=1 npx playwright test --project website --project website-live
```

`/ui/fixture.html?screen=website` shows the views over the fixture the component tests use.
With `WEBSITE_E2E_SCREENSHOTS` set, the end-to-end tests write every screen in light and dark
to `docs/official_docs/images/surfaces/website/`.

## Related

- [The Web Home](web-home.md) — the page the views are a section of
- [The AWP website product](../deployment/adk-website.md) — the site kinds, onboarding's stages, the team and the weekly review
- [The website runtime](../harness/website.md) — the harness half the site's agents run on
