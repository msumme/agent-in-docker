---
name: Devcontainer inversion epic
description: Architectural shift from custom container to project devcontainers + bind-mounted tools bundle (epic agent-in-docker-jmw)
type: project
---

Major architecture change planned (epic agent-in-docker-jmw, 11 tickets): invert from "our container + project code" to "project's devcontainer + our tools bind-mounted."

**Why:** Projects already have devcontainers. Our variant system (python-data, rust-dev, minimal) can't anticipate every project's needs.

**How to apply:** The tools bundle (claude, bd, dolt, entrypoint) lives at tools/ on host, mounted at /opt/agent-tools:ro. CLI uses `devcontainer build` to get image, `podman run` with mounts + entrypoint override. A fallback Containerfile.fallback exists for projects without devcontainers -- claude self-provisions what it needs at runtime.
