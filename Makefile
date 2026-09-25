# ttrss — Makefile for the oracle + record/replay harness workflow.
#
# This is the primary interface. `make` (or `make help`) lists every target;
# the targets launch the `oracle/scripts/` helpers and set the canonical env
# for the Rust server. Pass extra options with `ARGS=...`, e.g.
#   make harness ARGS=--no-build

.DEFAULT_GOAL := help
SHELL := /bin/bash
SCRIPTS     := oracle/scripts
HARNESS_BIN := ./target/debug/harness
# Server build profile for `run`. A debug build is ~7x slower per request than
# the optimized one (and the PHP oracle is compiled/OPcache'd), so the
# canonical server runs in release; set PROFILE=debug for fast iteration.
PROFILE     ?= release
CARGO_PROF  := $(if $(filter release,$(PROFILE)),--release,)
SERVER_BIN  := ./target/$(PROFILE)/ttrss

# Canonical Rust server values (the oracle web is on 8280; the Rust server on
# 8281 so the diff can compare them as the same app on different ports).
RUST_PORT := 8281
BASE_URL  := http://127.0.0.1:$(RUST_PORT)/tt-rss/
ORACLE_URL := http://127.0.0.1:8280/tt-rss/
SCENARIOS := oracle/corpus/scenarios.template.toml
VARS      := oracle/data/ids.json

# DB creds / plugins / the encryption key come from oracle/.env (source of truth).
DB_USER := $(shell grep -E '^TTRSS_DB_USER=' oracle/.env 2>/dev/null | cut -d= -f2-)
DB_NAME := $(shell grep -E '^TTRSS_DB_NAME=' oracle/.env 2>/dev/null | cut -d= -f2-)
DB_PASS := $(shell grep -E '^TTRSS_DB_PASS=' oracle/.env 2>/dev/null | cut -d= -f2-)
PLUGINS := $(shell grep -E '^TTRSS_PLUGINS=' oracle/.env 2>/dev/null | cut -d= -f2-)
ENC_KEY := $(shell grep -E '^TTRSS_ENCRYPTION_KEY=' oracle/.env 2>/dev/null | cut -d= -f2-)

# --- demo (LAN poking) ------------------------------------------------------
DEMO_PID  := demo.pid
DEMO_LOG  := demo.log
# The LAN IP for the self-URL (auto-detected from the default route; override
# with `make demo-start LAN_IP=<ip>`).
LAN_IP    ?= $(shell ip route get 1 2>/dev/null | awk '{for (i = 1; i < NF; i++) if ($$i == "src") print $$(i+1)}')
# The demo server's log level (the daemon loop's per-feed lines are debug;
# scoped to the ttrss crate so sqlx's per-query debug noise stays off).
RUST_LOG  ?= ttrss=debug

