You are scaffolding a production-oriented Rust workspace for a private Discord music platform.

Your task is to generate a complete, compilable starting workspace that is architecture-first and ready for long-term growth. The immediate v1 goal is a Discord music bot that can communicate with Discord, join voice, queue tracks, and stream local managed media. The broader product direction includes a future web portal, uploaded media, provider integrations, playlists, and a Postgres-backed metadata platform.

Important: do not design this as “just a bot.” Design it as a hexagonal monolith in a Cargo workspace, where the Discord bot is only one adapter/application entrypoint.

================================
PROJECT GOALS
================================

Immediate scope (v1):
- Discord bot for a single guild
- Slash commands only
- Startup command registration
- Join voice channel
- Leave voice channel
- Enqueue and play managed local media
- Use Songbird built-in queue for playback sequencing
- Persist metadata in Postgres via SQLx
- Managed media binaries stored on filesystem only for now
- Future web portal exists as an empty placeholder crate in the same workspace

Planned future scope (must influence architecture now):
- Member-uploaded content
- Unified managed media catalog
- Playlists
- Queue history
- Guild settings
- Additional provider integrations
- Apple Music as an integration target for catalog/provider metadata later
- Future web portal in same workspace
- Possible richer Discord interactions later (buttons/select menus), but not required in v1

================================
NON-GOALS FOR V1
================================

- No Poise in v1
- No web server logic yet beyond placeholder portal crate
- No event bus in v1
- No active playback restoration after restart
- No Apple Music playback relay implementation
- No object storage yet
- No storing binary media in Postgres yet
- No multi-guild optimization beyond keeping guild-aware types

================================
ARCHITECTURAL DECISIONS
================================

Use these decisions as fixed requirements:

1. Discord framework:
- Use raw Serenity, not Poise, for v1
- Reason: keep framework coupling low and preserve clean adapter boundaries

2. Voice:
- Use Songbird with serenity integration
- Use Songbird built-in queue for v1 playback flow
- Ensure required gateway intent for voice is configured

3. Persistence:
- Use Postgres as system of record for metadata
- Use SQLx
- Include migrations
- Metadata only for v1; no playback-state restoration

4. Media storage:
- Use filesystem storage for managed media binaries
- Postgres stores metadata, logical references, playlists, settings, queue history, and provider link records
- Build a media store abstraction now
- Normalize local/imported/uploaded media into one managed media store model from day one
- For v1, implement filesystem-backed media storage only

5. Workspace:
- Use one top-level Cargo workspace
- Include a placeholder portal crate from day one
- Use architecture-driven crate boundaries, not feature-only splits
- Use workspace dependency inheritance where appropriate
- Explicitly set workspace resolver

6. Application style:
- Hexagonal monolith
- Domain and application crates must not depend on Serenity, Songbird, SQLx, or filesystem implementation details
- Adapters depend inward, not vice versa

7. Internal orchestration:
- No central event bus in v1
- Use direct application-service calls
- Background jobs may be introduced later, but not now

8. Commands:
- Slash-command definitions and startup registration belong in Discord adapter
- Application crate exposes typed use cases and request/response types
- Discord adapter translates interaction payloads into application use-case calls

9. Source model:
- Design for future audio source kinds:
  - Local/imported managed file
  - Uploaded managed file
  - Remote URL
  - Provider catalog reference
- But only filesystem-backed managed local media must work in v1

================================
TECHNICAL FACTS TO RESPECT
================================

- Cargo workspaces share a lockfile and target directory; use workspace inheritance sensibly
- Explicitly set workspace resolver in the root Cargo.toml
- For voice support, Songbird/Discord voice handling requires GUILD_VOICE_STATES intent
- Use guild-scoped slash command registration in startup flow since this is a private single-guild bot
- Keep the bot compileable and runnable at each stage
- SQLx migrations should be part of the scaffold and runnable from the workspace/app flow
- Use tracing and tracing-subscriber for structured logs
- Use dotenvy or equivalent for local config loading
- Keep comments short and useful

