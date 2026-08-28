FROM rust:bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
COPY system_prompt.md ./
RUN cargo build --release && strip target/release/opencode-mcp-bridge-rust

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates bash curl && rm -rf /var/lib/apt/lists/* && useradd -r -u 1001 bridge
COPY --from=builder /app/target/release/opencode-mcp-bridge-rust /usr/local/bin/bridge
USER bridge
ENV MCP_PORT=3000
EXPOSE 3000
HEALTHCHECK --interval=30s --timeout=5s --retries=3 CMD curl -q --fail http://127.0.0.1:3000/health || exit 1
CMD ["bridge"]
