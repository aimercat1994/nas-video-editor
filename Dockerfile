# Build stage
FROM rust:1.87-slim AS builder

RUN apt-get update && apt-get install -y pkg-config && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY src/ ./src/

RUN cargo build --release

# Runtime stage
FROM debian:trixie-slim

RUN echo 'deb http://deb.debian.org/debian trixie non-free-firmware' >> /etc/apt/sources.list.d/non-free.list &&     apt-get update &&     apt-get install -y --no-install-recommends       ffmpeg       intel-media-va-driver       i965-va-driver       libva-drm2       libva2       vainfo &&     rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/nas-video-editor /app/nas-video-editor
COPY frontend/ /app/frontend/
RUN mkdir -p /videos /data

EXPOSE 8080

CMD ["/app/nas-video-editor"]
