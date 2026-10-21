set shell := ["bash", "-euo", "pipefail", "-c"]
set positional-arguments

# List available commands.
default:
    @just --list

# Build, test, and sign the macOS release and image decoder.
build: decoder
    env MACOSX_DEPLOYMENT_TARGET=26.0 cargo build --release --locked
    just sign-binary target/release/slack-tooy
    target/release/slack-tooy --version

# Build and verify the isolated native decoder.
[private]
decoder:
    xcodegen generate --spec macos/project.yml
    xcodebuild -quiet -project macos/SlackTooyMedia.xcodeproj -scheme DecoderChecks -configuration Release -derivedDataPath target/macos-media CODE_SIGN_IDENTITY=- build
    codesign --verify --strict target/macos-media/Build/Products/Release/slack-tooy-image-decoder
    xcrun xctest target/macos-media/Build/Products/Release/ImageDecoderTests.xctest
    python3 scripts/verify-macos-decoder.py target/macos-media/Build/Products/Release/slack-tooy-image-decoder target/macos-media/Build/Products/Release/slack-tooy-sandbox-probe
    mkdir -p target/release
    cp target/macos-media/Build/Products/Release/slack-tooy-image-decoder target/release/.slack-tooy-image-decoder.new
    mv -f target/release/.slack-tooy-image-decoder.new target/release/slack-tooy-image-decoder
    cargo test --workspace --all-features --locked native_decoder_round_trip -- --ignored --quiet
    codesign --verify --strict target/release/slack-tooy-image-decoder

# Sign the Rust executable after its final link or copy.
[private]
sign-binary path:
    codesign --force --sign - --options runtime --timestamp=none --identifier io.blacktop.slack-tooy --entitlements packaging/macos/slack-tooy.entitlements "$1"
    codesign --verify --strict "$1"

# Build the signed macOS release and run it with optional CLI arguments.
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

# Validate the release workflow and GoReleaser configuration.
release-check:
    just --fmt --check
    actionlint .github/workflows/release.yml
    goreleaser check

# Build local macOS release archives without publishing them.
package:
    goreleaser release --snapshot --clean

# Publish the checked-out tag; called by GitHub Actions.
[private]
release-publish:
    goreleaser release --clean

[private]
check-version version:
    #!/usr/bin/env bash
    set -euo pipefail
    st_tag="$1"
    if [[ ! "$st_tag" =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
        printf 'Invalid release tag %q; expected vMAJOR.MINOR.PATCH\n' "$st_tag" >&2
        exit 1
    fi
    st_version=$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["package"]["version"])')
    if [[ "${st_tag#v}" != "$st_version" ]]; then
        printf 'Tag %s does not match Cargo.toml version %s\n' "$st_tag" "$st_version" >&2
        exit 1
    fi

# Enforce release provenance in GitHub Actions, independently of the tag command.
[private]
verify-release-tag version:
    #!/usr/bin/env bash
    set -euo pipefail
    st_tag="$1"
    just check-version "$st_tag"
    st_ref="refs/tags/$st_tag"
    if [[ "$(git cat-file -t "$st_ref")" != tag ]]; then
        printf 'Release tag %s must be annotated and signed\n' "$st_tag" >&2
        exit 1
    fi
    st_object=$(git rev-parse --verify "$st_ref")
    st_commit=$(git rev-parse --verify "$st_ref^{commit}")
    if [[ "$st_commit" != "$(git rev-parse --verify 'HEAD^{commit}')" ]]; then
        printf 'Release checkout does not match tag %s\n' "$st_tag" >&2
        exit 1
    fi
    st_verified=$(gh api "repos/blacktop/slack-tooy/git/tags/$st_object" --jq '.verification.verified')
    if [[ "$st_verified" != true ]]; then
        printf 'GitHub has not verified the signature on tag %s\n' "$st_tag" >&2
        exit 1
    fi
    git fetch --no-tags origin '+refs/heads/main:refs/remotes/origin/main'
    if ! git merge-base --is-ancestor "$st_commit" refs/remotes/origin/main; then
        printf 'Release tag %s is not in origin/main history\n' "$st_tag" >&2
        exit 1
    fi

# Check, sign, and push a stable release tag, for example: just release v0.1.0.
release version:
    #!/usr/bin/env bash
    set -euo pipefail
    st_tag="$1"
    just check-version "$st_tag"
    st_head=$(git rev-parse --verify 'HEAD^{commit}')
    st_ref="refs/tags/$st_tag"
    st_require_main() {
        local st_branch st_current st_status st_remote
        st_branch=$(git branch --show-current)
        st_current=$(git rev-parse --verify 'HEAD^{commit}')
        st_status=$(git status --porcelain)
        if [[ "$st_branch" != main || "$st_current" != "$st_head" || -n "$st_status" ]]; then
            printf 'Release requires a clean main checkout at %s\n' "$st_head" >&2
            exit 1
        fi
        st_remote=$(git ls-remote --exit-code origin refs/heads/main)
        if [[ "$st_remote" != "$st_head"$'\trefs/heads/main' ]]; then
            printf 'Push main before releasing; origin/main must match %s\n' "$st_head" >&2
            exit 1
        fi
    }
    st_require_main
    if git show-ref --verify --quiet "$st_ref"; then
        printf 'Tag %s already exists locally; inspect it before retrying a push\n' "$st_tag" >&2
        exit 1
    fi
    st_remote_tag=$(git ls-remote --tags origin "$st_ref")
    if [[ -n "$st_remote_tag" ]]; then
        printf 'Tag %s already exists on origin\n' "$st_tag" >&2
        exit 1
    fi
    just check
    just build
    just release-check
    st_require_main
    git tag -s "$st_tag" "$st_head" -m "Release $st_tag"
    if ! git push --no-follow-tags origin "$st_ref"; then
        printf 'Tag %s remains local. After resolving the push failure, retry: git push --no-follow-tags origin %s\n' "$st_tag" "$st_ref" >&2
        exit 1
    fi
    printf 'Pushed %s; GitHub Actions will publish the release and Homebrew cask.\n' "$st_tag"
