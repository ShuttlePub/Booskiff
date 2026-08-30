# Booskiff recipes

check:
    cargo check

test:
    cargo test

clippy:
    cargo clippy --all-targets -- -D warnings

fmt:
    cargo fmt

fmt-check:
    cargo fmt --check

openapi:
    cargo run -p core -- openapi > openapi.json

compose-up:
    docker compose up -d

compose-down:
    docker compose down -v

e2e:
    bash e2e/run-e2e.sh

verify: fmt-check check clippy test
