# Phase 2B — `Arc`-based `HostCtx` (drop the `'a` lifetime)

**Status:** plan (not started). Follows Phase 1 (`#[async_trait]`, commit `807357d`).
On branch `feature/plugin-async-trait`.

## Goal

Phase 1 removed the hand-rolled `BoxFuture<'a,'h:'a>` + `Box::pin` boilerplate,
but every async hook still carries `&mut HostCtx<'_>` — a **two-lifetime** param
(the `&mut` borrow *and* the `HostCtx`'s inner `'a`). Under `#[async_trait]` that
desugars to `&'l1 mut HostCtx<'l2>` (a distinct inner lifetime) per hook.

2B makes `HostCtx` **own its long-lived handles** instead of borrowing them, so
the struct loses its lifetime parameter entirely. The hook param becomes the
idiomatic single-lifetime `&mut HostCtx`, and the `HostCtx` no longer borrows
from `&AppState` (it can outlive the request-scoped borrow).

Object-safety is **preserved** — `dyn Plugin`, the `Box<dyn Plugin>` registry,
and `Send + Sync` are untouched. (This is the option that keeps the WASM /
dynamic-plugin door open, unlike static-dispatch 2C.)

## Why it's feasible here (evidence)

The three fields that today drive the `'a` borrow are all long-lived and
shareable:

| Field | Today | Target | Why OK |
|---|---|---|---|
| `host: &'a PluginHost` | borrowed | `Arc<PluginHost>` | `AppState.plugins` is **already `Arc<PluginHost>`** (lib.rs:69). `PluginHost: Send + Sync` (all fields owned/`dyn Plugin: Send+Sync`/`Send+Sync` closures). `&mut PluginHost` is needed **only** in `init`/`run_init` (startup), never at dispatch → shared `Arc` is safe. |
| `pool: &'a PgPool` | borrowed | `PgPool` (owned) | `PgPool: Clone` is cheap (Arc-internally). `Db::pool() -> &PgPool` (db.rs:92) → `.clone()` at the call site. |
| `cfg: &'a Config` | borrowed | `Arc<Config>` | `Config` is all-owned (`Send + Sync + 'static`). `AppState.config: Config` → becomes `Arc<Config>` (deref keeps `state.config.*` working). |

## The core type change

```rust
// before
pub struct HostCtx<'a> {
    host: &'a PluginHost,
    pub pool: &'a PgPool,
    pub cfg: &'a Config,
    /* + owned fields: active, owner_uid, profile, access_level, storage, … */
}
// after
pub struct HostCtx {
    host: Arc<PluginHost>,
    pub pool: PgPool,
    pub cfg: Arc<Config>,
    /* owned fields unchanged */
}
```

Hook param, trait + every impl:
```rust
// before (Phase 1)
async fn hook_article_button(&self, host: &mut HostCtx<'_>, line: &Value) -> String { … }
// after
async fn hook_article_button(&self, host: &mut HostCtx, line: &Value) -> String { … }
```

`host.rs` dispatch bounds:
```rust
// before
for<'x> F: FnMut(&'x dyn Plugin, &'x mut HostCtx<'a>) -> BoxFuture<'x, ()>,
// after
for<'x> F: FnMut(&'x dyn Plugin, &'x mut HostCtx) -> BoxFuture<'x, ()>,
```

Dispatch helper fns (`dispatch.rs`) also shed their `'a`:
```rust
// before
pub(crate) fn login_ctx<'a>(state: &'a AppState) -> HostCtx<'a> { … }
// after
pub(crate) fn login_ctx(state: &AppState) -> HostCtx { … }
```
(same for `session_ctx`, `owner_ctx`, and the `host/pool/cfg` helper at
`dispatch.rs:141`.)

## Scope / ripple (measured)

- **`HostCtx<'…>` mentions: 133** → `HostCtx`. Breakdown: `plugin.rs` 56 (the
  trait), `share.rs` 11, `nsfw.rs` 9, `host.rs` 8, the rest 1–6 each across
  plugins + `dispatch.rs` + a few core files.
- **Struct + `with_active`** (`host.rs:480`, `storage.rs:111`): 3 field types.
- **5 constructors** (`empty`, `for_user`, `system_plugins`, `system_only`,
  `with_active`): params `&'a X` → `Arc<X>` / `PgPool`.
- **Call sites** (production `dispatch.rs` + tests): `&state.plugins` →
  `Arc::clone(&state.plugins)`, `state.db.pool()` → `state.db.pool().clone()`,
  `&state.config` → `Arc::clone(&state.config)`. Test setups wrap their local
  `Config`/`PluginHost` in `Arc::new(…)` once.
- **`host.pool` access: ~20 sites** in plugins (`.execute(host.pool)`,
  `.fetch_optional(host.pool)`, `get_tags(host.pool, …)`) → `&host.pool`
  (owned field now; pass a ref). See "Open decisions" for the accessor option.
- **`host.cfg` access: ~5 sites** → `&host.cfg` (or keep deref for field reads).
- **`AppState.config: Config` → `Arc<Config>`** (lib.rs:66): low ripple — `Arc`
  derefs for every `state.config.*` read; only `AppState::new` wraps it.
- **`host.host`** is not read by plugins (only internally by `host.rs`), so the
  `Arc<PluginHost>` change has near-zero plugin-side ripple there.

## Phasing

The lifetime removal is global (the whole change must land together to compile),
so verify with `cargo check` at the end of each *logical* step and fix
iteratively. Recommended order:

1. **`PluginHost: Sync` guard** — add `const _: () = [assert_sync::<PluginHost>()];`
   (or just let the `Arc<PluginHost>` field force it) to make the assumption
   explicit and fail fast if a field regresses.
2. **Struct + `with_active`** (`host.rs`, `storage.rs`) — drop the `'a`, change
   the 3 fields. (Won't compile until the rest lands.)
3. **Constructors** (`host.rs`) — `empty`, `for_user`, `system_plugins`,
   `system_only`, `with_active` → new param types.
4. **`host.rs` internals** — `run_hooks`/`run_hooks_until`/`run_hooks_fold`
   bounds, `let host = self.host;` → `Arc::clone`, any `self.host`/`self.pool`/
   `self.cfg` uses.
5. **Trait + plugins** — `&mut HostCtx<'_>` → `&mut HostCtx` (the 133 mentions);
   `host.pool` → `&host.pool`; `host.cfg` → `&host.cfg`.
6. **`AppState` + call sites** — `config: Arc<Config>`; update `dispatch.rs`
   helpers + all production/test constructor calls; drop the helper fns' `'a`.
7. **Verify** — `cargo check -p ttrss --all-targets` (target: 0 errors, 0
   warnings); full `cargo test -p ttrss` (green). Then a quick
   `rustc -Zunpretty=expanded` (or eyeball one hook) to confirm the desugared
   signature dropped the inner lifetime.

## Open decisions

1. **`host.pool` accessor** — (a) change ~20 sites to `&host.pool` (mechanical,
   keeps the field `pub`), vs (b) make `pool` private + add `fn pool(&self) ->
   &PgPool` and use `host.pool()` (clearer intent, but renames all sites). Lean
   (a) for minimal diff; revisit if (b) reads better.
2. **`AppState.config`** — make it `Arc<Config>` (chosen; low ripple) vs wrap
   per-call (`Arc::new(state.config.clone())` — wasteful, rejected).
3. **Bonus: also migrate `ComicFilter`** (af_comics) to `#[async_trait]`? It's
   object-safe + `Vec<Box<dyn ComicFilter>>`-used (af_comics.rs:86), same
   category as `Plugin`. Out of scope for 2B (different trait) but a natural
   companion that would clear the last non-`host.rs` `BoxFuture` uses.
4. **`TestPlugin` mock** (host.rs) — stays manual (free-standing, its
   `BoxFuture` hooks already match the `#[async_trait]` sig). No change needed.

## Verification bar

- `cargo check -p ttrss --all-targets` → 0 errors, 0 warnings.
- Full `cargo test -p ttrss` → green (1427 today).
- Desugared signature of one hook (e.g. `hook_article_button`) shows no inner
  `HostCtx` lifetime.

## Risk

- **Big-bang compile** — the lifetime removal touches all files at once; no
  intermediate step compiles standalone. Mitigate by editing in the phased order
  above and letting `cargo check` drive the fix loop at the end.
- **`PluginHost: Sync`** is the linchpin — if any field were non-`Sync`, the
  `Arc<PluginHost>` field would fail. Step 1 makes that explicit.
- **`&host.pool` ripple** is mechanical but wide (~20 sites); easy to miss one
  (compile will catch it).

## What it does NOT change

- Object-safety: `dyn Plugin`, `Box<dyn Plugin>`, `Send + Sync` all preserved.
- The `BoxFuture` alias + its legitimate non-trait uses (`dispatch_*` bounds,
  the `Box<dyn Fn…>` scheduled-task callback, `TestPlugin`) — see the Phase 1
  notes / Decision 1.
