FROM rust:1.98-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
COPY system_prompt.md ./
RUN cargo build --release && strip target/release/mcp-bridge

FROM node:22-bookworm-slim
ARG PLAYWRIGHT_VERSION=1.62.1
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates bash curl && rm -rf /var/lib/apt/lists/* && npm install --global playwright@${PLAYWRIGHT_VERSION} && useradd -r -u 1001 bridge
COPY --from=builder /app/target/release/mcp-bridge /usr/local/bin/bridge
COPY scripts/browser.cjs /usr/local/bin/browser.cjs
USER bridge
ENV MCP_HOST=0.0.0.0
ENV MCP_PORT=3000
ENV NODE_PATH=/usr/local/lib/node_modules
EXPOSE 3000
HEALTHCHECK --interval=30s --timeout=5s --retries=3 CMD curl -q --fail http://127.0.0.1:3000/health || exit 1
CMD ["bridge"]
