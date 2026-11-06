# Chunky-rs

The [Chunky](https://github.com/pop4959/Chunky) chunk pregenerator, ported to
[Horizon](https://playhorizon.dev) as a native mod.

Horizon's engine already generates, lights and saves chunks ahead of the
players — its own `/pregen` command runs a band sweep over a square or a
circle. This mod ports Chunky's surface onto that engine: the `/chunky`
command set, the selection that survives the world closing, and the progress
reports.

## Install

```
horizon-launcher native-build mixins/chunky --trust-native-code
```

Native mods run with your account's permissions; the launcher warns before
it builds one. First build takes a while (it compiles the game); rebuilds
are incremental. To check the mod against Horizon's source instead:

```
python3 native/tools/native.py check .   # from a horizon-mods checkout
```

## Commands

| Command | What it does |
| --- | --- |
| `/chunky start` | Pregenerate the selection (the source's position, else the spawn) |
| `/chunky pause` / `/chunky continue` | Hold or carry on |
| `/chunky cancel` | Stop and forget the task |
| `/chunky status` / `/chunky selection` | Progress and settings |
| `/chunky center <x> <z>` | Set the centre, in blocks |
| `/chunky radius <blocks>` | Set the radius, in blocks |
| `/chunky shape <square\|circle>` | Set the shape |
| `/chunky spawn` | Centre at the world's spawn |
| `/chunky quiet` | Toggle the periodic progress reports |

All of them need operator level 2. The console runs them on a dedicated
server too. The progress goes to the log and to whoever started the task,
every five seconds; a finished task reports in green.

A running task is saved to `chunky/chunky.json` in the world's folder. When
the world opens again, the task continues where it was — pause included —
and the world's backups carry the file.

## Threads and speed

The chunks are generated, decorated, lit and saved by Horizon's own worker
pool (one worker per core less the server thread). The driver hands the
sweep's band to that pool each level tick and pumps it — before and after
the pass — so the workers idle as little as the engine's in-flight caps
(`workers × 3` generating, `workers × 2` decorating) allow. On a 16-core
machine a task runs at roughly 400 fresh chunks a second on about half the
workers' duty cycle; chunks already on disk load several times faster.

Two things make it look idle when it is not:

- A paused world does no work. With *Pause on Lost Focus* on (Options),
  tabbing out pauses the game and the task with it.
- The rate in the progress line is the current one — what was done since
  the last report — not the run's average.

## What carries over from Chunky, and what does not

- The engine is Horizon's band pregeneration (square and circle), not
  Chunky's worker pool; it is the same one `/pregen` runs, with Chunky's
  scheduling of strips and rows on top.
- Tasks run in the dimension they were started in. A Nether or End task on
  an empty server needs someone in that dimension (Horizon only ticks
  dimensions with players, forced chunks or a `/pregen` of its own).
- `pattern`, `worldborder`, `corners` and `world` are not ported yet.

## Layout

```
mixins/chunky/
├── horizon-mixin.toml   # the manifest: modules and hook points
├── runtime.rs           # sweep engine, state, parser, persistence (+ tests)
├── glue.rs              # the edges to the game (commands, driver, lifecycle)
├── register.rs          # native_register_commands
├── command.rs           # native_perform_command (players)
├── console.rs           # native_perform_console_command (the console)
├── level_tick.rs        # native_level_tick_pre (the driver)
├── lifecycle.rs         # native_world_lifecycle (resume and save)
└── list.rs              # the row in Options > Mods
```

`runtime.rs` is the crate's own lib: it needs no game code, and `cargo test`
runs its tests anywhere. `glue.rs` and the helpers only compile inside the
game.

Licence: MIT, as Chunky's and Horizon's.