.PHONY: help
help: ## (default) list targets
	@echo "ttrss — oracle + record/replay harness workflow"
	@echo "Run a target with:  make <target>   (extra opts via ARGS=...)"
	@echo ""
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-20s\033[0m %s\n", $$1, $$2}'
	@echo ""

# --- oracle stack (live PHP tt-rss) ----------------------------------------
.PHONY: oracle-up
oracle-up: ## bring up the PHP oracle (db + app + updater + nginx)
	$(SCRIPTS)/up.sh

.PHONY: oracle-down
oracle-down: ## tear down the oracle stack (keeps the db volume)
	$(SCRIPTS)/down.sh

.PHONY: oracle-seed
oracle-seed: ## seed the golden dataset from scratch (fresh stack, ~3 min)
	python3 $(SCRIPTS)/seed.py

.PHONY: oracle-snapshot
oracle-snapshot: ## snapshot the current oracle state -> oracle/data/
	$(SCRIPTS)/snapshot.sh

.PHONY: oracle-bless
oracle-bless: ## re-baseline the manifest lineage to the current files (no re-seed)
	python3 $(SCRIPTS)/bless.py

.PHONY: oracle-restore
oracle-restore: ## restore golden-db.sql into a fresh db container
	$(SCRIPTS)/restore.sh

.PHONY: oracle-reset
oracle-reset: ## reset the running oracle DB to the golden baseline
	$(SCRIPTS)/reset-db.sh

.PHONY: oracle-check
oracle-check: ## verify the golden dataset matches the current seed recipe
	$(SCRIPTS)/check_sync.py

# --- Rust server -------------------------------------------------------------
.PHONY: build
build: ## build the debug Rust server + harness (for tests / the differential harness)
	cargo build -p ttrss --bin ttrss -p harness

.PHONY: build-server
build-server: ## build the server in $(PROFILE) (used by `run`)
	cargo build $(CARGO_PROF) -p ttrss --bin ttrss

.PHONY: run
run: build-server ## run the (release by default) Rust server (canonical env, port 8281)
	TTRSS_DB_HOST=127.0.0.1 TTRSS_DB_PORT=5432 \
	TTRSS_DB_USER=$(DB_USER) TTRSS_DB_NAME=$(DB_NAME) TTRSS_DB_PASS=$(DB_PASS) \
	TTRSS_PLUGINS=$(PLUGINS) TTRSS_ENCRYPTION_KEY=$(ENC_KEY) \
	TTRSS_SELF_URL_PATH=$(BASE_URL) TTRSS_LISTEN_PORT=127.0.0.1:$(RUST_PORT) \
	TTRSS_DOCROOT=$(CURDIR)/tt-rss TTRSS_CACHE_DIR=$(CURDIR)/oracle/data \
$(SERVER_BIN)

# --- demo (LAN poking) ------------------------------------------------------
# Bring up ONLY the oracle Postgres + the (release) Rust server on 0.0.0.0,
# so the app is reachable from the LAN. The PHP stack is stopped (if running)
# so only Rust serves the shared golden DB. demo-start never resets the DB —
# it starts against the DB in whatever state it is in; use demo-reset for a
# fresh golden baseline.
.PHONY: demo-db
demo-db: ## (internal) bring up the oracle db + wait for healthy
	@( cd oracle && docker compose up -d db )
	@for i in $$(seq 1 60); do \
		s=$$(docker inspect --format '{{.State.Health.Status}}' ttrss-oracle-db-1 2>/dev/null); \
		[ "$$s" = "healthy" ] && break; sleep 1; \
	done
	@docker inspect --format '{{.State.Health.Status}}' ttrss-oracle-db-1 2>/dev/null | grep -q healthy \
		|| { echo "error: oracle db did not become healthy"; exit 1; }

.PHONY: demo-reset
demo-reset: demo-db ## reset the oracle DB to the golden baseline (no server management)
	$(SCRIPTS)/reset-db.sh

.PHONY: demo-start
demo-start: build-server demo-db ## start the (release) Rust server on 0.0.0.0:8281 against the oracle DB, in its current state (LAN)
	@test -n "$(LAN_IP)" || { echo "error: could not detect a LAN IP; set one explicitly: make demo-start LAN_IP=<ip>"; exit 1; }
	@echo "stopping the PHP stack (app/updater/web), if running..."
	@( cd oracle && docker compose stop app updater web >/dev/null 2>&1 ) || true
	@if [ -f $(DEMO_PID) ] && kill -0 "$$(cat $(DEMO_PID))" 2>/dev/null; then \
		echo "stopping previous demo instance (pid $$(cat $(DEMO_PID)))..."; \
		kill "$$(cat $(DEMO_PID))" 2>/dev/null || true; sleep 1; \
	fi
	@echo "starting $(SERVER_BIN) on 0.0.0.0:$(RUST_PORT) (self url http://$(LAN_IP):$(RUST_PORT)/tt-rss/)"
	@TTRSS_DB_HOST=127.0.0.1 TTRSS_DB_PORT=5432 \
	TTRSS_DB_USER=$(DB_USER) TTRSS_DB_NAME=$(DB_NAME) TTRSS_DB_PASS=$(DB_PASS) \
	TTRSS_PLUGINS=$(PLUGINS) TTRSS_ENCRYPTION_KEY=$(ENC_KEY) \
	TTRSS_SELF_URL_PATH=http://$(LAN_IP):$(RUST_PORT)/tt-rss/ TTRSS_LISTEN_PORT=0.0.0.0:$(RUST_PORT) \
	TTRSS_DOCROOT=$(CURDIR)/tt-rss TTRSS_CACHE_DIR=$(CURDIR)/oracle/data \
	RUST_LOG=$(RUST_LOG) nohup $(SERVER_BIN) > $(DEMO_LOG) 2>&1 & echo $$! > $(DEMO_PID)
	@for i in $$(seq 1 30); do \
		if resp=$$(curl -sf http://127.0.0.1:$(RUST_PORT)/tt-rss/healthz 2>/dev/null) && [ "$$resp" = "OK" ]; then break; fi; \
		sleep 1; \
	done
	@if curl -sf http://127.0.0.1:$(RUST_PORT)/tt-rss/healthz >/dev/null 2>&1; then \
		echo ""; \
		echo "ttrss (Rust, release) is serving: http://$(LAN_IP):$(RUST_PORT)/tt-rss/"; \
		echo "  pid $$(cat $(DEMO_PID))  |  log: $(DEMO_LOG)  |  stop: make demo-stop  |  fresh baseline: make demo-reset"; \
		echo "  logins: admin/admin  |  otpuser/otppass (TOTP)  |  readonly/readpass  |  testuser/testpass"; \
	else \
		echo "error: server did not answer healthz on :$(RUST_PORT) — check $(DEMO_LOG)" >&2; exit 1; \
	fi

.PHONY: demo-stop
demo-stop: ## stop the server started by `make demo-start` (the db container stays up)
	@if [ ! -f $(DEMO_PID) ]; then echo "no $(DEMO_PID); nothing to stop"; exit 0; fi
	@echo "stopping demo instance (pid $$(cat $(DEMO_PID)))"
	@kill "$$(cat $(DEMO_PID))" 2>/dev/null || true
	@rm -f $(DEMO_PID)

.PHONY: demo-follow
demo-follow: ## follow the demo server's log output (tail -f)
	@test -f $(DEMO_LOG) || { echo "no $(DEMO_LOG); start the server first: make demo-start"; exit 1; }
	@tail -n 50 -f $(DEMO_LOG)

# --- shadow (prod-data confidence run) ---------------------------------------
# Everything lives under shadow/ (see docs/shadow-prod.md). Prod creds go in the
# gitignored shadow/.env (create from shadow/.env.example). The drift preflight
# hard-fails unless prod's ttrss_version.schema_version == EXPECTED_SCHEMA_VERSION.
SHADOW_SCRIPT := shadow/shadow.sh

.PHONY: shadow
shadow: ## preflight + build + bring up the shadow (db + rust) against a prod copy
	bash $(SHADOW_SCRIPT) preflight
	bash $(SHADOW_SCRIPT) up

.PHONY: shadow-copy
shadow-copy: ## re-sync the shadow from prod (re-dump + restore + restart; no rebuild)
	bash $(SHADOW_SCRIPT) copy

.PHONY: shadow-follow
shadow-follow: ## follow all shadow service logs (Ctrl-C to stop)
	(cd shadow && docker compose logs -f)
.PHONY: shadow-stop
shadow-stop: ## stop the shadow stack (db + rust); keeps the data volume
	bash $(SHADOW_SCRIPT) stop

.PHONY: shadow-db
shadow-db: ## (internal) ensure the shadow db is up + reset the shadow database
	bash $(SHADOW_SCRIPT) db

.PHONY: test
test: ## run the full workspace test suite (cargo nextest)
	cargo nextest run

.PHONY: clippy
clippy: ## run clippy across the workspace
	cargo clippy --workspace --all-targets

.PHONY: coverage
coverage: ## run test coverage (cargo-llvm-cov): CLI summary + HTML report
	@command -v cargo-llvm-cov >/dev/null 2>&1 || { echo "error: cargo-llvm-cov not found. Install:  cargo install cargo-llvm-cov"; exit 1; }
	@# cargo-llvm-cov needs llvm-cov + llvm-profdata (the llvm-tools-preview component, a system LLVM on PATH, or LLVM_COV/LLVM_PROFDATA).
	@cargo llvm-cov show-env --sh >/dev/null 2>&1 || { \
		echo "error: cargo-llvm-cov could not find llvm-cov/llvm-profdata."; \
		echo "  fix (pick one):"; \
		echo "    rustup component add llvm-tools-preview"; \
		echo "    export LLVM_COV=/path/to/llvm-cov LLVM_PROFDATA=/path/to/llvm-profdata"; \
		exit 1; }
	@echo "== running the ttrss test suite under LLVM source-based coverage instrumentation (HTML report follows)..."
	cargo llvm-cov -p ttrss --html
	@echo ""
	@echo "== HTML report: target/llvm-cov/html/index.html"
	@echo "== freeing the instrumented build dir (target/llvm-cov-target, ~18 GB) ..."
	@rm -rf target/llvm-cov-target
	@echo "== done - the HTML report above is kept; the extra build dir is removed."

# --- harness (record / replay / diff) ---------------------------------------
.PHONY: harness
harness: ## reset DB, build, run the corpus vs the Rust server, diff vs oracle-a
	$(SCRIPTS)/harness.sh $(ARGS)

.PHONY: harness-keep
harness-keep: ## like `harness` but leave the Rust server running
	$(SCRIPTS)/harness.sh --keep $(ARGS)

.PHONY: record
record: ## record a fresh oracle baseline -> oracle/data/runs/oracle-a
	@echo "stopping the updater (static oracle) + resetting the DB to the golden baseline ..."
	@( cd oracle && docker compose stop updater >/dev/null 2>&1 ) || true
	oracle/scripts/reset-db.sh
	$(SCRIPTS)/check_sync.py
	$(HARNESS_BIN) run --base-url $(ORACLE_URL) \
		--scenarios $(SCENARIOS) --vars $(VARS) --out oracle/data/runs/oracle-a

.PHONY: diff
diff: ## diff two run dirs (A/B via ARGS=--a <dir> --b <dir>)
	$(HARNESS_BIN) diff $(ARGS)

# --- differential param fixtures (recorded from the frozen PHP reference) ---
# These drive the *real* PHP helpers (the frozen tt-rss reference) over a
# front-end-shaped input battery and record the ground truth as a fixture the
# Rust tests compare against. They run in the *public* PHP image matching the
# app's PHP 8.5 — no oracle stack required. See oracle/recorders/.
.PHONY: record-params
record-params: ## re-record the param differential fixtures from frozen PHP (public php:8.5-cli)
	$(SCRIPTS)/record_params.sh
