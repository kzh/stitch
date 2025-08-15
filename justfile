# Stitch Workspace Justfile

# List available commands
default:
    @just --list

# Build the server (release)
build-server:
    cargo build --package stitch-server --release

# Build the client (release)
build-client:
    cargo build --package stitch --release

# Install the client with cargo install
install-client:
    cargo install --path client

# Run the server (debug)
run-server:
    RUST_BACKTRACE=1 cargo run --package stitch-server

# Run the client (debug)
run-client:
    RUST_BACKTRACE=1 cargo run --package stitch


# Build Docker image
build-docker:
    docker buildx build --platform linux/amd64 -f server/Dockerfile -t stitch-server .

build-docker-push:
    docker buildx build --platform linux/amd64 -f server/Dockerfile --tag ghcr.io/kzh/stitch-server:latest --tag ghcr.io/kzh/stitch-server:$(git rev-parse --short HEAD) --push .

# Run tests for all workspace crates
test:
    cargo test --workspace

# Format the code and lint with Clippy for all workspace crates
check:
    cargo fmt --all
    cargo clippy --workspace --all-features -- -D warnings

# Regenerate protobuf definitions
protoc:
    cd proto && cargo build

# Run sqlx migrations
migrate:
    sqlx migrate run --source server/migrations
