FROM rust:1.88-bookworm AS builder
ARG BUILD_COMMIT=unknown
ARG BUILD_DIRTY=false
ENV MCP_BUILD_COMMIT_OVERRIDE=${BUILD_COMMIT}
ENV MCP_BUILD_DIRTY_OVERRIDE=${BUILD_DIRTY}
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY build.rs ./
COPY src ./src
COPY system_prompt.md ./
RUN cargo build --release --locked && strip target/release/mcp-bridge

FROM node:22-bookworm-slim
ARG PLAYWRIGHT_VERSION=1.62.1
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates bash curl \
    && rm -rf /var/lib/apt/lists/* \
    && npm install --global playwright@${PLAYWRIGHT_VERSION} \
    && useradd --create-home --uid 1001 --shell /usr/sbin/nologin bridge \
    && mkdir -p /work /state \
    && chown bridge:bridge /work /state
COPY --from=builder /app/target/release/mcp-bridge /usr/local/bin/bridge
COPY scripts/browser.cjs /usr/local/bin/browser.cjs
USER bridge
WORKDIR /work
ENV MCP_PROFILE=server-secure
ENV MCP_HOST=0.0.0.0
ENV MCP_PORT=3000
ENV BRIDGE_WORKDIR=/work
ENV MCP_STATE_FILE=/state/state.json
ENV NODE_PATH=/usr/local/lib/node_modules
EXPOSE 3000
HEALTHCHECK --interval=30s --timeout=5s --retries=3 CMD curl -q --fail http://127.0.0.1:3000/ready || exit 1
CMD ["bridge"]
