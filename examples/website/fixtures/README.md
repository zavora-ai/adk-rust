# Fixtures

The local fixtures of spec 3.5 §11. Every fixture binds to loopback; nothing exploits anything.

| Fixture | Contents | Used by |
|---|---|---|
| `wordpress/compose.yaml` | The current WordPress release on PHP 8.4 (the patched line) with MariaDB 11.4 on `127.0.0.1:8080`; `seed.sh` installs a contact form (Contact Form 7), a small shop (WooCommerce) and the posts, pages, products and legacy redirect the recordings hold | The nightly `website-wordpress` job |
| `wordpress/compose.infected.yaml`, `wordpress/infected/` | The `infected` overlay: hidden spam links to `.test` domains, a script that decodes harmless text, a crawler-only script from a domain that never resolves, an inert `.php` in uploads and a file whose name claims an image | S3 at night |
| `recorded/` | The inventories `IsolatedReplay`, `WordPressRest` and `Wxr` take of the recordings | S3 and the pull-request tier |
| `presence/` | A business's listing, review page and social profile, with JSON-LD and Open Graph | S2 |
| `empty/` | Nothing: the start of S1 | S1 |

The recordings the pull-request tier serves — REST collections, crawled pages, uploads and
the overlay — live in `adk-site/tests/fixtures/migrate/`, shared with `adk-site`'s migration
tests. `website-scenarios record` re-records `recorded/` from them; the nightly
`website-scenarios wordpress` checks that the compose fixture still serves what they hold.

## Start the WordPress fixture

```bash
cd examples/website/fixtures/wordpress
docker compose up -d --wait
docker compose run --rm seed
cargo run --manifest-path ../../Cargo.toml -- wordpress http://127.0.0.1:8080

# The infected overlay
docker compose -f compose.yaml -f compose.infected.yaml up -d --wait
docker compose -f compose.yaml -f compose.infected.yaml run --rm infect
cargo run --manifest-path ../../Cargo.toml -- wordpress http://127.0.0.1:8080 --infected

docker compose down -v
```
