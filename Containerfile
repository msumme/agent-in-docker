# Single-stage: no bridge build needed (bridge runs on host)
FROM node:22-alpine

# Install packages
RUN apk add --no-cache bash curl python3 git

# Install Claude Code CLI
RUN npm install -g @anthropic-ai/claude-code

# Install beads (bd)
RUN apk add --no-cache --virtual .bd-deps go && \
    GOBIN=/usr/local/bin go install github.com/steveyegge/beads/cmd/bd@latest && \
    apk del .bd-deps

# Workspace
RUN mkdir -p /workspace && chown node:node /workspace

# Copy entrypoint
COPY scripts/entrypoint.sh /opt/entrypoint.sh
RUN chmod +x /opt/entrypoint.sh

WORKDIR /workspace

ENTRYPOINT ["/opt/entrypoint.sh"]
