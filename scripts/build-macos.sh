#!/bin/sh
set -eu

slack_tooy_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
cd "$slack_tooy_root"

xcodegen generate --spec macos/project.yml
xcodebuild -quiet -project macos/SlackTooyMedia.xcodeproj -scheme DecoderChecks \
    -configuration Release -derivedDataPath target/macos-media CODE_SIGN_IDENTITY=- build

slack_tooy_products="$slack_tooy_root/target/macos-media/Build/Products/Release"
codesign --verify --strict "$slack_tooy_products/slack-tooy-image-decoder"
xcrun xctest "$slack_tooy_products/ImageDecoderTests.xctest"
python3 scripts/verify-macos-decoder.py \
    "$slack_tooy_products/slack-tooy-image-decoder" \
    "$slack_tooy_products/slack-tooy-sandbox-probe"

mkdir -p target/release
cp "$slack_tooy_products/slack-tooy-image-decoder" target/release/.slack-tooy-image-decoder.new
mv -f target/release/.slack-tooy-image-decoder.new target/release/slack-tooy-image-decoder
cargo test --workspace --all-features --locked native_decoder_round_trip -- --ignored --quiet
cargo build --release --locked
codesign --force --sign - --options runtime --timestamp=none \
    --identifier io.blacktop.slack-tooy \
    --entitlements packaging/macos/slack-tooy.entitlements target/release/slack-tooy
codesign --verify --strict target/release/slack-tooy
codesign --verify --strict target/release/slack-tooy-image-decoder
target/release/slack-tooy --version
