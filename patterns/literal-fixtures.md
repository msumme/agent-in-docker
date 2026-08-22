# Literal fixtures

**Principle:** a test states only what it asserts. Filler required to
construct a value belongs in one shared default, not copied per test.

## Anti-pattern

```rust
// near-identical 16-field literals in three different crates
fn make_payload(name: &str) -> StartAgentPayload {
    StartAgentPayload {
        name: name.into(), role: "r".into(), mode: "oneshot".into(),
        project_path: "/tmp".into(), prompt: "hi".into(),
        agent_dir: "/a".into(), role_prompt: String::new(),
        seed_credentials: "/creds".into(), image_name: "img".into(),
        network_name: "net".into(), orchestrator_port: 1,
        mcp_port: 2, dolt_port: None, extra_mounts: vec![],
        model: None, effort: None, resume_session: false,
    }
}
```

Why it's wrong: removing one field touched fixtures in three crates. Worse,
the reader can't tell which of the 16 values the test actually depends on —
the signal is buried in filler. Fixture duplication also quietly diverges:
each copy drifts its own defaults, and a test passes or fails on filler it
never meant to assert.

## Correct

The filler is test data, so it lives with the tests: one fixture default
per test module (a `const` where the fields allow it, a plain fn
otherwise), named for the suite it serves. Each test overrides only what
it's about:

```rust
#[cfg(test)]
mod tests {
    // the default belongs to these tests, not to the type
    fn test_launch_default() -> ResolvedLaunch {
        ResolvedLaunch { /* benign placeholders, defined once, here */ }
    }

    #[test]
    fn mounts_agent_dir() {
        let launch = ResolvedLaunch {
            agent_dir: "/tmp/agent".into(),
            ..test_launch_default()
        };
        assert!(container_run_args(&launch).iter()
            .any(|a| a == "/tmp/agent:/root/.claude:Z"));
    }
}
```

Now the test reads as its own specification: this test is about
`agent_dir`, nothing else — and a field change touches one fixture item
per test module instead of every literal.

Two things this is deliberately NOT:

- **Not `impl Default`.** `Default` is production API — implementing it
  with test placeholders lets production code construct half-real values
  (`port: 0`, empty paths) that compile fine and fail at runtime.
- **Not a method on the production type at all** — not even
  `#[cfg(test)] impl`. Test filler hanging off the type's namespace is
  test concerns leaking into production code's home. The fixture is owned
  by the tests it serves; different suites may legitimately want different
  defaults.

## Exceptions

- **Small types.** A 3-field struct written out literally is clearer than a
  builder. The smell needs size and repetition.
- **Shape tests.** A test *about* construction or serde format (roundtrip,
  raw-JSON parsing) legitimately spells out every field — every field is the
  point.
- **When no benign placeholder exists.** If a field must be semantically
  valid for any test to make sense, don't invent filler for it — use a named
  builder with required arguments for exactly those fields
  (`test_launch(agent_dir: &str)`), defaults for the rest.

## Review cues

- The same struct literal (± two fields) in more than one test module.
- Fixtures with obviously-arbitrary values (`port: 1`, `"/tmp"`) — filler
  that Default should own.
- A field change whose diff is mostly test-fixture churn.
- Helper fns named `make_*`/`sample_*` duplicated across crates.
