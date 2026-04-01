# V1 Scaffolding Report

**Date**: April 2026  
**Status**: Core scaffolding complete. Workspace compiling cleanly. Migrated to serenity `next` and songbird `serenity-next` development branches.

## Executive Summary
The `teamti` workspace has been successfully initialized as a Rust Cargo Workspace implementing a **Hexagonal Monolith Architecture**. The primary objective of separating pure business logic from external adapter logic (Discord APIs, Voice logic, Postgres persistence, and Filesystem I/O) has been achieved. 

The immediate V1 milestone—a single-guild Discord music bot capable of joining voice, reading a database schema, and handling local files—has its skeleton fully fleshed out and validated by the compiler.

## Application Architecture

### Core (Inward-facing)
- **`crates/domain`**: Contains pure entities. `MediaAsset`, `Playlist`, `QueueRequest`, and `GuildId` were created using `uuid` keys. Completely decoupled from external libraries.
- **`crates/application`**: Defines our core use cases (`JoinVoice`, `LeaveVoice`, `RegisterMedia`, `EnqueueTrack`) and our `ports` interfaces (`MediaStore`, `PlaybackGateway`, `MediaRepository`, `SettingsRepository`). 

### Adapters (Outward-facing)
- **`crates/adapters-discord`**: Implements Serenity (`next` branch). Wires up `/ping`, `/join`, `/leave`, and `/play_local` slash commands via the new `dispatch()`-based `EventHandler`. Registers commands to the target guild on startup. Successfully handles cross-thread async caching blocks to pass `Send` bounds safely.
- **`crates/adapters-voice`**: Implements `PlaybackGateway` via songbird (`serenity-next` branch). Leverages the `"builtin-queue"` feature to manage basic sequencing. Registers global `TrackEvent` and `CoreEvent` handlers for driver lifecycle logging.
- **`crates/adapters-persistence`**: Implements `MediaRepository` and others via `sqlx` and Postgres. Includes the `0001_initial_schema.sql` migration for the `teamti_music` DB. Uses compile-time query macros correctly.
- **`crates/adapters-media-store`**: Implements `MediaStore` by copying local inputs into a central storage blob root and minting `ManagedBlobRef` keys.

### Presentation / Apps (Composition Roots)
- **`apps/bot`**: Wires up all specific adapters into `Arc` traits, establishes DB connection, runs pending migrations via `sqlx::migrate!`, initializes the Songbird context globally, and launches the Serenity client. Installs the rustls `ring` crypto provider at startup.
- **`apps/portal`**: Placeholder crate prepared for future web APIs.

## Verified State
- `cargo check --workspace` passes cleanly with latest dependencies.
- Compile-time macro `sqlx::query!` is successfully bound to the applied initial Postgres schema.
- Songbird integration uses `.voice_manager(songbird)` on the `ClientBuilder` (takes `Arc<dyn VoiceGatewayManager>`).
- Serenity `EventHandler` uses the new `dispatch(&self, &Context, &FullEvent)` pattern.

---

## Serenity `next` & Songbird `serenity-next` Migration Reference

This project tracks the **development branches** of both serenity and songbird. These branches contain significant breaking API changes compared to stable releases. Subsequent agents **must** understand the patterns documented below before modifying any Discord or voice adapter code.

### Source Branches & References

| Library | Branch | Repository |
|---|---|---|
| serenity | `next` | https://github.com/serenity-rs/serenity/tree/next |
| songbird | `serenity-next` | https://github.com/serenity-rs/songbird/tree/serenity-next |

