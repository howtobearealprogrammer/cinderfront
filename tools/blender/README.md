# Cinderfront models

`build_models.py` builds every unit and structure model from primitives (bmesh, no
hand-edited data), exports `assets/models/<name>.glb`, renders `assets/icons/<name>.png`
and saves `assets/models/source.blend` for browsing. It starts from an empty scene each
time and produces the same geometry on every run.

## Regenerate

From the repo root:

```
/home/dan/blender-5.2.2-linux-x64/blender --background --factory-startup --python tools/blender/build_models.py
```

It takes about a minute, most of it icon rendering. The GLBs come out byte-identical on
every run: after export, `patch_glb` rewrites each file's vertex and index buffers in a
fixed order and recomputes normals in Python, because the exporter's vertex order shifts
with last-bit float noise between runs. The script asserts that every GLB
contains the required nodes and the `Team` material, then prints a tri count and the node
and material names for each model.

## Conventions

- 1 Blender unit = 1 map tile. Blender Z is up. GLBs use the exporter defaults (+Y up).
- Map +x = Blender +X. Map +y (south, screen down) = Blender -Y.
- Units: origin at the centre, at ground level (Z = 0). Forward is +X. Air units also
  have their lowest point at Z = 0; the game lifts them.
- Structures: origin at the footprint centre, ground at Z = 0. Geometry stays inside the
  footprint with a 0.06 tile inset (the wall fills its whole tile). Doors face south (-Y).
- Hierarchy: the root node is named after the file (`tank`). Under it are `body` (all
  static geometry) and the moving parts, each with its origin at its pivot:
  - `turret`: yaw around local Z, barrel along +X (tank, turret, rocket_turret)
  - `rotor`: spins around local Z (gunship, carrier); `rotor2` is the carrier's rear rotor
  - `load`: harvester ore pile, origin at its base; scale it on Z by the fill level
  `Model.add` takes parts built in model coordinates and moves them to the pivot. A part
  built around its own origin (the rotors) is added with `local=True`.
- Materials are solid-colour Principled BSDF. Any part in `Team` takes the player's
  colour; it is blue (0.22, 0.52, 0.98) in the file. Other shared names: `Steel`, `Hull`,
  `Glass`, `Lamp`, `Tread`, `Ore`, plus a few extras (`Wall`, `Concrete`, `Hazard`,
  `Window`, `Reactor`, `Research`, and others). See the `MATERIALS` table in the script.
- Object names in Blender are global, so in `source.blend` non-root objects are called
  `<model>:<part>` (for example `tank:turret`). The script strips the prefix from node
  and mesh names in the GLB after export.
- Icons are 128x96 RGBA with a transparent background. They are rendered with an
  orthographic Eevee camera looking north and pitched 50 degrees down, framed so the
  model fills about 80% of the frame.

## Adding a model

Write a `build_<name>()` function that returns a `Model`, register it in `BUILDERS`
(and in `REQUIRED_PARTS` if it has moving parts), and add the name to `UNITS` or
`STRUCTS`. Structures share `base_block` (plinth, walls, pylons, parapet) and a small kit of
details (`bay_door`, `fan`, `vent_box`, `mast`, `dish`, `tank`, `pipe`). Unit builders are written in pixels of the old 2D renderer (`PX` = 1/40 tile)
so they match the old `draw_unit` shapes. Structure builders use tile units.
