# cargo-chef version = 0.1.73
# Rust version = 1.91.1
# Bookworm = Debian 12 -> LTS support until 2028
# Pinned for security & build performance reasons
FROM lukemathwalker/cargo-chef:0.1.73-rust-1.91.1-bookworm@sha256:ac22e8377f914b774d0d3a27cb60ab93b9115d39ef3f72e01f2dfbb55433b3eb AS chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM node:22-bookworm-slim AS pyodide-assets
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci --ignore-scripts --omit=dev

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release --locked --bin code-interpreter-mcp

# Runtime stage - minimal image for the Rust and Pyodide server.
# Pinned for security & performance reasons.
FROM debian:bookworm-slim@sha256:e899040a73d36e2b36fa33216943539d9957cba8172b858097c2cabcdb20a3e2

WORKDIR /app

# hadolint ignore=DL3008
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/code-interpreter-mcp /app/code-interpreter-mcp
COPY --from=pyodide-assets /app/node_modules/pyodide /app/node_modules/pyodide

RUN useradd -m -u 1000 appuser && \
    chown -R appuser:appuser /app

USER appuser

ENV CODE_INTERPRETER_MCP_BIND_ADDR=0.0.0.0:8000
EXPOSE 8000

CMD ["./code-interpreter-mcp"]