================================
TARGET WORKSPACE SHAPE
================================

Generate this workspace structure, or a very close equivalent if you can justify small changes:

/
├─ Cargo.toml
├─ .env.example
├─ README.md
├─ rust-toolchain.toml               # optional if justified
├─ migrations/                       # if you choose one shared migration root
│  └─ ...
├─ apps/
│  ├─ bot/
│  │  ├─ Cargo.toml
│  │  └─ src/main.rs
│  └─ portal/
│     ├─ Cargo.toml
│     └─ src/lib.rs                  # empty placeholder crate only
└─ crates/
   ├─ domain/
   │  ├─ Cargo.toml
   │  └─ src/
   │     ├─ lib.rs
   │     ├─ media.rs
   │     ├─ playback.rs
   │     ├─ playlist.rs
   │     ├─ guild.rs
   │     └─ error.rs
   ├─ application/
   │  ├─ Cargo.toml
   │  └─ src/
   │     ├─ lib.rs
   │     ├─ ports/
   │     │  ├─ mod.rs
   │     │  ├─ media_repository.rs
   │     │  ├─ media_store.rs
   │     │  ├─ playback_gateway.rs
   │     │  └─ settings_repository.rs
   │     ├─ services/
   │     │  ├─ mod.rs
   │     │  ├─ enqueue_track.rs
   │     │  ├─ join_voice.rs
   │     │  ├─ leave_voice.rs
   │     │  └─ register_media.rs
   │     └─ dto.rs
   ├─ adapters-discord/
   │  ├─ Cargo.toml
   │  └─ src/
   │     ├─ lib.rs
   │     ├─ commands.rs
   │     ├─ handler.rs
   │     ├─ register.rs
   │     └─ response.rs
   ├─ adapters-voice/
   │  ├─ Cargo.toml
   │  └─ src/
   │     ├─ lib.rs
   │     ├─ songbird_gateway.rs
   │     └─ mapper.rs
   ├─ adapters-persistence/
   │  ├─ Cargo.toml
   │  └─ src/
   │     ├─ lib.rs
   │     ├─ db.rs
   │     ├─ models.rs
   │     ├─ repositories/
   │     │  ├─ mod.rs
   │     │  ├─ media_repository.rs
   │     │  ├─ playlist_repository.rs
   │     │  ├─ queue_history_repository.rs
   │     │  └─ settings_repository.rs
   │     └─ migrations.rs            # optional helper
   ├─ adapters-media-store/
   │  ├─ Cargo.toml
   │  └─ src/
   │     ├─ lib.rs
   │     ├─ fs_store.rs
   │     ├─ importer.rs
   │     └─ path_policy.rs
   ├─ shared-config/
   │  ├─ Cargo.toml
   │  └─ src/lib.rs
   └─ shared-observability/
      ├─ Cargo.toml
      └─ src/lib.rs

================================
CRATE RESPONSIBILITIES
================================

These boundaries are important:

- domain:
  Pure types and invariants only. No Serenity, Songbird, SQLx, or filesystem code.

- application:
  Pure use-case orchestration and ports.
  Owns typed requests/responses and business-level operations.
  Does not know Serenity, Songbird, SQLx, or direct filesystem APIs.

- adapters-discord:
  Owns slash command schema, guild registration on startup, interaction parsing, response formatting.
  Maps Discord payloads into application use cases.

- adapters-voice:
  Implements playback/voice ports using Songbird.
  Handles join/leave/enqueue at transport/playback layer.
  Must not absorb all business logic.

- adapters-persistence:
  Implements repositories with SQLx and Postgres.
  Includes migrations and repository structs.

- adapters-media-store:
  Implements managed media binary storage on filesystem.
  Handles import/copy into managed storage root.
  Should support creating canonical managed paths and returning logical blob locations.

- shared-config:
  Typed environment config.

- shared-observability:
  tracing setup.

