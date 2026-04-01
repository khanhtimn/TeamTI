# TeamTI Music Bot

A production-oriented Discord music platform built in Rust. It follows a Hexagonal Monolith architecture in a Cargo workspace.

## Prerequisites
- Rust (>=1.75)
- PostgreSQL

## Startup
1. Copy `.env.example` to `.env` and fill in the values.
2. Initialize database:
   ```sh
   createdb teamti_music
   ```
3. Run the bot (migrations run automatically on start):
   ```sh
   cargo run --bin bot
   ```
   
## Architecture
This project uses Ports and Adapters:
- `domain`: Pure business rules
- `application`: Use cases and port traits
- `adapters-*`: Concrete implementations (Discord, SG/Voice, Postgres)
- `apps/*`: Composition roots

## Extension Points
- **Future Portal**: `apps/portal` is currently a placeholder crate meant to grow into a web interface.
- **Richer Discord Interactions**: Extend `crates/adapters-discord` commands to include buttons or select menus if required, wiring them into new application services.
- **Uploaded Media**: `MediaStore` port (`import_local`) can be augmented with `import_stream` or similar to handle incoming file uploads from a web interface into the same managed store.
- **Remote Providers**: Add `PlayableSource::UnresolvedRemote` handling in `adapters-voice` to relay audio from YouTube or Apple Music instead of just local filesystem.
- **S3 Media Storage**: Create an `S3Store` implementing `MediaStore` in `adapters-media-store`, keeping the application core oblivious to the backend.
