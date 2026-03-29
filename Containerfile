# Stage 1: Build the TypeScript bridge
FROM node:22-slim AS builder
WORKDIR /build
COPY bridge/package.json bridge/package-lock.json ./
RUN npm ci
COPY bridge/ ./
RUN npm run build

# Stage 2: Runtime
FROM node:22-slim

# Install curl (for task queue polling in long-running mode)
RUN apt-get update && apt-get install -y --no-install-recommends curl && rm -rf /var/lib/apt/lists/*
RUN npm install -g @anthropic-ai/claude-code

# Non-root user (node:22-slim already has uid 1000 as 'node', reuse it)
RUN mkdir -p /workspace && chown node:node /workspace

# Copy built bridge
COPY --from=builder /build/dist /opt/bridge/dist
COPY --from=builder /build/node_modules /opt/bridge/node_modules
COPY --from=builder /build/package.json /opt/bridge/package.json

# Copy entrypoint
COPY scripts/entrypoint.sh /opt/entrypoint.sh
RUN chmod +x /opt/entrypoint.sh

WORKDIR /workspace

ENTRYPOINT ["/opt/entrypoint.sh"]
