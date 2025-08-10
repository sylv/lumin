# Stage 1: Prepare for Backend Build with Cargo Chef
FROM docker.io/lukemathwalker/cargo-chef:latest-rust-1-alpine AS chef
WORKDIR /app

# Install build dependencies needed by chef cook and the final build
RUN apk add --no-cache musl-dev gcc make libc-dev
RUN cargo install sqlx-cli --no-default-features --features sqlite

FROM chef AS planner
WORKDIR /app
# Copy everything needed for planning
COPY . .
# Compute dependencies
RUN cargo chef prepare --recipe-path recipe.json





# Stage 2: Build Backend Dependencies and Application
FROM chef AS builder
WORKDIR /app
# Copy the dependency recipe
COPY --from=planner /app/recipe.json recipe.json
# Build dependencies based on the recipe
# Pass necessary target and features for dependencies
RUN cargo chef cook --release --recipe-path recipe.json --target x86_64-unknown-linux-musl

# Copy application code
COPY . .

# sqlx needs database info to typecheck properly.
ENV DATABASE_URL=sqlite:///app/db.sqlite
RUN sqlx database create && sqlx migrate run

# Build the application, linking against the pre-built dependencies.
RUN cargo build --release --target x86_64-unknown-linux-musl --bin lumin --locked





# Stage 3: Final Runtime Image
FROM docker.io/alpine:latest
WORKDIR /app

# for fuse
RUN apk add --no-cache fuse3 curl
RUN sed -i 's/#user_allow_other/user_allow_other/' /etc/fuse.conf

# Copy backend build from Stage 3 (builder)
COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/lumin ./lumin

ENV LUMIN_DATA_DIR=/data
ENV LUMIN_MOUNT_PATH=/mnt/lumin
env LUMIN_HOST=0.0.0.0

EXPOSE 8000
CMD ["/app/lumin"]