- apps/bot:
  Composition root only.
  Boot config, init logs, connect DB, run migrations, build Serenity client, register Songbird, wire adapters to application, and start bot.

- apps/portal:
  Empty placeholder crate only.
  Do not add actual web logic yet.

================================
V1 DOMAIN MODEL EXPECTATIONS
================================

Generate a clean starting domain model for:
- GuildId wrapper or equivalent
- MediaAsset
- MediaOrigin
- BlobLocation / ManagedBlobRef
- PlayableSource / ResolvedPlayable
- Playlist
- PlaylistItem
- Queue request / Enqueue request types
- Basic app/domain error types

Suggested media model direction:
- MediaAsset is canonical metadata
- Local/imported files are normalized into managed assets
- Files are copied/imported into managed storage root
- Database stores metadata and blob references, not raw file bytes in v1

================================
DATABASE REQUIREMENTS
================================

Use Postgres + SQLx.

Include:
- schema/migrations for:
  - media_assets
  - playlists
  - playlist_items
  - guild_settings
  - queue_history
  - provider_links (or equivalent placeholder table)
- basic repository methods needed for v1
- startup migration execution
- SQLx-friendly setup

Prefer:
- SQLx migrations in a workspace-root migrations folder, unless you strongly justify another layout
- sqlx::migrate! usage or a similarly robust approach
- repository implementations that are simple and direct

================================
DISCORD / VOICE REQUIREMENTS
================================

Required commands in v1:
- /ping
- /join
- /leave
- /play_local path:<string>

Behavior:
- /ping responds successfully
- /join joins the invoking member’s current voice channel
- /leave leaves the current guild voice channel
- /play_local path:<string> imports or resolves a local file into the managed media store if needed, persists metadata, and enqueues it for playback
- show clean error messages if user is not in voice, guild is missing, path invalid, or file unsupported

Implementation notes:
- use slash commands only
- guild command registration on startup
- use Songbird built-in queue
- configure required intents including voice state intent
- keep the implementation minimal but real
- if importer and playback need to be split, do so cleanly

================================
DEPENDENCIES
================================

Use modern stable crate versions that are compatible with each other.
Prefer workspace dependency inheritance.

Expected crates include:
- tokio
- serenity
- songbird
- symphonia (if needed for codec support decisions)
- sqlx
- tracing
- tracing-subscriber
- anyhow
- thiserror
- serde
- serde_json
- uuid
- chrono or time
- dotenvy

You may add a small number of extra crates if justified.

================================
OUTPUT FORMAT
================================

Produce the result in this order:

1. Explain the chosen architecture briefly and confirm dependency direction
2. Show the final workspace tree
3. Show the root Cargo.toml
4. Show each crate Cargo.toml
5. Show all Rust source files needed for a compilable starting point
6. Show SQL migrations
7. Show README.md
8. Show .env.example
9. Show exact commands to run:
   - create DB
   - run migrations
   - cargo check
   - run bot
10. Explain key extension points for:
   - future portal
   - richer Discord interactions
   - uploaded media
   - remote/provider sources
   - alternate binary storage backends

================================
QUALITY BAR
================================

- The code must compile or be very close to compileable with conservative API choices
- Avoid placeholder-only architecture with no working vertical slice
- Favor one working vertical slice over many TODOs
- Keep the bot binary thin
- Keep core crates free of Discord/SQL/filesystem coupling
- Keep comments brief
- Do not use Poise
- Do not overabstract with excessive traits
- Do not create circular dependencies
- Keep v1 simple, but preserve long-term boundaries

================================
ASSUMPTIONS TO STATE
================================

If you must assume an API detail for current serenity/songbird/sqlx versions, state the assumption explicitly and choose the most conservative implementation path.

Important:
- Generate actual file contents, not just descriptions.
- Prefer compileable code over ambitious abstractions.
- If a specific API is uncertain, keep the adapter narrow and document the assumption inline.
- Use guild-scoped slash commands for startup registration.