# voisestats2

A minimal Rust Discord bot for one small private server. It tracks all-time voice activity, keeps one persistent embed panel, logs joins and leaves, and creates temporary personal voice rooms from a permanent "create voice" channel.

It intentionally does not use slash commands, buttons, a web dashboard, Redis, PostgreSQL, Docker, a command framework, or the Message Content intent.

## Required Discord permissions

Give the bot role these permissions in the server:

- View Channels
- Send Messages
- Embed Links
- Read Message History
- Manage Channels
- Move Members
- Connect

## Required gateway intents

Enable these in the Discord developer portal and in the bot code:

- Server Members Intent
- Guilds
- Guild Voice States
- Guild Members

The bot does not request Message Content intent.

## Environment variables

Copy `.env.example` to `.env` and fill in the values:

```env
DISCORD_TOKEN=your_discord_bot_token_here
GUILD_ID=your_guild_id_here
PANEL_CHANNEL_ID=1506016592645853255
LOG_CHANNEL_ID=1506019143005245623
CREATE_VOICE_CHANNEL_ID=1505903481691705447
DATABASE_PATH=data/voicebot.sqlite
PANEL_UPDATE_SECONDS=15
RUST_LOG=info
```

`DATABASE_PATH` uses SQLite. Discord snowflake IDs are stored as `TEXT`.

## Run locally

Install Rust, then run:

```bash
cargo fmt
cargo check
cargo run
```

The bot initializes the SQLite tables automatically. On startup it closes stale active sessions, recreates active sessions for users currently in voice if they are visible in cache, cleans up empty known temporary channels, and creates or edits the persistent panel message.

## Run on a VPS with systemd

Build the bot:

```bash
cargo build --release
```

Create a working directory such as `/opt/voisestats2`, then place the compiled binary and `.env` there:

```bash
sudo mkdir -p /opt/voisestats2
sudo cp target/release/voisestats2 /opt/voisestats2/
sudo cp .env /opt/voisestats2/.env
```

Create `/etc/systemd/system/voisestats2.service`:

```ini
[Unit]
Description=Voice Stats Discord Bot
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
WorkingDirectory=/opt/voisestats2
EnvironmentFile=/opt/voisestats2/.env
ExecStart=/opt/voisestats2/voisestats2
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

Enable and start it:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now voisestats2
sudo journalctl -u voisestats2 -f
```
