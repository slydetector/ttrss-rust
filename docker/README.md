# Docker

The blessed image + stack for the Rust tt-rss server: a single container
replacing the official PHP stack's nginx + PHP-FPM + updater (the server
serves static assets, dynamic routes and runs the feed daemon in-process).

## Files

| file | purpose |
|---|---|
| `../Dockerfile` | the image (repo root = build context) |
| `../.dockerignore` | context exclusions |
| `compose.yaml` | the blessed 2-service stack (postgres + app) |
| `.env.example` | app configuration template (copy to `.env`) |

`shadow/` is a separate, prod-copy confidence stack (docs/10) that builds the
*same* `Dockerfile`.

## Build

```sh
docker build -t ttrss-rust .
```

## Run (blessed stack)

```sh
cd docker
cp .env.example .env        # then edit (at minimum: HTTP_PORT, TTRSS_DB_PASS)
docker compose up -d
```

First boot with the stock credentials shows the tt-rss install banner; the
`admin` password is printed in the app logs (`docker compose logs app`) and is
random unless `ADMIN_USER_PASS` is set. Add a feed and the in-container
daemon updates it — there is no separate updater service.

## Configuration

All app configuration is `.env` (see `.env.example`): values pass to the app
verbatim (`env_file:`) and interpolate `compose.yaml`. The image carries
sane defaults for `TTRSS_LISTEN_ADDR` / `TTRSS_DATA_DIR` /
`TTRSS_DOCROOT`; anything in `.env` overrides them. `HTTP_PORT` (host port)
has **no** default and must be defined.

Secret-style keys (`TTRSS_DB_PASS`, `TTRSS_ENCRYPTION_KEY`, …) are expanded
in-app at startup (`VAR__FILE` form, official `startup.sh` parity) — no
shell expansion happens in the container.

## Healthcheck

The image's `HEALTHCHECK` probes the real login page at the configured base
path (official-parity probe). Semantics: the app serves ⇒ healthy, even with
the database down (a page-serving liveness probe, like the official stack's).

## Publishing (planned, docs/19 P3)

`docker buildx build --platform linux/amd64,linux/arm64 -t <registry>/ttrss:latest --push .`
via the standard `setup-buildx` / `login` / `build-push-action` CI trio — no
context flags needed since the Dockerfile sits at the repo root.
