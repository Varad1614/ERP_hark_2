# SutraERP developer entry points. `make help` lists targets.
SHELL := /bin/bash
CARGO ?= cargo
PNPM ?= pnpm

.PHONY: help setup up down api web migrate openapi check fmt fmt-check lint test test-db build web-check ci clean

help: ## Show targets
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'

setup: ## Install JS deps and copy .env
	@test -f .env || cp .env.example .env
	$(PNPM) install

up: ## Start local infra (postgres 18, keycloak 26, valkey, mailpit)
	docker compose up -d

down: ## Stop local infra
	docker compose down

migrate: ## Apply database migrations
	$(CARGO) run -p sutra-api -- migrate

api: ## Run the API (http://localhost:8080)
	$(CARGO) run -p sutra-api -- serve

web: ## Run the web app (http://localhost:5173)
	$(PNPM) --filter @sutra/web dev

openapi: ## Regenerate openapi/openapi.json and the typed TS client
	$(CARGO) run -q -p sutra-api -- openapi > openapi/openapi.json
	$(PNPM) gen:api

fmt: ## Format Rust
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all -- --check

lint: ## clippy -D warnings + eslint
	$(CARGO) clippy --workspace --all-targets --all-features -- -D warnings
	$(PNPM) lint

test: ## Rust + web unit tests (no database needed)
	$(CARGO) test --workspace
	$(PNPM) -r test

test-db: ## Integration tests against a live PostgreSQL (DATABASE_URL)
	$(CARGO) test -p sutra-persistence -p sutra-platform -p sutra-api --features db-tests

build: ## Build API and web
	$(CARGO) build --workspace
	$(PNPM) -r build

web-check: ## typecheck + lint + test + build the web workspace
	$(PNPM) -r typecheck && $(PNPM) lint && $(PNPM) -r test && $(PNPM) -r build

ci: fmt-check lint test build ## Everything CI runs (plus OpenAPI drift)
	$(CARGO) run -q -p sutra-api -- openapi | diff -u openapi/openapi.json -
	$(PNPM) gen:api && git diff --exit-code packages/api-client/src/schema.d.ts

clean:
	$(CARGO) clean
	rm -rf node_modules apps/*/node_modules packages/*/node_modules apps/web/build apps/web/.react-router
