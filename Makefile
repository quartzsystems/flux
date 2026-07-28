# Flux — build, test, and development targets.
#
# `make dev` is the one to know: it starts fluxd in fully-mocked mode and the
# Next.js dev server side by side, so the whole product is exercisable without
# DPDK, a NIC, or root.

SHELL := /bin/bash
.DEFAULT_GOAL := help

CARGO ?= cargo
NPM   ?= npm
WEB   := web

# Loaded by the dev targets. Copy .env.example to .env and edit DATABASE_URL.
-include .env
export

.PHONY: help
help: ## Show this help
	@grep -hE '^[a-zA-Z0-9_-]+:.*?## ' $(MAKEFILE_LIST) \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-18s\033[0m %s\n", $$1, $$2}'

# ---------------------------------------------------------------------------
# Rust
# ---------------------------------------------------------------------------

.PHONY: build
build: ## Build the workspace in release mode
	$(CARGO) build --workspace --release

.PHONY: check
check: ## Type-check the workspace
	$(CARGO) check --workspace --all-targets

.PHONY: lint
lint: ## Clippy with warnings denied — the quality bar for merging
	$(CARGO) clippy --workspace --all-targets -- -D warnings

.PHONY: fmt
fmt: ## Format all Rust sources
	$(CARGO) fmt --all

.PHONY: fmt-check
fmt-check: ## Verify formatting without changing anything
	$(CARGO) fmt --all -- --check

.PHONY: test
test: ## Run the Rust test suite
	$(CARGO) test --workspace

# ---------------------------------------------------------------------------
# Web
# ---------------------------------------------------------------------------

.PHONY: web-install
web-install: ## Install web dependencies
	cd $(WEB) && $(NPM) install

.PHONY: web-build
web-build: ## Produce the static export in web/out
	cd $(WEB) && $(NPM) run build

.PHONY: web-lint
web-lint: ## Lint and type-check the web app
	cd $(WEB) && $(NPM) run lint && $(NPM) run typecheck

.PHONY: web-dev
web-dev: ## Run the Next.js dev server (proxies /api to fluxd)
	cd $(WEB) && $(NPM) run dev

# ---------------------------------------------------------------------------
# Database
# ---------------------------------------------------------------------------

.PHONY: db-setup
db-setup: ## Create the flux role and database (needs a superuser connection)
	@echo "Creating the flux role and database using $$PGSUPERUSER_URL"
	@psql "$$PGSUPERUSER_URL" -v ON_ERROR_STOP=1 -f deploy/sql/bootstrap.sql

.PHONY: db-reset
db-reset: ## Drop and recreate the flux database — destroys all data
	@read -p "This deletes every run, result, and account. Type 'yes' to continue: " ok; \
	 [ "$$ok" = "yes" ] || { echo "aborted"; exit 1; }
	@psql "$$PGSUPERUSER_URL" -v ON_ERROR_STOP=1 \
		-c "DROP DATABASE IF EXISTS flux;" -c "CREATE DATABASE flux OWNER flux;"

# ---------------------------------------------------------------------------
# Development
# ---------------------------------------------------------------------------

.PHONY: dev
dev: ## Run fluxd (mocked) and the Next.js dev server together
	@command -v $(NPM) >/dev/null || { echo "npm is required for 'make dev'"; exit 1; }
	@echo "fluxd  → http://127.0.0.1:8080  (API)"
	@echo "web    → http://127.0.0.1:3000  (UI, proxies /api to fluxd)"
	@trap 'kill 0' EXIT INT TERM; \
	 FLUX_ENGINE=mock FLUX_PORTD=mock FLUX_BIND=127.0.0.1:8080 \
	   $(CARGO) run -p fluxd & \
	 cd $(WEB) && $(NPM) run dev & \
	 wait

.PHONY: dev-api
dev-api: ## Run only fluxd, fully mocked
	FLUX_ENGINE=mock FLUX_PORTD=mock FLUX_BIND=127.0.0.1:8080 $(CARGO) run -p fluxd

.PHONY: serve
serve: web-build ## Build the UI and serve everything from fluxd alone
	FLUX_ENGINE=mock FLUX_PORTD=mock FLUX_BIND=127.0.0.1:8080 \
	FLUX_WEB_ROOT=$(CURDIR)/$(WEB)/out $(CARGO) run -p fluxd

.PHONY: ci
ci: fmt-check lint test web-lint web-build ## Everything CI runs

.PHONY: clean
clean: ## Remove build output
	$(CARGO) clean
	rm -rf $(WEB)/.next $(WEB)/out
