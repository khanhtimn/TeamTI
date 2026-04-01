# V1 Scaffolding Report

**Date**: April 2026  
**Status**: Core scaffolding complete. Workspace compiling cleanly.

## Executive Summary
The `teamti` workspace has been successfully initialized as a Rust Cargo Workspace implementing a **Hexagonal Monolith Architecture**. The primary objective of separating pure business logic from external adapter logic (Discord APIs, Voice logic, Postgres persistence, and Filesystem I/O) has been achieved. 

The immediate V1 milestone—a single-guild Discord music bot capable of joining voice, reading a database schema, and handling local files—has its skeleton fully fleshed out and validated by the compiler.

## Application Architecture

### Core (Inward-facing)
- **`crates/domain`**: Contains pure entities. `MediaAsset`, `Playlist`, `QueueRequest`, and `GuildId` were created using `uuid` keys. Completely decoupled from external libraries.
- **`crates/application`**: Defines our core use cases (`JoinVoice`, `LeaveVoice`, `RegisterMedia`, `EnqueueTrack`) and our `ports` interfaces (`MediaStore`, `PlaybackGateway`, `MediaRepository`, `SettingsRepository`). 

### Adapters (Outward-facing)
- **`crates/adapters-discord`**: Implements Serenity. Wires up `/ping`, `/join`, `/leave`, and `/play_local` slash commands. Registers them to the target guild on startup. Successfully handles cross-thread async caching blocks to pass `Send` bounds safely.
- **`crates/adapters-voice`**: Implements `PlaybackGateway` via `songbird 0.5.0`. Leverages the `"builtin-queue"` feature to manage basic sequencing.
- **`crates/adapters-persistence`**: Implements `MediaRepository` and others via `sqlx` and Postgres. Includes the `0001_initial_schema.sql` migration for the `teamti_music` DB. Uses compile-time query macros correctly.
- **`crates/adapters-media-store`**: Implements `MediaStore` by copying local inputs into a central storage blob root and minting `ManagedBlobRef` keys.

### Presentation / Apps (Composition Roots)
- **`apps/bot`**: Wires up all specific adapters into `Arc` traits, establishes DB connection, runs pending migrations via `sqlx::migrate!`, initializes the Songbird context globally, and launches the Serenity client.
- **`apps/portal`**: Placeholder crate prepared for future web APIs.

## Verified State
- `cargo check --workspace` passes cleanly with latest dependencies.
- Compile-time macro `sqlx::query!` is successfully bound to the applied initial Postgres schema.
- Songbird integration successfully tracks the 0.5.x API signatures standardizing around `.voice_manager_arc()`.

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
