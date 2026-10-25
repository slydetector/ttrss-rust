<p align="center">
  <img src="ttrss-rust.png" alt="ttrss-rust" width="200">
</p>

# ttrss-rust

A port of the [tt-rss](https://github.com/tt-rss/tt-rss) (Tiny Tiny RSS)
server backend from PHP to Rust. The front-end is untouched.

This is mostly a drop-in replacement for the tt-rss backend. A four container PHP deployment (app + updater + web-nginx + db) reduces down to two containers (rust app + db) now. 

Performance-wise, it seems to be the same speed if not a little bit snappier. Most of the heavylifting is done by the database which is unchanged, so that pretty much tracks. I imagine it takes less resources (CPU + RAM) too, but haven't done any measurements.

## Quick start

```shell
git clone https://github.com/slydetector/ttrss-rust
cd ttrss-rust/docker

# Configure by customizing .env
cp .env.example .env  

# Start up ttrss-rust
docker compose up -d

# Option A: 
#   Import your existing postgres db into this instance. This allows you to 
#   take ttrss-rust for a quick spin with a copy of your existing setup. Your
#   postgres host:port must be reachable and you'll be prompted for your db info.
docker compose exec app ttrss importdb

#   Restart the app so it picks up the imported data.
docker compose restart app

# Option B: 
#   Import your feeds via opml from your existing php tt-rss instance. 
#   1. Grab the admin password from the logs
#   2. Browse to http://localhost:9000/tt-rss/ , login as admin and do the OPML import.
docker compose logs app

# Option C: 
#   Start with a blank slate. 
#   1. Grab the admin password from the logs
#   2. Browse to http://localhost:9000/tt-rss/ and you're good to login as admin.
docker compose logs app
```

## Caveats

### Plugins
- All bundled first-party PHP plugins have been ported to rust.
- 3rd party PHP plugins are incompatible and require porting to rust.
### Configuration
- The vast majority of configuration via env vars works out of the box.
- Configuration via php obviously doesn't work.
### CLI
- I don't use the CLI so I didn't port it.

## Development

Run `make bootstrap` first (initializes the `tt-rss` reference submodule).

The top-level **`Makefile`** is the primary interface — `make help` lists every target. 
The most common targets:

```sh
make build          # build the Rust server + harness
make run            # run the Rust server in the foreground (port 9000)
make test           # run all the unit and integration tests
make e2e            # Run e2e Playwright tests
make php-sync       # Identifies changes to upstream php tt-rss that require porting.
```

### Layout

| Path | Contents |
|---|---|
| `Makefile` | Start here to see useful targets |
| `crates/ttrss` | Rust port |
| `docker/` | Docker compose stack |
| `docs/` | Planning & research docs for the Rust port |
| `e2e/` | Playwright based e2e acceptance tests |
| `tt-rss/` | Upstream PHP checkout (frozen reference, own git repo) |

## Backstory
This started out as an experiment to see if a fully self-hosted LLM on consumer grade hardware was capable of porting a large complex PHP codebase to Rust. 

The answer is a resounding yes. However, it did require a significant amount of steering and architectural experience. Keep in mind, I have zero professional experience with PHP or Rust.

Regardless, ttrss-rust is now my production daily driver, so I plan to keep it in sync with the PHP implementation.

### AI Setup
- LLM: Qwen 3.8 27b 
- GPU: Nvidia RTX 3090 with 24GB VRAM
- Harness: [pi.dev](https://pi.dev) with plugins: pi-ketch, pi-vcc, pi-token-speed, pi-turn-timer
- LLM runner: Started out with [club-3090](https://github.com/noonghunna/club-3090) then migrated to [llamAmpere](https://github.com/JakeATX/llamAmpere) to max out token decode speed (30-70 tps)

## Credits
Much gratitude to the original authors, contributors, and maintainers of tt-rss.