# Development

Use macOS 26 or later, Xcode 26 or later, and Rust stable. Build commands live
in the justfile:

```fish
brew install just xcodegen
just check
just build
just run
```

`just build` runs the native ImageIO and sandbox checks, builds the release
executable, and verifies both ad-hoc signatures. Native checks need a normal
macOS terminal because Seatbelt profiles cannot be nested inside the agent
sandbox. `just install` installs the app and image decoder beside one another.

## Releases

The release workflow publishes ARM64 and Intel macOS archives, checksums, and
`Casks/slack-tooy.rb` in `blacktop/homebrew-tap`. Each archive contains both
signed executables. The cask requires macOS 26 or later. These are locally
ad-hoc-signed builds; Developer ID signing and notarization are separate work.

Install the release tools, update the version in `Cargo.toml` and `Cargo.lock`,
commit the changes, and push `main`. Then run:

```fish
brew install goreleaser actionlint
just release-check
just package
just release v0.1.0
```

`just package` creates snapshot archives in `target/dist` without publishing.
`just release` requires a clean, pushed `main`, checks that the tag matches
`Cargo.toml`, runs the Rust and native checks, creates a signed tag at the checked
commit, and pushes only that tag. If the push fails, the signed local tag is
retained and the command prints the safe retry command.

GitHub Actions first checks a native Intel build, then independently requires a
GitHub-verified annotated tag whose commit matches the checkout and belongs to
`origin/main` history before publishing from the ARM64 runner.

Before the first release, create the `release` environment in
`blacktop/slack-tooy`. Add `HOMEBREW_TAP_TOKEN` there using a fine-grained GitHub
token restricted to `blacktop/homebrew-tap`, with repository Contents permission
set to read and write. From your own terminal:

```fish
gh api --method PUT repos/blacktop/slack-tooy/environments/release
gh secret set HOMEBREW_TAP_TOKEN --repo blacktop/slack-tooy --env release
```

Paste the token at the hidden prompt. After the first workflow succeeds, users
can install with:

```fish
brew install --cask blacktop/tap/slack-tooy
```
