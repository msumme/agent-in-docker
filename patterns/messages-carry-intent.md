# Messages carry intent

**Principle:** a request contains only what the caller decides; anything the
receiver can derive from its own config is resolved once, receiver-side.

## Anti-pattern

Every sender recomputes projections of shared config and ships them in the
message:

```rust
// three call sites, each doing this independently
let payload = StartAgentPayload {
    name, role, prompt,
    agent_dir,
    seed_credentials: cfg.seed_dir.join(".credentials.json")
        .to_string_lossy().to_string(),
    image_name: cfg.image_name.clone(),
    network_name: cfg.network_name.clone(),
    orchestrator_port: cfg.orchestrator_port,
    // ...
};
```

Why it's wrong: the derivation is duplicated at every sender. Adding a call
site copies it again; changing it means finding every copy — and one gets
missed. (It happened here: a field-removal spec listed the known
construction sites and missed a third one in `server.rs`.) The receiver also
ends up trusting whatever paths arrive instead of owning its own policy.

## Correct

The wire message is the caller's decision, nothing more; one resolver turns
it into a runnable thing:

```rust
pub struct AgentSpec {          // crosses the wire
    pub name: String,
    pub role: String,
    pub mode: Mode,
    pub prompt: String,
}

pub fn resolve_launch(cfg: &ProjectConfig, spec: AgentSpec)
    -> Result<ResolvedLaunch>   // never crosses the wire
```

Test: for each field of a message, ask "could the receiver compute this from
what it already knows?" If yes, delete the field and compute it in the
resolver.

## Exceptions

- **True external boundaries.** If the receiver genuinely lacks the config
  (a third-party API, a service with no shared filesystem), the values must
  travel. This pattern is about senders and receivers that share a config
  source.
- **Capability/token passing.** A value that must be *granted* rather than
  derived (a signed URL, a one-time credential) belongs in the message even
  if it looks derivable.
- **Deliberate denormalization.** Caching a derived value with a single
  writer and an explicit invalidation story is a performance decision, not
  this smell — but it should say so in a comment.

## Review cues

- A `cfg.something.join(...)`/`cfg.x.clone()` cluster right before
  constructing a request struct — especially the same cluster in more than
  one file.
- Message fields whose doc comment explains how to compute them.
- A receiver that uses a transmitted path/port without ever consulting its
  own config for it.
