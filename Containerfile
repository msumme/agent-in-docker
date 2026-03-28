# Stage 1: Build the TypeScript bridge
FROM node:22-slim AS builder
WORKDIR /build
COPY bridge/package.json bridge/package-lock.json ./
RUN npm ci
COPY bridge/ ./
RUN npm run build

# Stage 2: Runtime
FROM node:22-slim

# Install Claude Code CLI
RUN npm install -g @anthropic-ai/claude-code

# Non-root user
RUN useradd -m -u 1000 -s /bin/bash agent \
    && mkdir -p /workspace && chown agent:agent /workspace

# Copy built bridge
COPY --from=builder /build/dist /opt/bridge/dist
COPY --from=builder /build/node_modules /opt/bridge/node_modules
COPY --from=builder /build/package.json /opt/bridge/package.json

# Copy entrypoint
COPY scripts/entrypoint.sh /opt/entrypoint.sh
RUN chmod +x /opt/entrypoint.sh

USER agent
WORKDIR /workspace

ENTRYPOINT ["/opt/entrypoint.sh"]
