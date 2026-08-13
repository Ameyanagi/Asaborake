# Asaborake developer entry points.
#
# `make check` is what the pre-commit hook and CI both run; keep them aligned.

CARGO ?= cargo
BUN ?= bun

.DEFAULT_GOAL := help

.PHONY: help
help: ## Show this help
	@grep -hE '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2}'

.PHONY: hooks
hooks: ## Enable the repository's git hooks
	git config core.hooksPath .githooks
	@chmod +x .githooks/*
	@echo "git hooks enabled (core.hooksPath=.githooks)"

.PHONY: reference
reference: ## Clone the upstream projects Asaborake is modelled on (not redistributed)
	@mkdir -p reference
	@test -d reference/Amatsukaze || git clone --depth 1 --no-recurse-submodules \
		https://github.com/nekopanda/Amatsukaze.git reference/Amatsukaze
	@test -d reference/join_logo_scp || git clone --depth 1 \
		https://github.com/nekopanda/join_logo_scp.git reference/join_logo_scp

.PHONY: fmt
fmt: ## Format Rust sources
	$(CARGO) fmt --all

.PHONY: lint
lint: ## Run clippy across the workspace
	$(CARGO) clippy --workspace --all-targets --all-features -- -D warnings

.PHONY: test
test: ## Run the Rust test suite
	$(CARGO) test --workspace --all-features

.PHONY: check
check: ## Everything CI runs for Rust
	$(CARGO) fmt --all --check
	$(MAKE) lint
	$(MAKE) test

.PHONY: build
build: ## Build the release binary
	$(CARGO) build --release --bin asaborake

.PHONY: web-install
web-install: ## Install web dependencies
	cd web && $(BUN) install

.PHONY: web-check
web-check: ## Typecheck and lint the web app
	cd web && $(BUN) run typecheck && $(BUN) run lint

.PHONY: web-dev
web-dev: ## Run the web app against a local engine
	cd web && $(BUN) run dev
