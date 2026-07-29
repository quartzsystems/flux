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

# The single source of truth for the version. Everything else derives from it:
# the binaries read it at build time, and `make version-sync` writes it into
# Cargo.toml and web/package.json.
VERSION := $(shell tr -d '[:space:]' < VERSION)

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
# Version and packaging
# ---------------------------------------------------------------------------

.PHONY: version
version: ## Print the version everything is built from
	@echo $(VERSION)

.PHONY: version-sync
version-sync: ## Write VERSION into Cargo.toml and web/package.json
	@scripts/sync-version.sh

.PHONY: version-check
version-check: ## Fail if the manifests disagree with VERSION
	@scripts/sync-version.sh --check

.PHONY: dist
dist: build web-build ## Build the release tarball into dist/
	@scripts/package.sh

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
ci: version-check exec-check fmt-check lint test web-lint web-build shell-lint install-check ## Everything CI runs

.PHONY: shell-lint
shell-lint: ## ShellCheck the installer and the helper scripts
	@if command -v shellcheck >/dev/null 2>&1; then 		shellcheck --severity=warning deploy/install.sh scripts/*.sh && echo "shellcheck: clean"; 	else 		echo "shellcheck: not installed, skipping (CI runs it regardless)"; 	fi

.PHONY: install-check
install-check: ## Exercise the installer's version and config logic
	@scripts/test-install.sh

# The scripts are invoked by path, so a lost executable bit is a CI failure
# rather than a warning. Easy to lose on Windows, where core.filemode is false.
.PHONY: exec-check
exec-check: ## Verify the scripts are executable in git's index
	@fail=0; for f in deploy/install.sh scripts/*.sh; do 		mode=$$(git ls-files -s "$$f" | cut -d' ' -f1); 		if [ "$$mode" != "100755" ]; then 			echo "$$f is $$mode in the index, expected 100755"; 			echo "  fix: git update-index --chmod=+x $$f"; 			fail=1; 		fi; 	done; 	[ $$fail -eq 0 ] && echo "exec bits: ok"; exit $$fail

.PHONY: clean
clean: ## Remove build output
	$(CARGO) clean
	rm -rf $(WEB)/.next $(WEB)/out dist
