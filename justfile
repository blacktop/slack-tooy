# List available commands.
default:
    @just --list

# Build, test, and sign the macOS release and image decoder.
build:
    sh scripts/build-macos.sh

# Build the signed macOS release and run it with optional CLI arguments.
[positional-arguments]
run *args: build
    @exec ./target/release/slack-tooy "$@"

# Build and install both signed executables together.
install: build
    install -d "$HOME/.cargo/bin"
    install -m 755 target/release/slack-tooy target/release/slack-tooy-image-decoder "$HOME/.cargo/bin/"

# Check Rust formatting, lints, and tests.
check:
    cargo fmt --check
    cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
    cargo test --workspace --all-features --locked -- --quiet
