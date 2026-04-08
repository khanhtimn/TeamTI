# TeamTI Music Bot 🎵

A Discord music bot for local libraries.

## Quick Start

1. [Features](#features)
2. [Commands](#commands)
3. [Setup](#setup)
4. [Environment Variables](#environment-variables)
5. [First Use](#first-use)
6. [Development](#development)

## Features

| Area | What You Get |
| --- | --- |
| ▶ Playback | Play tracks in voice channels, pause/resume, skip, leave |
| 🧾 Queue | View queue, remove/move tracks, shuffle, clear |
| 📀 Library | Save and play playlists, manage favourites, view history |
| 📻 Discovery | Radio mode for continuous playback |
| 🔎 Search | Fast slash-based search and selection |
| 🎚 Session UX | Now playing view and queue controls for active sessions |

## Commands

| Group | Commands |
| --- | --- |
| Playback | `/play`, `/pause`, `/resume`, `/skip`, `/leave`, `/nowplaying` |
| Queue | `/queue`, `/remove`, `/move`, `/shuffle`, `/clear` |
| Library | `/playlist`, `/favourite`, `/history` |
| Discovery / Maintenance | `/radio`, `/rescan` |

## Setup

### Option 1: Docker Compose (Recommended) 🐳

1. Create a `.env` file next to `docker-compose.yml`.
2. Set required values:
  - `POSTGRES_USER`
  - `POSTGRES_PASSWORD`
  - `POSTGRES_DB`
   - `DISCORD_TOKEN`
   - `DISCORD_GUILD_ID`
   - `MEDIA_ROOT`
   - `ACOUSTID_API_KEY`
3. Use this `docker-compose.yml`:

```yaml
services:
  db:
    image: pgvector/pgvector:pg18-trixie
    container_name: postgres
    restart: unless-stopped
    ports:
      - "5432:5432"
    environment:
      POSTGRES_USER: ${POSTGRES_USER}
      POSTGRES_PASSWORD: ${POSTGRES_PASSWORD}
      POSTGRES_DB: ${POSTGRES_DB}
    volumes:
      - pg_data:/var/lib/postgresql/data
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U ${POSTGRES_USER}"]
      interval: 5s
      timeout: 3s
      retries: 5
  bot:
    image: khanhtimn/teamti_music_bot:latest
    container_name: teamti_music_bot
    restart: unless-stopped
    depends_on:
      db:
        condition: service_healthy
    environment:
      DATABASE_URL: postgres://${POSTGRES_USER}:${POSTGRES_PASSWORD}@db:5432/${POSTGRES_DB}
      DISCORD_TOKEN: ${DISCORD_TOKEN}
      DISCORD_GUILD_ID: ${DISCORD_GUILD_ID}
      MEDIA_ROOT: /app/media_data
      ACOUSTID_API_KEY: ${ACOUSTID_API_KEY}
    volumes:
      - search_data:/app/tantivy_index
      - ${MEDIA_ROOT}:/app/media_data
volumes:
  pg_data:
  search_data:
```

4. Start services:

```sh
docker compose up -d
```

### Option 2: Build From Source 🛠

1. Install Rust nightly and PostgreSQL with vector extension.
2. Create `.env` from `.env.example`.
3. Set required values:
   - `DATABASE_URL`
   - `DISCORD_TOKEN`
   - `DISCORD_GUILD_ID`
   - `MEDIA_ROOT`
   - `ACOUSTID_API_KEY`

4. Start bot:

```sh
cargo run
```

## Environment Variables

Use `.env.example` as the source of truth.

| Name | Required | Type | Default | Description |
| --- | --- | --- | --- | --- |
| `DATABASE_URL` | Source only | string | `postgres://postgres:password@localhost:5432/teamti_music` | PostgreSQL connection URL |
| `POSTGRES_USER` | Docker only | string | `postgres` | PostgreSQL username for compose DB |
| `POSTGRES_PASSWORD` | Docker only | string | `password` | PostgreSQL password for compose DB |
| `POSTGRES_DB` | Docker only | string | `teamti_music` | PostgreSQL database name for compose DB |
| `DISCORD_TOKEN` | Yes | string | none | Discord bot token |
| `DISCORD_GUILD_ID` | Yes | string | none | Discord server ID for command registration |
| `MEDIA_ROOT` | Yes | path | `./media_data` | Local path to media library |
| `ACOUSTID_API_KEY` | Yes | string | none | AcoustID API key |
| `LASTFM_API_KEY` | No | string | empty | Enables Last.fm similarity features |
| `SCAN_INTERVAL_SECS` | No | u64 | `300` | Scan interval in seconds |
| `AUTO_LEAVE_SECS` | No | u64 | `30` | Idle auto-leave delay in seconds |
| `TANTIVY_INDEX_PATH` | No | path | `./search_index` | Local path for search index files |
| `DB_POOL_SIZE` | No | u32 | `10` | Database connection pool size |
| `RUST_LOG` | No | string | `info,teamti=debug` | Log level filter |
| `LOG_FORMAT` | No | string | `pretty` | Log output style |

## First Use

1. Start the bot.
2. Join a voice channel in your Discord server.
3. Run `/play` and select a track.
4. Use `/queue` and playback commands as needed.

## Development

### Repository Layout

| Path | Purpose |
| --- | --- |
| `apps/bot` | Bot runtime and wiring |
| `crates/domain` | Domain models and rules |
| `crates/application` | Use-cases and ports |
| `crates/adapters-*` | Adapter implementations (Discord, DB, voice, search, APIs) |
| `migrations` | PostgreSQL schema migrations |