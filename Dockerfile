# ── Stage 1: Chef — cache dependency builds across source changes ──
FROM rustlang/rust:nightly-bookworm AS chef

RUN cargo install cargo-chef --locked
RUN apt-get update && apt-get install -y --no-install-recommends \
    cmake pkg-config libopus-dev libssl-dev mold \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# ── Stage 2: Planner — compute the dependency recipe ──────────
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ── Stage 3: Builder — build deps first (cached), then source ─
FROM chef AS builder

# Use mold linker + nightly parallel frontend for faster builds
ENV RUSTFLAGS="-C link-arg=-fuse-ld=mold -Zshare-generics=y -Zthreads=0"
ENV SQLX_OFFLINE=true

# Build dependencies only (this layer is cached until Cargo.lock changes)
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json --package bot

# Now build the actual source
COPY . .
RUN cargo build --release --package bot

# ── Stage 4: Runtime — minimal image ─────────────────────────
FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates libopus0 libssl3 \
    && apt-get autoremove -y \
    && apt-get clean -y \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/bot /app/teamti_music_bot
COPY --from=builder /app/migrations /app/migrations

ENV RUST_LOG="info"
VOLUME ["/app/media_data"]

CMD ["/app/teamti_music_bot"]
