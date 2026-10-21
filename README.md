# slack-tooy

A terminal Slack client with vim-style keys, channels, DMs, threads, reactions,
and an Activity inbox for replies you might otherwise miss.

## Install

On macOS 26 or later, install from the [Homebrew tap](https://github.com/blacktop/homebrew-tap):

```sh
brew install --cask blacktop/tap/slack-tooy
```

The cask installs the app and its image decoder. Each release also includes
Apple Silicon and Intel archives on [GitHub](https://github.com/blacktop/slack-tooy/releases).

## Set up Slack

Create `~/.config/slack-tooy/config.toml` with your Slack token:

```toml
slack_token = "xoxp-your-token"
```

Slack [user and bot tokens](https://docs.slack.dev/authentication/tokens) are
supported. For a browser session, use an `xoxc-` token and its `d` cookie:

```toml
slack_token = "xoxc-your-token"
cookie = "xoxd-your-cookie"
```

You can find these in your browser's developer tools while signed into Slack:
look for the token on a Slack API request in the Network tab, and the `d` cookie
under Application → Cookies → `https://app.slack.com`. The cookie can include or
omit the `d=` prefix.

Then run:

```sh
slack-tooy
```

You can also use `--token` and `--cookie`, or the `SLACK_TOKEN` and `SLACK_COOKIE`
environment variables. Command-line values take precedence over the config file,
then environment variables. Use `--config <path>` for a different config file.

## Using it

Press `?` in Normal mode for the full, scrollable key reference. Press `Esc` to
close it.

| Key | Action |
| --- | --- |
| `Tab` | Switch between the sidebar and messages |
| `j` / `k` | Navigate or scroll |
| `Enter` | Open the selected channel or thread |
| `J` / `K` | Select the next or previous message |
| `i` | Write a message; `Enter` sends it |
| `Alt+Enter` / `Ctrl+j` | Add a newline |
| `Esc` | Leave the composer or close a thread |
| `a` | Open Activity |
| `N` | Open the latest notified conversation |
| `+` | Add a reaction in the messages panel |
| `c` / `o` | Copy the selected message / open its links |
| `Space` / `d` | Preview media on macOS / download attached files |
| `q` | Quit |

Channels and Direct Messages start collapsed. Click a heading to expand it, or
press `Z` in the sidebar to expand everything. New messages appear in Unread.
The red new-message divider stays in place while you scroll; clicking the chat
or composer acknowledges it.

Activity shows replies to threads you follow or have participated in, including
threads opened in another Slack client. It uses Slack's session thread API and
works with browser session credentials. New replies trigger the same terminal
bell and named Rex notifications as new channel messages. If your token cannot
access Activity, the inbox shows the error and lets you retry.

In the composer, use `/upload <path> [comment]` to share a file. On macOS,
`Space` opens images in Quick Look and videos in QuickTime Player. For pages such
as YouTube, use `o` to open the browser. To select terminal text with the mouse,
hold your terminal's selection modifier, usually `Shift` or `Option`.

## Configuration

These optional settings show the defaults:

```toml
sidebar_width = 3       # 1–11, in 12-column grid units
tick_rate_ms = 250      # minimum 50
poll_interval_secs = 5  # minimum 1
```

Session state and logs live under `~/Library/Application Support/slack-tooy` on
macOS, or `~/.local/share/slack-tooy` on Linux. Use `--debug` for verbose logging
and `--help` for all command-line options.

On macOS, avatars and inline images are decoded in a separate signed process
under an embedded Seatbelt sandbox. It starts automatically. See the
[decoder documentation](macos/README.md) for its limits and security boundaries.

## Build from source

Use Rust stable, macOS 26 or later, and Xcode 26 or later:

```sh
git clone https://github.com/blacktop/slack-tooy.git
cd slack-tooy
brew install just xcodegen
just install
```

`just install` builds, tests, signs, and installs both executables in
`~/.cargo/bin`. Use `just run` to build and launch, or `just check` for the Rust
checks. On other platforms, run `cargo install --path .` from the checkout.
See [CONTRIBUTING.md](CONTRIBUTING.md) for development and release instructions.

## License

MIT
