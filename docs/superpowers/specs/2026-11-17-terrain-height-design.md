# Terrain height and buildable-ground overlay

## Intent

Two features for Cinderfront, in the spirit of Red Alert / C&C:

1. **Buildable ground is visible when it matters.** While a building is on the cursor the player sees every tile they can place on, not just the footprint under the mouse.
2. **Terrain height is a gameplay factor.** High ground shoots and sees a little further, and height (banks with ramps between levels) controls where ground units can go, creating chokepoints worth holding.

## Goals

- Three height levels (0 low, 1 middle, 2 high) that the sim knows about.
- Level changes are only crossable by ground units at ramps; elsewhere a ring of impassable, unbuildable bank tiles separates levels.
- Shooter higher than target: +1 tile range per level of difference (units and turrets). Viewer: +1 tile sight per level it stands on.
- Map: each base on a middle-level home plateau with 2 ramps down; ore fields on low ground; a high plateau in the map centre. Generated, seeded, 4-way mirrored like today.
- Terrain renders the levels as raised ground with sloped rocky banks; units and buildings sit at their ground height.
- Placement overlay: while placing, a green grid over every tile where the held building could currently be placed (rock, within build range, free); everything else dimmed.

## Non-goals

- Low ground blind to high ground (StarCraft-style vision). Low ground can see and shoot up normally. Could be layered on later.
- Range penalty for shooting uphill, any bonus against aircraft, banks blocking shots or vision.
- Hand-authored maps or a map editor.
- Changing what counts as buildable (still `Rock` only, on any level).

## Anti-goals

- Anything that breaks lockstep determinism: height logic in the sim is integer-only and part of the generated map (same seed → same levels on every peer).
- Chokepoints the generator can't guarantee: every level boundary must be bank or ramp, no accidental walk-up spots.
- A cluttered normal view: the build grid shows only while placing.

## Constraints

- `sim.rs` rules: integer math, ordered containers, state changes only via `apply` / `tick`.
- Map is generated from a seed and mirrored into 4 quadrants (`gen_map`); the start-to-centre connectivity check must keep holding.
- Fog of war / sight is client-side (`Game::update_vis`), so the sight bonus does not touch the sim.
- Terrain renderer is a per-chunk heightfield (`terrain.rs`, `SUB = 4` vertices per tile) with baked textures; existing `Cliff` outcrops are already raised geometry.

## Approaches considered

- **Level grid + Bank / Ramp terrain types, generator extended with rules (chosen).** Each tile gets a level 0–2; `Terrain` gains `Bank` (not walkable, not buildable) and `Ramp` (walkable, not buildable). Because the generator guarantees banks or ramps at every level change, `find_path` and `walkable` need no new rule; units route through ramps on their own. Keeps seeded maps.
- **Same representation, hand-drawn 48×48 quadrant baked in.** Exact control of chokepoints, but one map only. Rejected by Dan in favour of the generator.
- **Continuous heightmap with slope-based passability.** Rejected: the generator can't promise where chokepoints are, and controlling movement is the point.

## Decided

- Height effects: range and sight bonus as above; low ground still sees and shoots up (Dan, option A).
- Height look: sloped rocky banks occupying the outer tile ring of each plateau (Dan, option B), not sheer walls.
- Buildable overlay only while placing (Dan, option B).
- Map layout: home plateaus at middle level with 2 ramps, low-ground ore, high centre (Dan).
- Generator approach (Dan, option 1).

## My calls (Dan can override)

- Ramps 2–3 tiles wide. A wall across a ramp seals it (agreed in playback), so `Wall` is the one structure placeable on `Ramp`; nothing else builds on ramps or banks.
- A ramp tile counts as the lower of the two levels it joins, for range and sight.
- Level used for range: the tile under the shooter's position (unit) or the footprint's top-left / centre tile (structure); target likewise. Aircraft as targets get no bonus; aircraft as shooters get no bonus.
- Existing `Cliff` outcrops stay as small impassable obstacles on any level, rendered as today.
- Level is static after map generation, so it does not need to go into `World::hash()`.
- Minimap shades levels (lighter = higher) and draws banks dark, ramps as the level colour.

## Left to the builder

- Exact generator rules and noise thresholds for plateau outlines, ramp placement (fixed angles out of each home plateau and the centre), and how outcrops/ore/rock are distributed per level. Must guarantee: symmetry, every level boundary is bank or ramp, every start reaches the centre on foot, home plateau contains the start's rock area.
- How the overlay maps tiles to "placeable": per-tile (`place_tile_ok` + within `BUILD_GAP` of own structures) is fine and simpler than per-footprint.
- Rendering: level height in world units, bank slope shape and texture, how units' / structures' Z follows ground (including on ramps), keeping chunk re-bake working.
- Tests in `tests/sim.rs`: banks block paths, ramps connect levels, range bonus applies uphill-only, map invariants.
