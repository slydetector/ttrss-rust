# tt-rust — Makefile for the oracle + record/replay harness workflow.
#
# This is the primary interface. `make` (or `make help`) lists every target;
# the targets launch the `oracle/scripts/` helpers and set the canonical env
# for the Rust server. Pass extra options with `ARGS=...`, e.g.
#   make harness ARGS=--no-build

.DEFAULT_GOAL := help
SHELL := /bin/bash
SCRIPTS     := oracle/scripts
HARNESS_BIN := ./target/debug/tt-rust-harness
# Server build profile for `run`. A debug build is ~7x slower per request than
# the optimized one (and the PHP oracle is compiled/OPcache'd), so the
# canonical server runs in release; set PROFILE=debug for fast iteration.
PROFILE     ?= release
CARGO_PROF  := $(if $(filter release,$(PROFILE)),--release,)
SERVER_BIN  := ./target/$(PROFILE)/tt-rust

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

.PHONY: help
help: ## (default) list targets
	@echo "tt-rust — oracle + record/replay harness workflow"
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
	cargo build -p ttrss-server --bin tt-rust -p tt-rust-harness

.PHONY: build-server
build-server: ## build the server in $(PROFILE) (used by `run`)
	cargo build $(CARGO_PROF) -p ttrss-server --bin tt-rust

.PHONY: run
run: build-server ## run the (release by default) Rust server (canonical env, port 8281)
	TTRSS_DB_HOST=127.0.0.1 TTRSS_DB_PORT=5432 \
	TTRSS_DB_USER=$(DB_USER) TTRSS_DB_NAME=$(DB_NAME) TTRSS_DB_PASS=$(DB_PASS) \
	TTRSS_PLUGINS=$(PLUGINS) TTRSS_ENCRYPTION_KEY=$(ENC_KEY) \
	TTRSS_SELF_URL_PATH=$(BASE_URL) TTRSS_RUST_LISTEN=127.0.0.1:$(RUST_PORT) \
	TTRSS_RUST_DOCROOT=$(CURDIR)/tt-rss TTRSS_RUST_FEED_ICONS_DIR=$(CURDIR)/oracle/data/feed-icons \
	$(SERVER_BIN)

.PHONY: test
test: ## run the full workspace test suite (cargo nextest)
	cargo nextest run

.PHONY: clippy
clippy: ## run clippy across the workspace
	cargo clippy --workspace --all-targets

# --- harness (record / replay / diff) ---------------------------------------
.PHONY: harness
harness: ## reset DB, build, run the corpus vs the Rust server, diff vs oracle-a
	$(SCRIPTS)/harness.sh $(ARGS)

.PHONY: harness-keep
harness-keep: ## like `harness` but leave the Rust server running
	$(SCRIPTS)/harness.sh --keep $(ARGS)

.PHONY: record
record: ## record a fresh oracle baseline -> oracle/data/runs/oracle-a
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
