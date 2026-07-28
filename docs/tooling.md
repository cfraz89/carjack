# Tooling notes

Things about the build/run harness that cost time to work out. The commands themselves are
documented in `CLAUDE.md`; this is the "why is it like that" layer.

## Worktrees start without a `.env` (2026-07-27)

`.env` is gitignored, so a fresh `git worktree` has none - and **every** setting that matters at
runtime lives there: `CARJACK.SOURCE`, `CARJACK.BLUETOOTH.DEVICE`, `RUST_LOG`, `TOOLBOX_BUILD`.
Running from a worktree without copying it silently gets you defaults (`source = "app"`, no
Bluetooth target, so carjack errors out at startup) rather than the setup you meant to test. Copy
it in as the first step in any worktree: `cp ../../../.env .`

Keep logs too - `just test-run` only writes to the terminal, so redirect both streams if the run
is to be diagnosable afterwards. Two captures were analysed blind for want of this, and one of the
two diagnoses was wrong.

## `just test-run` (2026-07-26)

- **`just test-run` needs `timeout --foreground`.** Without it `timeout` puts the command in a new
  process group; a terminal only sends SIGINT to its *foreground* group, so Ctrl-C reached the
  script but never carjack - it ran on (spewing `RUST_LOG=trace`) until the timeout expired, which
  reads as a wedged terminal. With the flag, measured **52 ms** from SIGINT to exit mid-desktop-
  stream. Cost: on expiry `timeout` signals only its direct child (cargo), so the `pkill` in the
  `cleanup` trap is what reaps carjack.
- `test-run` now takes `build` as a recipe dependency, keeping the compile - and `_setcap`'s `run0`
  polkit escalation - out of the timed section.
- `_setcap` skips `run0` entirely when `getcap` already shows `cap_net_admin` (relinking drops the
  cap, so this is an exact test). Previously it prompted on every build and failed outright in a
  non-interactive shell.
- The `cleanup` `pkill` pattern is now **anchored** (`^target/(debug|release)/carjack$`). Unanchored
  it matched any process whose command line merely contained that path - it killed a wrapper script
  driving the recipe, which masqueraded as carjack hanging on Ctrl-C.
- `main.rs` has a SIGINT watchdog (`spawn_sigint_watchdog`): a second Ctrl-C force-exits with 130.
  Not what was broken above, but a backstop - once anything awaits `tokio::signal::ctrl_c()`, the
  default SIGINT disposition is gone process-wide, so a stalled graceful path means Ctrl-C does
  nothing at all.
