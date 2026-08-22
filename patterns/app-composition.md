# App composition

**Principle:** the app assembles once, at a composition root, with every
dependency injected. Non-determinism (time, randomness, network, disk,
process spawning) never appears in app logic — only behind traits, with the
real implementations constructed in `main` and fakes constructed in tests.
The result: the entire app runs as a real vertical slice in tests, with
zero flakiness by construction.

This is the base example. It is deliberately small; the shape scales.

## The app: logic with injected capabilities

```rust
// Capabilities: traits exist ONLY at non-deterministic / external
// boundaries. Pure logic you own does not get a trait or a mock.
pub trait Clock {
    fn now(&self) -> Timestamp;
}
pub trait Store {
    fn save(&mut self, r: Reminder) -> Result<Id>;
    fn due(&self, at: Timestamp) -> Vec<Reminder>;
    fn mark_sent(&mut self, id: Id) -> Result<()>;
}
pub trait Notifier {
    fn notify(&mut self, r: &Reminder) -> Result<()>;
}

pub struct App<C, S, N> {
    clock: C,
    store: S,
    notifier: N,
}

impl<C: Clock, S: Store, N: Notifier> App<C, S, N> {
    pub fn new(clock: C, store: S, notifier: N) -> Self {
        Self { clock, store, notifier }
    }

    pub fn schedule(&mut self, text: &str, delay: Duration) -> Result<Id> {
        let due = self.clock.now() + delay;   // time from the injected clock
        self.store.save(Reminder::pending(text, due))
    }

    pub fn tick(&mut self) -> Result<usize> {
        let now = self.clock.now();
        let due = self.store.due(now);
        for r in &due {
            self.notifier.notify(r)?;
            self.store.mark_sent(r.id)?;
        }
        Ok(due.len())
    }
}
```

Notes:

- `App` contains no `SystemTime::now()`, no `thread_rng()`, no file paths,
  no network. It cannot be non-deterministic; the type system won't let it.
- Generics keep it zero-cost; `Box<dyn Clock>` is equally fine at app
  scale. Don't debate it in review — either passes.
- Per-call values may also arrive as arguments (`fn tick_at(&mut self,
  now: Timestamp)`); see `inject-at-construction.md`. Don't do both — a
  unit holding a clock doesn't also take `now` parameters.

## The composition root: the only place reality is constructed

```rust
// main.rs — wiring happens here, once. Nothing else constructs
// real infrastructure, reads env/config, or touches the ambient world.
fn main() -> Result<()> {
    let cfg = Config::load()?;                       // raw config: root-only
    let mut app = App::new(
        SystemClock,
        SqliteStore::open(&cfg.db_path)?,            // derivations from cfg
        DesktopNotifier::new(&cfg.notify_socket)?,   //   happen here, once
    );
    run_loop(&mut app, cfg.tick_interval)
}
```

## The tests: the real app, fake edges, no non-determinism

Fakes are hand-written, dumb, and in-memory. They implement the same trait
the real infra does — no mocking framework, no expectations on calls
(assert on observable outcomes, not on which method was invoked).

```rust
#[cfg(test)]
mod tests {
    // fixture filler lives with the tests — see literal-fixtures.md

    #[derive(Clone, Default)]
    struct FakeClock(Rc<Cell<Timestamp>>);
    impl FakeClock {
        fn advance(&self, d: Duration) { self.0.set(self.0.get() + d); }
    }
    impl Clock for FakeClock {
        fn now(&self) -> Timestamp { self.0.get() }
    }

    #[derive(Default)]
    struct InMemStore(Vec<Reminder>);      // real trait, trivial storage
    #[derive(Default)]
    struct RecordingNotifier(Vec<String>); // records what the user would see

    fn test_app() -> (FakeClock, App<FakeClock, InMemStore, RecordingNotifier>) {
        let clock = FakeClock::default();
        let app = App::new(clock.clone(), InMemStore::default(),
                           RecordingNotifier::default());
        (clock, app)
    }

    #[test]
    fn reminder_fires_exactly_once_when_due() {
        let (clock, mut app) = test_app();
        app.schedule("stand up", Duration::from_secs(60)).unwrap();

        assert_eq!(app.tick().unwrap(), 0);          // not due yet
        clock.advance(Duration::from_secs(60));
        assert_eq!(app.tick().unwrap(), 1);          // fires at due time
        assert_eq!(app.tick().unwrap(), 0);          // and only once
        assert_eq!(app.notifier.0, ["stand up"]);    // what the user saw
    }

    #[test]
    fn notifier_failure_leaves_reminder_pending() {
        // swap in a FailingNotifier; assert the reminder is still due on
        // the next tick — error paths are just another deterministic test.
    }
}
```

What makes this "deep integration, not unit": the test assembles the *real*
`App` with its real logic — scheduling, due-computation, mark-sent — and
fakes only the three external boundaries. Nothing is mocked that we own;
nothing can flake; every failure reproduces.

## Exceptions

- **Binaries that are all edge.** A tool that is one subprocess call with
  arg formatting has nothing meaningful to inject; test the pure
  arg-formatting function and smoke-test the rest manually.
- **Trait-per-dependency is for non-determinism and external systems, not
  everything.** Internal pure helpers are called directly. A trait with one
  impl and no non-determinism behind it is speculative generality
  (see `leaky-fields.md` for the closed-set/enum discussion).
- **Constructor bloat.** If `App::new` grows past ~5 dependencies, that's
  the kitchen-drawer smell (`kitchen-drawer-config.md`) applied to wiring —
  group capabilities into cohesive sub-structs owned by sub-domains.

## Review cues

- `SystemTime::now()`, `thread_rng()`, `std::env`, file/network I/O
  anywhere except `main`/composition root or a capability impl.
- App logic constructing its own dependencies (`new` calls to infra inside
  business code).
- Tests that mock owned logic, or assert "method X was called" instead of
  an observable outcome.
- A test that needs a real database/network/sleep to pass.
- `#[ignore]`-d or retried-on-flake tests — flakiness is a design defect,
  the dependency causing it isn't injected.
