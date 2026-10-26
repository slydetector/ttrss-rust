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

`up` pulls the published image from GHCR (`ghcr.io/slydetector/ttrss-rust:latest`);
to run unpushed changes from this checkout, `docker compose up --build`.

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

## Try it with your existing data (`importdb`)

Run this port on a tt-rss install you already have — the import copies its
database into this deployment; nothing on the source side is touched:

```sh
docker compose exec app ttrss importdb          # guided prompts
docker compose exec app ttrss importdb          # re-run: remembered source, press Y
docker compose exec -T app ttrss importdb --yes # scriptable (no TTY)
docker compose exec app ttrss importdb --forget # clear the remembered source
```

The source credentials are remembered in `TTRSS_DATA_DIR/importdb-source`
(mode 0600) after a successful run — that is what makes re-running one
keypress. `IMPORTDB_HOST/PORT/DB/USER` + `IMPORTDB_PASS` (or `PGPASSWORD`)
env vars win over the remembered file (and enable TTY-less runs).

The import streams `pg_dump | pg_restore` into this deployment's database and
is idempotent — a Ctrl-C or a failed run is fine to simply re-run. When it
finishes: `docker compose restart app`.

## Healthcheck

The image's `HEALTHCHECK` probes the real login page at the configured base
path (official-parity probe). Semantics: the app serves ⇒ healthy, even with
the database down (a page-serving liveness probe, like the official stack's).

## Publishing

Every green `main` push publishes `ghcr.io/slydetector/ttrss-rust:latest`
(`.github/workflows/build.yml`, image job: buildx build → e2e →
`make image-push REGISTRY=ghcr.io/slydetector`). The repo is public, so the
image is public (repository-scoped packages inherit visibility). Manual push
of a locally built image: `make image-push REGISTRY=ghcr.io/slydetector`.
