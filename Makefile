SHELL := /bin/bash

.PHONY: up down logs demo smoke test fmt clippy bench-up bench bench-down clean

up:
	docker compose up -d --build

down:
	docker compose down

logs:
	docker compose logs -f rusto2-gw rusto2-is

demo:
	bash scripts/demo.sh

smoke:
	bash scripts/smoke.sh

test:
	cargo test --workspace --all-targets

fmt:
	cargo fmt --all

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

bench-up:
	docker compose -f docker-compose.yml -f docker-compose.bench.yml up -d --build

bench:
	bash scripts/bench.sh

bench-down:
	docker compose down

clean:
	cargo clean
