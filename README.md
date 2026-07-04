# slack-tooy

A terminal Slack TUI with vim-style keybindings.

## Install

Requires Rust stable (latest).

```sh
cargo install --path .
```

Or run directly:

```sh
cargo run --release
```

## Authentication

slack-tooy needs a Slack API token. Token precedence: CLI flag > config file > `SLACK_TOKEN` env var.

### User/bot tokens (`xoxp-`, `xoxb-`)

Set the token in the config file or pass it directly:

```sh
slack-tooy --token xoxp-your-token
```

### Browser session tokens (`xoxc-`)

`xoxc-` tokens require a browser session cookie. To get both:

1. Open Slack in your browser
2. Open Developer Tools (F12)
3. Go to Application > Cookies > `https://app.slack.com`
4. Copy the `d` cookie value (starts with `xoxd-`)
5. Go to Console, run `window.prompt("token", document.cookie.match(/(?:^|;\s*)xoxc[^;]+/)?.[0]?.split("=")[1])` and copy the token

Then either set both in the config file or pass them:

```sh
slack-tooy --token xoxc-your-token --cookie xoxd-your-cookie
```

The cookie value can be passed with or without the `d=` prefix.

## Configuration

Config file location: `~/.config/slack-tooy/config.toml`

```toml
slack_token = "xoxp-your-token"
cookie = ""                    # required for xoxc- tokens
sidebar_width = 3              # 1-11, in 12-column grid units
tick_rate_ms = 250             # UI refresh interval (min 50)
poll_interval_secs = 5         # how often to poll Slack for new messages (min 1)
```

All fields are optional. Token and cookie can also be set via `SLACK_TOKEN` and `SLACK_COOKIE` environment variables.

## Keybindings

### Global (Normal mode)

| Key | Action |
|-----|--------|
| `q` | Quit |
| `i` | Enter insert mode |
| `Esc` | Return to normal mode |
| `Tab` | Switch focus between Channels and Messages |
| `1` / `2` | Focus Channels / Messages directly |

### Channels panel

| Key | Action |
|-----|--------|
| `j` / `k` | Move selection down / up |
| `g` / `G` | Jump to first / last channel |
| `z` / `Z` | Collapse current section / expand all sections |
| `Enter` | Open channel |
| `R` | Mark all channels as read |
| `u` | Toggle unread-only filter |

### Messages panel

| Key | Action |
|-----|--------|
| `j` / `k` | Scroll by line |
| `J` / `K` | Jump to next / previous message |
| `l` / Right | Open thread for selected message |
| `h` / Left | Close thread |
| `Enter` | Open thread (alias for `l`) |
| `d` | Download files on the selected message to `~/Downloads` |
| `Esc` | Close thread |
| `g` / `G` | Scroll to top / bottom |
| `Ctrl+u` / `Ctrl+d` | Page up / down |
| `Ctrl+b` / `Ctrl+f` | Large scroll up / down |

### Insert mode

| Key | Action |
|-----|--------|
| `Enter` | Send message |
| `Alt+Enter` / `Ctrl+J` | Insert newline |
| `Shift+Enter` | Insert newline (terminals with the kitty keyboard protocol — kitty, Ghostty, foot, WezTerm with `enable_kitty_keyboard=true`; elsewhere it sends) |
| `/upload <path> [comment]` | Upload a local image/file to the current chat |
| `Esc` | Return to normal mode |
| Arrow keys | Move cursor |
| `Home` / `End` | Move to start / end of line |

### Mouse

| Action | Effect |
|--------|--------|
| Click a channel | Open it |
| Click a section header | Collapse / expand the section |
| Click a message | Select it |
| Click the input box | Enter insert mode |
| Scroll wheel over messages | Scroll history |
| Scroll wheel over channels | Move selection |

Mouse capture takes over the terminal's native text selection — hold `Shift`
(most terminals) or `Option`/`Fn` (macOS Terminal.app/iTerm2) while dragging
to select text normally.

## Data storage

slack-tooy stores session and read-state data in a SQLite database:

- macOS: `~/Library/Application Support/slack-tooy/slack-tooy.db`
- Linux: `~/.local/share/slack-tooy/slack-tooy.db`

Logs are written to the same directory under `logs/app.log`. Pass `--debug` for verbose output.

## Features

- Channel and DM navigation with unread indicators
- Threaded conversations
- Message sending and replies
- User avatar display (on terminals that support image protocols)
- Custom emoji rendering in reactions
- Unicode-aware text wrapping
- Vim-style keybindings throughout
- Session persistence (remembers last channel across restarts)

## CLI options

```
Usage: slack-tooy [OPTIONS]

Options:
  -c, --config <CONFIG>  Path to config file
  -t, --token <TOKEN>    Slack API token (overrides config file)
      --cookie <COOKIE>  Browser session cookie for xoxc- tokens (the `d` value)
  -d, --debug            Enable debug logging
  -h, --help             Print help
  -V, --version          Print version
```

## License

MIT
