# ── Stage 1: Chef — cache dependency builds across source changes ──
FROM rustlang/rust:nightly-bookworm AS chef

RUN cargo install cargo-chef --locked
RUN apt-get update && apt-get install -y --no-install-recommends \
    cmake pkg-config libopus-dev libssl-dev mold clang libclang-dev \
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
    python3 python3-venv pipx \
    unzip \
    wget xz-utils \
    && apt-get autoremove -y \
    && apt-get clean -y \
    && rm -rf /var/lib/apt/lists/*

# Install deno (required for yt-dlp-ejs)
RUN wget -qO /tmp/deno.zip https://github.com/denoland/deno/releases/latest/download/deno-x86_64-unknown-linux-gnu.zip && \
    unzip -q /tmp/deno.zip -d /usr/local/bin/ && \
    rm /tmp/deno.zip

# Install yt-dlp and inject yt-dlp-ejs natively isolated
ENV PATH="/root/.local/bin:${PATH}"
RUN pipx install yt-dlp && pipx inject yt-dlp yt-dlp-ejs

# Download the highly recommended youtube-patched FFmpeg binaries and bind to path
RUN mkdir -p /tmp/ffmpeg && \
    wget -qO /tmp/ffmpeg.tar.xz https://github.com/yt-dlp/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-linux64-gpl.tar.xz && \
    tar -xf /tmp/ffmpeg.tar.xz -C /tmp/ffmpeg && \
    mv /tmp/ffmpeg/*/bin/ffmpeg /tmp/ffmpeg/*/bin/ffprobe /usr/local/bin/ && \
    rm -rf /tmp/ffmpeg /tmp/ffmpeg.tar.xz

WORKDIR /app

COPY --from=builder /app/target/release/bot /app/teamti_music_bot
COPY --from=builder /app/migrations /app/migrations

ENV RUST_LOG="info"
VOLUME ["/app/media_data"]

CMD ["/app/teamti_music_bot"]
