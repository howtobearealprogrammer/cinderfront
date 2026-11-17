# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Cinderfront: a LAN-multiplayer RTS (up to 4 players) in Rust on Bevy 0.20. Crate name `cinderfront`.

## Commands

```
cargo run -- solo --windowed        # single player; without --windowed it opens borderless fullscreen
cargo run -- demo --windowed        # sandbox scene (used for visual / perf checks)
cargo run -- host [N]               # host a LAN game; N = auto-start once N players have joined
cargo run -- join <addr>            # join a host
cargo run                           # main menu

cargo test                          # all tests (tests/sim.rs: sim + net, no window needed)
cargo test same_commands_same_state # one test by name
cargo test -- --nocapture           # some tests print map / balance stats
```

Debug environment variables (all read in `src/main.rs`):

- `CF_SHOT=out.png` — take a screenshot after `CF_SHOT_AT` seconds (default 8) and quit; forces windowed. This is the way to check visuals without a human.
- `CF_AUTO_SOLO=secs` — start a solo game from the main menu after that many seconds. Use with `CF_SHOT` to cover the menu → match path, which behaves differently from launching straight into `solo` / `demo`. `CF_FULLSCREEN=1` keeps a screenshot run in fullscreen.
- `CF_RES=WxH` — window size (forces windowed). `CF_ZOOM` — starting zoom, demo only.
- `CF_PERF=1` — log average / worst frame time to stderr every 2 s.
- `CF_REVEAL=1` — no fog of war (pair with `CF_SHOT` and `CF_ZOOM` to see the whole map).
- `CF_AUTOPLAY=1` — scripted orders, to exercise netcode without a human.
- `CF_CURSORS=out.png` — write the cursor sprite sheet and exit, without opening a window.

Models and icons are generated, not hand-edited. Regenerate with Blender (about a minute):

```
/home/dan/blender-5.2.2-linux-x64/blender --background --factory-startup --python tools/blender/build_models.py
```

`tools/blender/README.md` holds the model conventions (axes, origins, required node names `body` / `turret` / `rotor` / `load`, the `Team` material that takes the player colour) and how to add a model.

`dist/cinderfront-linux-x86_64` is a committed single-file release binary; commits that change `src/` refresh it with `cargo build --release && strip -o dist/cinderfront-linux-x86_64 target/release/cinderfront`.

## Architecture

Two crates' worth of code in one package:

- **Library** (`src/lib.rs`): only `sim` and `net`. No Bevy. This is what `tests/sim.rs` exercises.
- **Binary** (`src/main.rs` + `game`, `scene`, `terrain`, `hud`, `cursor`, `gfx`): the Bevy client.

### Deterministic lockstep

`sim.rs` is the whole game. Every peer runs its own `World` and must reach the same state from the same commands, so the sim uses **integer math only and ordered containers only** (`BTreeMap`, no `HashMap`, no floats, no wall-clock, no unseeded randomness). Positions are fixed-point: `TILE = 256` sub-units per tile on a 96x96 map. The only ways to change a `World` are `World::apply(player, &Command)` and `World::tick()` (20 ticks/s). `World::hash()` covers the state that must match; new sim state has to be added to it.

`net.rs` is the transport: newline-delimited JSON over TCP (port 47810), host discovery over UDP broadcast (47811). Clients send `C2H::Cmd`; the host batches everyone's commands into numbered turns and broadcasts `H2C::Turn`.

`Play::frame` in `main.rs` ties them together: a turn is 2 ticks; the host (or solo) cuts a turn every `TURN_DT`; each peer applies a turn's commands and then ticks, never advancing past the turns it holds, and runs at 2x when more than two turns are queued. Every 20 turns clients report `World::hash()` and the host flags a `DESYNC`. Solo games skip the turn queue for the local player's own commands and apply them immediately. A host who exits a match that others are still playing keeps relaying it in `Session::background`.

`Command` is serialised over the wire, so changing it changes the protocol (`MAGIC` in `net.rs` is the version string).

### Client layering

Data flows one way: input → `Game` → `Command` → sim → `Game` → renderers.

- `game.rs` — `Game` owns the `World` plus all client-only state: camera, selection, control groups, fog of war, ghosts of last-seen enemy structures, particles. It never mutates the sim; `Game::frame` returns the `Command`s the player issued. Fog is purely a view filter — the sim knows nothing about it. `Game::context()` decides both the cursor shape and what a right click does, so the two cannot disagree. HUD layout constants and HUD click handling also live here.
- `main.rs` — `run_session` (system set `RunGame`) is the menu / lobby / match state machine and runs before anything draws. The live `Game` sits in the `View(Option<Game>)` resource.
- `scene.rs`, `terrain.rs`, `hud.rs`, `cursor.rs` — read `View` each frame and mirror it into Bevy entities. Nothing here feeds back into the sim. The 3D camera (`scene.rs`) and the UI camera (`hud.rs`) draw to the same window and must have the same `Msaa` setting; if they differ, the UI camera gets its own never-cleared texture and paints stale menu / sidebar frames over the map. These write components only when values change (Bevy re-lays-out UI and re-propagates transforms otherwise); keep that pattern when adding HUD or scene sync code.

### Terrain height

`World::level` holds a height level (0–2) per tile, fixed by `gen_map`. Levels only change across `Bank` tiles (impassable, unbuildable) or `Ramp` tiles (walkable; only walls build there), so pathfinding has no height rule of its own; the `levels_only_change_at_ramps` test guards that invariant. `World::height_bonus` gives +1 tile of weapon range per level the shooter stands above its target (no bonus involving aircraft); sight gets +1 per level in `Game::update_vis`. On the client, `corner_heights` / `ground_h` (`game.rs`) turn levels into a height field used by the terrain mesh, by `Game::wpos` / `on_ground` for placing models and effects, and by `proj` / `ground_at` so picking matches what is drawn. Anything new drawn in the world should go through those, not a hard-coded y.

### Units of measure

Three coordinate systems are in play:

- Sim: fixed-point, 256 per tile (`Pos`), or whole tiles (`Tile`).
- Client effects and legacy unit shapes: "ground pixels", `PX = 40` per tile, heights in the same unit.
- Render world: 1 world unit = 1 tile (`scene.rs` divides by `PX`). Models are also 1 Blender unit = 1 tile.

### Assets

`build.rs` walks `assets/` and bakes every `.glb`, `.png` and `.ttf` into the binary; `main.rs` registers them with Bevy's embedded source, so asset paths are `embedded://models/tank.glb`, not file paths. A new asset file needs no registration beyond existing in `assets/`, but a new unit or structure kind also needs its file name in `unit_file` / `struct_file` in `scene.rs`.

Terrain chunks (8x8 tiles) bake their texture and mesh on worker threads and re-bake when a chunk's tile signature changes; cliffs are real geometry, fog goes into vertex colours.