**Key source files to reference directly** (read raw source when the docs are insufficient):
- `EventHandler` trait: [`src/gateway/client/event_handler.rs`](https://github.com/serenity-rs/serenity/blob/next/src/gateway/client/event_handler.rs)
- `FullEvent` enum: [`src/model/event.rs`](https://github.com/serenity-rs/serenity/blob/next/src/model/event.rs)
- `ClientBuilder`: [`src/gateway/client/mod.rs`](https://github.com/serenity-rs/serenity/blob/next/src/gateway/client/mod.rs)
- `Token` type: [`src/secrets.rs`](https://github.com/serenity-rs/serenity/blob/next/src/secrets.rs)
- `CreateCommand` builder: [`src/builder/create_command.rs`](https://github.com/serenity-rs/serenity/blob/next/src/builder/create_command.rs)
- Songbird serenity example: [`examples/serenity/voice/src/main.rs`](https://github.com/serenity-rs/songbird/blob/serenity-next/examples/serenity/voice/src/main.rs)

> **NOTE**: The songbird serenity example may be outdated relative to the latest serenity `next` branch. Always prefer the serenity source code as the authoritative reference for `EventHandler`, `ClientBuilder`, and `Token` types.

### Breaking API Changes — Quick Reference

#### 1. EventHandler Trait (serenity)

**Old pattern** (removed):
```rust
#[async_trait]
impl EventHandler for MyHandler {
    async fn ready(&self, ctx: Context, ready: Ready) { ... }
    async fn interaction_create(&self, ctx: Context, interaction: Interaction) { ... }
}
```

**New pattern** (required):
```rust
use serenity::model::event::FullEvent;

#[async_trait]
impl EventHandler for MyHandler {
    async fn dispatch(&self, ctx: &Context, event: &FullEvent) {
        match event {
            FullEvent::Ready { data_about_bot, .. } => { ... }
            FullEvent::InteractionCreate { interaction, .. } => { ... }
            _ => {}
        }
    }
}
```

Key differences:
- `ctx` is now `&Context` (reference), not owned `Context`
- All events are routed through a single `dispatch()` method
- `FullEvent` variants are `#[non_exhaustive]` — match arms **must** include `..`
- The `Ready` event's field is named `data_about_bot`, not `ready`

#### 2. Client Builder (serenity)

```rust
use serenity::prelude::Token;

let token: Token = config.discord_token.parse()?;

let mut client = Client::builder(token, intents)     // Token type, not &str
    .event_handler(Arc::new(handler))                 // Arc<dyn EventHandler>
    .voice_manager(songbird)                          // was voice_manager_arc()
    .await?;
```

#### 3. Builder Lifetime Parameters (serenity)

`CreateCommand` and `CreateCommandOption` now have lifetime parameters:
```rust
pub fn ping() -> CreateCommand<'static> {
    CreateCommand::new("ping").description("A ping command")
}
```

#### 4. Command Registration (serenity)

`set_commands` now takes a slice reference instead of `Vec`:
```rust
guild_id.set_commands(http, &[
    commands::ping(),
    commands::join(),
]).await?;
```

#### 5. Rustls Crypto Provider

rustls 0.23+ requires explicitly installing a crypto provider before any TLS connections:
```rust
rustls::crypto::ring::default_provider()
    .install_default()
    .expect("Failed to install rustls crypto provider");
```
This must be the **first thing** in `main()`, before database connections or Discord client initialization.

#### 6. Songbird Voice API (unchanged)

The songbird events API is **stable** across the `serenity-next` branch:
- `EventHandler::act(&self, &EventContext) -> Option<Event>` — unchanged
- `EventContext::Track`, `EventContext::DriverConnect`, etc. — unchanged
- `Songbird::join()`, `Songbird::leave()`, `Songbird::get()` — unchanged
- `Call::add_global_event()`, `Call::enqueue()`, `Call::queue()` — unchanged
- `songbird::input::File`, `songbird::input::Input` — unchanged

### Workspace Dependencies Configuration

```toml
# Discord & Voice — tracking development branches
serenity = { git = "https://github.com/serenity-rs/serenity", branch = "next", features = [
    "gateway", "rustls_backend", "model", "cache", "voice",
] }
songbird = { git = "https://github.com/serenity-rs/songbird", branch = "serenity-next", features = [
    "driver", "rustls", "serenity", "builtin-queue",
] }
rustls = { version = "0.23", default-features = false, features = ["ring", "std"] }
```

### Guidance for Subsequent Agents

1. **Do not use serenity crate docs from docs.rs** — they reflect the stable release, not the `next` branch. Read the source code directly from the GitHub links above.
2. **When adding new event handlers**, add new match arms to the existing `dispatch()` method in `handler.rs`. Do not try to add separate trait methods.
3. **When creating new builders** (embeds, components, modals), check for lifetime parameters in the source — most builder types in `next` use `Cow<'a, str>` internally.
4. **The `Interaction` enum** still uses `Interaction::Command(CommandInteraction)` for slash commands.
5. **Context references**: Since `ctx` is now `&Context`, there is no need to clone it for passing to helper methods. This is more ergonomic than the old owned-value pattern.
6. **`serenity::prelude::*`** re-exports: `Client`, `Context`, `EventHandler`, `RawEventHandler`, `GatewayIntents`, `Token`, `Mentionable`, and error types. Use the prelude import for common types.

---

## Known Gaps & Next Steps for Subsequent Agents

When expanding upon this initial scaffold, next-step agents should prioritize the following areas:

### 1. Songbird Track Events & Playback State
- **Gap**: We currently shove items into the `Songbird` queue natively, but aren't listening for when tracks finish, error out, or get skipped.
- **Action**: Register global `TrackEvent` handlers in `adapters-voice/src/songbird_gateway.rs`.
- **Action**: These handlers should probably communicate back to `application` (maybe via an MPSC channel or callback) to log playback into the `queue_history` DB table and update the user interface.

### 2. Media Metadata & Extraction (Symphonia)
- **Gap**: `crates/application/src/services/register_media.rs` currently registers local paths directly and leaves `duration_ms` as `None`.
- **Action**: Integrate `symphonia` or `ffprobe` into `adapters-media-store` to extract true track length, artist, and bitrate before writing the `MediaAsset` to Postgres.

### 3. Remote Streaming & Providers
- **Gap**: The system is designed to handle `MediaOrigin::Remote`, but it throws a "Not Supported" error in `adapters-voice/src/mapper.rs`.
- **Action**: Implement plugins for Apple Music / YouTube URL resolution to stream audio buffers directly into `Songbird`.

### 4. User Interaction & Settings
- **Gap**: `/play_local` expects a raw filesystem path from the end-user, which is dangerous/unfriendly. Settings fetches are stubbed.
- **Action**: Implement interactive Discord components (Buttons, Select Menus). Provide a search or auto-complete command using Postgres text-matching to find existing `MediaAssets` by name.

### 5. Web Portal Initialization
- **Gap**: `apps/portal` is completely empty.
- **Action**: Set up `axum` or `actix-web` within that crate. Provide REST/GraphQL endpoints that utilize the existing `crates/application` ports to fetch `Playlists` and `QueueHistory` to render a frontend.
