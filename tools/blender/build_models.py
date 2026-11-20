"""Build every Cinderfront 3D model from primitives, export GLBs, render icons.

Run from the repo root:
    blender --background --factory-startup --python tools/blender/build_models.py

Outputs:
    assets/models/<name>.glb     one per model
    assets/icons/<name>.png      128x96 RGBA icon per model
    assets/models/source.blend   every model, side by side, for inspection

See tools/blender/README.md for conventions.
"""

import json
import math
import os
import struct
import sys

import bmesh
import bpy
from mathutils import Euler, Matrix, Vector

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
MODEL_DIR = os.path.join(ROOT, "assets", "models")
ICON_DIR = os.path.join(ROOT, "assets", "icons")

UNITS = ["trooper", "rocketeer", "buggy", "tank", "harvester", "carrier", "gunship", "explorer", "mcv"]
STRUCTS = ["yard", "power", "refinery", "barracks", "factory", "airpad", "wall", "turret", "armory",
           "tech_lab", "rocket_turret"]
FOOTPRINT = {"yard": (3, 3), "power": (2, 2), "refinery": (3, 2), "barracks": (2, 2), "factory": (3, 2),
             "airpad": (2, 2), "wall": (1, 1), "turret": (1, 1), "armory": (2, 2), "tech_lab": (2, 2),
             "rocket_turret": (1, 1)}
INSET = 0.06
TEAM_BLUE = (0.22, 0.52, 0.98)
PX = 1.0 / 40.0  # one screen pixel of the old 2D renderer (TW = 40) in tiles

# ---------------------------------------------------------------------------
# Materials
# ---------------------------------------------------------------------------

# name: (base colour, roughness, metallic, emission colour or None, emission strength)
MATERIALS = {
    "Team": (TEAM_BLUE, 0.55, 0.1, None, 0.0),
    "Steel": ((0.16, 0.16, 0.18), 0.5, 0.35, None, 0.0),
    "Hull": ((0.56, 0.57, 0.62), 0.6, 0.15, None, 0.0),
    "Wall": ((0.40, 0.41, 0.45), 0.7, 0.05, None, 0.0),
    "Deck": ((0.30, 0.31, 0.35), 0.75, 0.1, None, 0.0),
    "Concrete": ((0.62, 0.61, 0.60), 0.8, 0.0, None, 0.0),
    "Asphalt": ((0.24, 0.25, 0.28), 0.85, 0.0, None, 0.0),
    "Dark": ((0.05, 0.05, 0.06), 0.8, 0.0, None, 0.0),
    "Glass": ((0.35, 0.78, 0.95), 0.2, 0.0, (0.35, 0.78, 0.95), 0.4),
    "Lamp": ((1.0, 0.7, 0.18), 0.4, 0.0, (1.0, 0.7, 0.18), 2.0),
    "Window": ((0.95, 0.85, 0.5), 0.4, 0.0, (0.95, 0.85, 0.5), 0.8),
    "RedLamp": ((1.0, 0.2, 0.15), 0.4, 0.0, (1.0, 0.2, 0.15), 3.0),
    "GreenLamp": ((0.3, 1.0, 0.45), 0.4, 0.0, (0.3, 1.0, 0.45), 3.0),
    "Reactor": ((0.3, 0.9, 1.0), 0.3, 0.0, (0.3, 0.9, 1.0), 1.6),
    "Research": ((0.25, 0.85, 0.45), 0.25, 0.0, (0.25, 0.85, 0.45), 1.0),
    "Tread": ((0.13, 0.12, 0.12), 0.9, 0.0, None, 0.0),
    "Ore": ((0.9, 0.45, 0.1), 0.6, 0.0, (1.0, 0.5, 0.1), 0.25),
    "Hazard": ((0.95, 0.75, 0.12), 0.55, 0.05, None, 0.0),
    "Yellow": ((0.82, 0.66, 0.24), 0.6, 0.1, None, 0.0),
    "Paint": ((0.92, 0.92, 0.88), 0.6, 0.0, None, 0.0),
    "Skin": ((0.86, 0.70, 0.56), 0.7, 0.0, None, 0.0),
    "Olive": ((0.30, 0.36, 0.26), 0.7, 0.05, None, 0.0),
    "Crate": ((0.50, 0.45, 0.32), 0.8, 0.0, None, 0.0),
    "Red": ((0.85, 0.30, 0.20), 0.55, 0.05, None, 0.0),
    "Pipe": ((0.45, 0.42, 0.42), 0.6, 0.3, None, 0.0),
}


def make_materials():
    for name, (col, rough, metal, emit, strength) in MATERIALS.items():
        m = bpy.data.materials.new(name)
        nt = m.node_tree
        p = nt.nodes["Principled BSDF"]
        p.inputs["Base Color"].default_value = (*col, 1.0)
        p.inputs["Roughness"].default_value = rough
        p.inputs["Metallic"].default_value = metal
        if emit:
            p.inputs["Emission Color"].default_value = (*emit, 1.0)
            p.inputs["Emission Strength"].default_value = strength
        m.diffuse_color = (*col, 1.0)


def mat(name):
    return bpy.data.materials[name]


# ---------------------------------------------------------------------------
# Geometry builder
# ---------------------------------------------------------------------------

class Part:
    """Accumulates primitives into one bmesh. Coordinates are multiplied by `k`
    (so unit models can be written in old-renderer pixels) and offset by `zoff`
    (in the same units) before scaling."""

    def __init__(self, k=1.0, zoff=0.0):
        self.k = k
        self.zoff = zoff
        self.bm = bmesh.new()
        self.mats = []

    def P(self, x, y, z):
        return Vector((x * self.k, y * self.k, (z + self.zoff) * self.k))

    def _mi(self, name):
        if name not in self.mats:
            self.mats.append(name)
        return self.mats.index(name)

    def _finish_prim(self, verts, mat_name, bevel=0.0):
        # Ordered de-duplication (not sets) so element order, and so the export, is deterministic.
        vs = list(dict.fromkeys(verts))
        faces = list(dict.fromkeys(f for v in vs for f in v.link_faces))
        mi = self._mi(mat_name)
        for f in faces:
            f.material_index = mi
        if bevel > 0:
            edges = list(dict.fromkeys(e for v in vs for e in v.link_edges))
            res = bmesh.ops.bevel(self.bm, geom=edges, offset=bevel, offset_type='OFFSET', segments=1,
                                  profile=0.5, affect='EDGES', clamp_overlap=True)
            for f in res["faces"]:
                f.material_index = mi

    def box(self, x, y, z0, z1, hx, hy, m, bevel=None, rz=0.0, taper=(1.0, 1.0), shift=(0.0, 0.0)):
        """Axis box centred at (x, y), from z0 to z1, half-extents hx, hy.
        taper scales the top face, shift moves it (both in local XY)."""
        k = self.k
        sx, sy, sz = 2 * hx * k, 2 * hy * k, (z1 - z0) * k
        c = self.P(x, y, (z0 + z1) / 2)
        ret = bmesh.ops.create_cube(self.bm, size=1.0)
        verts = ret["verts"]
        for v in verts:
            top = v.co.z > 0
            lx, ly = v.co.x, v.co.y
            if top:
                lx = lx * taper[0] + shift[0] / (2 * hx) if hx else lx
                ly = ly * taper[1] + shift[1] / (2 * hy) if hy else ly
            v.co = Vector((lx * sx, ly * sy, v.co.z * sz))
        rot = Matrix.Rotation(rz, 4, 'Z')
        bmesh.ops.transform(self.bm, matrix=Matrix.Translation(c) @ rot, verts=verts)
        if bevel is None:
            mn = min(sx, sy, sz)
            bevel = 0.012 if mn >= 0.06 else (0.006 if mn >= 0.03 else 0.0)
        self._finish_prim(verts, m, bevel)

    def cyl(self, p0, p1, r0, m, r1=None, seg=12, cap=True):
        """Cylinder / cone from point p0 to p1 (unscaled coords), radii r0 at p0, r1 at p1."""
        if r1 is None:
            r1 = r0
        a, b = self.P(*p0), self.P(*p1)
        d = b - a
        ln = d.length
        ret = bmesh.ops.create_cone(self.bm, cap_ends=cap, cap_tris=True, segments=seg,
                                    radius1=max(r0 * self.k, 1e-5), radius2=r1 * self.k, depth=ln)
        q = Vector((0, 0, 1)).rotation_difference(d.normalized())
        mtx = Matrix.Translation((a + b) / 2) @ q.to_matrix().to_4x4()
        bmesh.ops.transform(self.bm, matrix=mtx, verts=ret["verts"])
        self._finish_prim(ret["verts"], m)

    def cylz(self, x, y, z0, z1, r, m, r1=None, seg=12):
        self.cyl((x, y, z0), (x, y, z1), r, m, r1, seg)

    def sphere(self, x, y, z, r, m, scale=(1, 1, 1), seg=12, rings=6, hemi=False):
        ret = bmesh.ops.create_uvsphere(self.bm, u_segments=seg, v_segments=rings, radius=1.0)
        verts = ret["verts"]
        if hemi:
            low = [v for v in verts if v.co.z < -1e-4]
            bmesh.ops.delete(self.bm, geom=low, context='VERTS')
            verts = [v for v in verts if v.is_valid]
        k = self.k
        c = self.P(x, y, z)
        for v in verts:
            v.co = Vector((v.co.x * r * scale[0] * k, v.co.y * r * scale[1] * k, v.co.z * r * scale[2] * k)) + c
        self._finish_prim(verts, m)

    def ring(self, x, y, z0, z1, r_in, r_out, m, seg=32):
        """Flat annulus (paint marks)."""
        bm = self.bm
        rows = []
        for (rr, zz) in ((r_out, z0), (r_out, z1), (r_in, z1), (r_in, z0)):
            rows.append([bm.verts.new(self.P(x + rr * math.cos(2 * math.pi * i / seg),
                                             y + rr * math.sin(2 * math.pi * i / seg), zz)) for i in range(seg)])
        verts = [v for row in rows for v in row]
        for j in range(4):
            r0, r1 = rows[j], rows[(j + 1) % 4]
            for i in range(seg):
                i2 = (i + 1) % seg
                bm.faces.new((r0[i], r0[i2], r1[i2], r1[i]))
        bmesh.ops.recalc_face_normals(bm, faces=list(dict.fromkeys(f for v in verts for f in v.link_faces)))
        self._finish_prim(verts, m)

    def poly(self, x, y, z0, z1, pts, m, top=(1.0, 1.0), shift=(0.0, 0.0), bevel=0.0):
        """Prism over the convex outline `pts` (XY offsets from (x, y), counter-clockwise).
        `top` scales the top outline, `shift` moves it."""
        bm = self.bm
        lo = [bm.verts.new(self.P(x + px, y + py, z0)) for px, py in pts]
        hi = [bm.verts.new(self.P(x + px * top[0] + shift[0], y + py * top[1] + shift[1], z1)) for px, py in pts]
        n = len(pts)
        bm.faces.new(hi)
        bm.faces.new(lo[::-1])
        for i in range(n):
            bm.faces.new((lo[i], lo[(i + 1) % n], hi[(i + 1) % n], hi[i]))
        self._finish_prim(lo + hi, m, bevel)

    def cbox(self, x, y, z0, z1, hx, hy, c, m, top=(1.0, 1.0), shift=(0.0, 0.0), bevel=0.0):
        """Box with its four vertical corners clipped by `c` (octagonal footprint)."""
        pts = [(hx - c, -hy), (hx, -hy + c), (hx, hy - c), (hx - c, hy),
               (-hx + c, hy), (-hx, hy - c), (-hx, -hy + c), (-hx + c, -hy)]
        self.poly(x, y, z0, z1, pts, m, top, shift, bevel)

    def to_object(self, name, parent, pivot=(0.0, 0.0, 0.0), coll=None, local=False):
        """Create the mesh object. `pivot` is in final (tile) coordinates relative to the parent.
        `local` means the geometry was already built around the pivot (origin = pivot)."""
        bm = self.bm
        pv = Vector(pivot)
        if not local:
            bmesh.ops.translate(bm, vec=-pv, verts=bm.verts)
        bmesh.ops.recalc_face_normals(bm, faces=bm.faces)
        # Triangulate here with fixed methods so the exported index order is reproducible.
        bmesh.ops.triangulate(bm, faces=bm.faces[:], quad_method='FIXED', ngon_method='EAR_CLIP')
        bm = canonical(bm)
        # Auto-smooth: smooth faces, sharp edges above 40 degrees.
        thr = math.radians(40)
        for f in bm.faces:
            f.smooth = True
        for e in bm.edges:
            if len(e.link_faces) != 2 or e.calc_face_angle(math.pi) > thr:
                e.smooth = False
        me = bpy.data.meshes.new(name)
        bm.to_mesh(me)
        bm.free()
        for mn in self.mats:
            me.materials.append(mat(mn))
        ob = bpy.data.objects.new(name, me)
        (coll or bpy.context.scene.collection).objects.link(ob)
        ob.parent = parent
        ob.location = pv
        return ob


def canonical(bm):
    """Rebuild a triangulated bmesh with vertices and faces in a geometry-defined order.
    Some bmesh operators (bevel) emit elements in an order that varies between runs, which
    would make the exported GLBs differ byte-for-byte; this makes them reproducible."""
    def q(co):
        return (round(co.x, 6), round(co.y, 6), round(co.z, 6))

    def centroid(f):
        return q(f.calc_center_median())

    vkey = {v: (q(v.co), tuple(sorted(centroid(f) for f in v.link_faces))) for v in bm.verts}
    order = sorted(bm.verts, key=lambda v: vkey[v])
    rank = {v: i for i, v in enumerate(order)}
    faces = []
    for f in bm.faces:
        idx = [rank[v] for v in f.verts]
        m = idx.index(min(idx))
        idx = idx[m:] + idx[:m]
        faces.append((f.material_index, idx))
    faces.sort()
    out = bmesh.new()
    nv = [out.verts.new(v.co) for v in order]
    for mi, idx in faces:
        f = out.faces.new([nv[i] for i in idx])
        f.material_index = mi
    bm.free()
    return out


def tris(ob):
    return sum(len(p.vertices) - 2 for p in ob.data.polygons)


# ---------------------------------------------------------------------------
# Model scaffolding
# ---------------------------------------------------------------------------

class Model:
    def __init__(self, name):
        self.name = name
        self.coll = bpy.data.collections.new(name)
        bpy.context.scene.collection.children.link(self.coll)
        self.root = bpy.data.objects.new(name, None)
        self.root.empty_display_size = 0.3
        self.coll.objects.link(self.root)
        self.parts = {}

    def add(self, part_name, part, pivot=(0.0, 0.0, 0.0), local=False):
        # Blender object names are global, so non-root objects carry a "<model>:" prefix
        # that is stripped from the GLB afterwards (see patch_glb).
        ob = part.to_object(f"{self.name}:{part_name}", self.root, pivot, self.coll, local)
        ob["gltf_name"] = part_name
        self.parts[part_name] = ob
        return ob

    def objects(self):
        return [self.root] + list(self.root.children_recursive)


def wheel_y(P, x, y, zc, r, half_w, m="Tread", hub="Steel", seg=12):
    P.cyl((x, y - half_w, zc), (x, y + half_w, zc), r, m, seg=seg)
    side = 1 if y > 0 else -1
    P.cyl((x, y + side * half_w, zc), (x, y + side * (half_w + 0.3), zc), r * 0.45, hub, seg=8)


def tracks(P, hx, off, guard="Hull", wheels=5):
    for s in (-1, 1):
        y = off * s
        P.box(0, y, 0, 4.5, hx, 2.6, "Tread", bevel=0.02)
        P.box(0, y, 4.5, 5.2, hx + 0.6, 2.9, guard)
        for i in range(wheels):
            x = -hx + 2.5 + i * (2 * hx - 5.0) / (wheels - 1)
            P.cyl((x, y + s * 2.6, 2.3), (x, y + s * 2.9, 2.3), 1.6, "Steel", seg=8)


def rotor_part(radius, k, blades=2):
    """Rotor in its own local frame: hub at origin, blades in the XY plane."""
    R = Part(k)
    R.cylz(0, 0, 0, 1.0, 1.6, "Steel", seg=10)
    for i in range(blades):
        R.box(0, 0, 0.3, 0.8, radius, 0.85, "Steel", bevel=0.0, rz=math.pi * i / blades + math.pi / 4)
    for i in range(blades * 2):
        a = math.pi * i / blades + math.pi / 4
        R.box(radius * 0.92 * math.cos(a), radius * 0.92 * math.sin(a), 0.32, 0.82, radius * 0.08, 0.9, "Team",
              bevel=0.0, rz=a)
    return R


# ---------------------------------------------------------------------------
# Units (written in old-renderer pixels; r of the old code maps to Blender -Y)
# ---------------------------------------------------------------------------

def build_infantry(name, rocket):
    M = Model(name)
    P = Part(0.9 * PX)
    for s in (-1, 1):
        P.box(0.3 * s, 1.6 * s, 0, 5.0, 0.9, 0.9, "Steel")  # legs (slight stride)
        P.box(1.0 * s + 0.3, 1.6 * s, 0, 0.8, 1.4, 1.0, "Dark")  # boots
    P.box(0, 0, 4.6, 9.6, 1.8, 2.7, "Team", taper=(0.9, 0.95))  # torso
    P.box(0, 0, 4.4, 5.4, 1.9, 2.8, "Steel")  # belt
    for s in (-1, 1):
        P.box(0.6, 3.4 * s, 5.6, 9.2, 0.8, 0.7, "Team")  # arms
    P.sphere(0, 0, 11.0, 2.2, "Skin", seg=10, rings=6)
    P.sphere(-0.2, 0, 11.4, 2.55, "Team", scale=(1.05, 1.0, 0.85), seg=10, rings=4, hemi=True)  # helmet
    if rocket:
        P.box(-3.0, 0, 5.4, 9.8, 1.2, 2.2, "Olive")  # backpack
        P.cyl((-4.0, -2.2, 10.2), (11.0, -2.2, 10.2), 1.4, "Olive", seg=10)  # launcher tube
        P.cyl((11.0, -2.2, 10.2), (11.6, -2.2, 10.2), 1.6, "Steel", seg=10)
        P.cyl((-4.6, -2.2, 10.2), (-4.0, -2.2, 10.2), 1.6, "Steel", seg=10)
    else:
        P.box(4.0, -1.4, 7.4, 8.8, 4.2, 0.55, "Steel")  # rifle
        P.box(1.6, -1.4, 6.0, 7.6, 0.6, 0.5, "Steel")  # grip
    M.add("body", P)
    return M


def build_buggy():
    M = Model("buggy")
    P = Part(1.1 * PX)
    for a in (-1, 1):
        for b in (-1, 1):
            wheel_y(P, 6.5 * a, 5.8 * b, 2.4, 2.4, 1.6)
    P.box(0, 0, 2.0, 6.0, 9.5, 4.6, "Team")  # chassis
    P.box(6.8, 0, 6.0, 7.0, 2.6, 3.6, "Hull", taper=(0.8, 0.9))  # hood
    P.box(-2.5, 0, 6.0, 9.5, 3.6, 3.8, "Hull", taper=(0.75, 0.9), shift=(-0.6, 0))  # cabin
    P.box(1.15, 0, 6.3, 9.0, 0.45, 3.3, "Glass", taper=(1.0, 0.9), shift=(-1.0, 0))  # windscreen
    P.box(-8.0, 0, 6.0, 6.8, 1.6, 4.4, "Steel")  # rear rack
    for s in (-1, 1):
        P.box(9.6, 3.0 * s, 4.0, 5.4, 0.3, 0.9, "Lamp")
    P.cylz(-1.5, 0, 9.5, 10.6, 1.3, "Steel", seg=8)  # gun ring
    P.box(-1.5, 0, 10.4, 11.8, 1.6, 1.0, "Steel")
    P.cyl((-0.5, 0, 11.1), (8.5, 0, 11.1), 0.55, "Steel", seg=8)  # machine gun
    M.add("body", P)
    return M


def build_tank():
    M = Model("tank")
    P = Part(PX)
    tracks(P, 13.0, 8.2)
    P.box(1.0, 0, 2.0, 7.0, 12.0, 6.6, "Team", taper=(0.92, 0.95), shift=(-1.0, 0))  # hull
    P.box(11.5, 0, 2.0, 5.2, 1.6, 5.6, "Hull", taper=(0.6, 1.0), shift=(-1.0, 0))  # glacis plate
    P.box(-8.5, 0, 7.0, 8.2, 2.2, 5.6, "Steel")  # engine deck
    for s in (-1, 1):
        P.box(-8.5, 3.0 * s, 8.2, 8.6, 1.4, 1.0, "Hull")  # vents
        P.box(12.6, 4.6 * s, 4.0, 5.2, 0.3, 0.8, "Lamp")
    M.add("body", P)

    T = Part(PX)
    T.box(-0.5, 0, 7.0, 11.5, 6.4, 5.0, "Team", taper=(0.85, 0.85), shift=(-0.6, 0))  # turret shell
    T.box(6.4, 0, 8.0, 11.0, 1.4, 2.6, "Hull", taper=(0.8, 0.9))  # mantlet
    T.cyl((7.0, 0, 9.6), (19.5, 0, 9.6), 1.0, "Steel", seg=10)  # barrel
    T.cyl((19.5, 0, 9.6), (22.6, 0, 9.6), 1.5, "Steel", seg=10)  # muzzle brake
    T.cylz(-2.5, -1.8, 11.5, 12.3, 2.0, "Hull", seg=10)  # hatch
    T.box(-6.6, 0, 8.0, 11.0, 1.2, 4.0, "Steel")  # bustle rack
    T.cylz(-5.0, 3.4, 11.5, 22.0, 0.25, "Steel", seg=6)  # antenna
    M.add("turret", T, pivot=(-0.5 * PX, 0.0, 7.0 * PX))
    return M


def build_harvester():
    M = Model("harvester")
    P = Part(PX)
    tracks(P, 15.0, 9.4)
    P.box(17.0, 0, 0.0, 4.5, 1.6, 9.5, "Steel", taper=(0.5, 1.0), shift=(-1.2, 0))  # scoop blade
    for i in range(5):
        P.box(18.9, -7.6 + i * 3.8, 0.0, 1.2, 0.6, 0.6, "Hull", bevel=0.0)  # teeth
    P.box(-2.5, 0, 2.0, 11.0, 12.0, 8.4, "Yellow")  # body
    for s in (-1, 1):
        P.box(-2.5, 8.5 * s, 6.5, 9.0, 11.2, 0.25, "Team", bevel=0.0)  # team side stripe
    # Hopper rim around the ore bay (load sits on the bay floor at z = 11).
    P.box(-2.5, 0, 10.6, 11.0, 9.6, 6.2, "Dark", bevel=0.0)  # bay floor
    for s in (-1, 1):
        P.box(-2.5, 6.9 * s, 11.0, 14.0, 10.6, 0.8, "Steel")
        P.box(-2.5 + 10.0 * s, 0, 11.0, 14.0, 0.8, 6.1, "Steel")
    P.box(11.5, 0, 2.0, 13.5, 3.2, 6.8, "Team", taper=(0.9, 0.92))  # cab
    P.box(14.75, 0, 8.5, 12.8, 0.45, 5.6, "Glass", shift=(-0.5, 0))
    P.box(11.2, 0, 13.5, 14.0, 2.6, 6.2, "Hull")  # cab roof
    P.cylz(10.2, 3.5, 14.0, 15.6, 1.1, "Lamp", seg=8)  # beacon
    P.cylz(7.0, -7.2, 9.0, 18.0, 0.8, "Steel", seg=8)  # exhaust stack
    P.cylz(7.0, -7.2, 18.0, 18.6, 1.0, "Dark", seg=8)
    M.add("body", P)

    L = Part(PX)
    # Ore pile: a slab with a mounded top, origin at its base (bay floor).
    L.box(-2.5, 0, 11.0, 13.2, 9.5, 6.0, "Ore", bevel=0.0)
    L.box(-2.5, 0, 13.2, 15.2, 8.0, 4.6, "Ore", taper=(0.55, 0.5), bevel=0.0)
    for (x, y, h, rz) in ((-6.5, 2.0, 2.2, 0.4), (1.0, -2.2, 2.6, 1.1), (-3.0, -3.0, 1.8, 0.8), (3.5, 2.6, 1.6, 0.2)):
        L.box(x, y, 13.0, 13.0 + h, 1.5, 1.3, "Ore", taper=(0.3, 0.3), rz=rz, bevel=0.0)
    M.add("load", L, pivot=(-2.5 * PX, 0.0, 11.0 * PX))
    return M


def build_explorer():
    M = Model("explorer")
    P = Part(1.3 * PX)
    for a in (-1, 1):
        for b in (-1, 1):
            wheel_y(P, 5.0 * a, 4.4 * b, 1.9, 1.9, 1.3)
    P.box(0, 0, 1.5, 5.0, 7.5, 3.4, "Team", taper=(0.95, 0.95))  # body
    P.box(2.7, 0, 5.0, 7.5, 1.9, 2.8, "Glass", taper=(0.6, 0.85), shift=(-0.6, 0))  # canopy
    P.box(-3.2, 0, 5.0, 5.8, 3.4, 3.0, "Hull")  # rear deck
    P.box(7.6, 0, 2.0, 4.0, 0.5, 3.0, "Steel")  # bumper
    for s in (-1, 1):
        P.box(7.5, 2.4 * s, 3.8, 4.6, 0.3, 0.6, "Lamp")
    # Scanner mast and dish (static).
    P.cylz(-3.2, 0, 5.8, 12.5, 0.5, "Steel", seg=6)
    P.cyl((-3.6, 0, 12.4), (-2.2, 0, 13.6), 0.8, "Paint", r1=3.4, seg=12)
    P.sphere(-2.6, 0, 13.4, 0.6, "GreenLamp", seg=6, rings=4)
    M.add("body", P)
    return M


def build_mcv():
    M = Model("mcv")
    P = Part(1.25 * PX)
    tracks(P, 17.0, 10.5, wheels=6)
    P.box(-2.0, 0, 2.0, 12.0, 15.0, 9.4, "Hull")  # main hull
    for s in (-1, 1):
        P.box(-2.0, 9.55 * s, 7.0, 10.0, 14.0, 0.25, "Team", bevel=0.0)  # side stripes
    P.box(-5.0, 0, 12.0, 16.0, 9.0, 7.0, "Hull", taper=(0.95, 0.95))  # upper deck
    P.box(-5.0, 0, 16.0, 17.0, 6.0, 4.5, "Team")  # roof plate
    P.box(12.5, 0, 2.0, 15.0, 3.6, 8.0, "Team", taper=(0.9, 0.95))  # cab
    P.box(15.9, 0, 9.5, 14.4, 0.45, 6.6, "Glass", shift=(-0.6, 0))
    P.box(16.1, 0, 3.0, 6.0, 0.6, 7.0, "Steel")  # bumper
    # Folded crane on the right side.
    P.cylz(-13.0, -6.0, 16.0, 17.5, 2.2, "Steel", seg=10)
    P.box(-3.5, -6.0, 17.0, 19.0, 10.5, 1.2, "Hazard")
    P.box(7.5, -6.0, 15.0, 19.0, 0.8, 1.0, "Hazard")  # boom rest
    P.box(-12.0, 6.0, 12.0, 15.0, 2.0, 1.4, "Steel")  # equipment box
    for s in (-1, 1):
        P.cylz(11.0, 6.0 * s, 15.0, 16.6, 1.1, "Lamp", seg=8)
        P.box(16.3, 5.0 * s, 6.5, 8.0, 0.3, 1.0, "Lamp")
    M.add("body", P)
    return M


def build_carrier():
    """Tandem-rotor heavy lifter (the 2D sprite had four corner rotors; the 3D model
    uses two so it fits the `rotor` / `rotor2` naming)."""
    M = Model("carrier")
    CK = 1.2 * PX
    P = Part(CK)
    # Cargo clamp claws are the lowest point (z = 0).
    for a in (-1, 1):
        for b in (-1, 1):
            P.box(9.0 * a, 4.8 * b, 0.0, 4.0, 0.9, 0.7, "Steel")
            P.box(9.0 * a, 4.2 * b, 0.0, 0.9, 1.0, 1.2, "Hazard", bevel=0.0)
    P.box(0, 0, 3.0, 10.0, 15.0, 6.0, "Team", taper=(0.95, 0.85))  # fuselage
    P.box(16.4, 0, 3.4, 9.0, 1.8, 4.8, "Hull", taper=(0.55, 0.7), shift=(-0.9, 0))  # nose
    P.box(16.6, 0, 5.6, 9.0, 1.2, 4.0, "Glass", taper=(0.5, 0.8), shift=(-0.9, 0))
    P.box(0, 0, 10.0, 11.5, 9.0, 3.0, "Hull")  # spine
    for s in (-1, 1):
        P.cyl((-6.0, 7.6 * s, 7.0), (6.0, 7.6 * s, 7.0), 2.2, "Steel", seg=10)  # engine pods
        P.cyl((6.0, 7.6 * s, 7.0), (7.0, 7.6 * s, 7.0), 1.6, "Dark", seg=10)
        P.box(0, 6.4 * s, 6.0, 8.0, 3.0, 1.2, "Hull")
        P.box(14.0, 3.6 * s, 4.0, 5.0, 0.3, 0.8, "Lamp")
    P.cylz(12.0, 0, 9.0, 14.0, 1.4, "Steel", seg=10)  # front rotor mast
    P.box(-12.0, 0, 8.0, 17.0, 3.2, 2.2, "Hull", taper=(0.7, 0.8))  # rear pylon
    P.box(-12.0, 0, 13.0, 15.0, 3.0, 2.3, "Team", bevel=0.0)
    M.add("body", P)
    M.add("rotor", rotor_part(11.0, CK, blades=3), pivot=(12.0 * CK, 0.0, 14.0 * CK), local=True)
    M.add("rotor2", rotor_part(11.0, CK, blades=3), pivot=(-12.0 * CK, 0.0, 17.0 * CK), local=True)
    return M


def build_gunship():
    M = Model("gunship")
    P = Part(PX, zoff=1.0)  # rocket pods hang to z = -1 in the old sprite
    P.box(-14.0, 0, 2.0, 4.5, 6.0, 1.1, "Team", taper=(1.0, 0.8))  # tail boom
    P.box(-19.5, 0, 3.0, 4.5, 1.2, 4.0, "Hull")  # stabiliser
    P.box(-19.5, 0, 4.5, 9.0, 1.6, 0.6, "Team", taper=(0.7, 1.0), shift=(-0.8, 0))  # fin
    P.cyl((-20.0, 0.6, 6.5), (-20.0, 1.0, 6.5), 2.4, "Steel", seg=8)  # tail rotor disc
    P.box(1.0, 0, 1.0, 3.0, 2.6, 10.0, "Hull")  # stub wings
    for s in (-1, 1):
        P.cyl((-1.5, 9.5 * s, 0.25), (6.0, 9.5 * s, 0.25), 1.25, "Steel", seg=8)  # rocket pods
        P.cyl((6.0, 9.5 * s, 0.25), (6.6, 9.5 * s, 0.25), 1.25, "Red", r1=0.6, seg=8)
        P.box(-6.0, 3.0 * s, -1.0, 0.0, 6.0, 0.4, "Steel", bevel=0.0)  # skids
        for x in (-8.0, -3.0):
            P.box(x, 3.0 * s, 0.0, 1.0, 0.3, 0.3, "Steel", bevel=0.0)
    P.box(0, 0, 0.0, 6.5, 11.0, 3.4, "Team", taper=(0.9, 0.8))  # body
    P.box(-2.0, 0, 6.5, 7.5, 5.0, 2.4, "Hull")
    P.box(8.0, 0, 3.0, 7.8, 3.6, 2.6, "Glass", taper=(0.55, 0.8), shift=(-1.2, 0))  # canopy
    P.box(11.2, 0, 0.5, 4.5, 1.4, 2.4, "Hull", taper=(0.5, 0.8))  # nose
    P.cyl((11.0, 0, 0.6), (15.0, 0, 0.6), 0.5, "Steel", seg=6)  # chin gun
    P.cylz(1.0, 0, 6.5, 9.5, 1.0, "Steel", seg=8)  # mast
    M.add("body", P)
    M.add("rotor", rotor_part(16.0, PX, blades=2), pivot=(1.0 * PX, 0.0, 10.5 * PX), local=True)
    return M


# ---------------------------------------------------------------------------
# Structures (tile units; x along footprint width, y with north = +Y)
# ---------------------------------------------------------------------------

class Bldg:
    def __init__(self, name):
        self.name = name
        self.W, self.H = FOOTPRINT[name]
        self.hx = self.W / 2 - INSET
        self.hy = self.H / 2 - INSET
        self.front = 0.2 if name == "barracks" else 0.07  # main-block setback from the south edge

    def X(self, fx):
        return (fx - 0.5) * self.W

    def Y(self, fy):  # fy is the old screen fraction (0 = north edge)
        return (0.5 - fy) * self.H


def base_block(P, B, ht, wall="Wall", roof="Deck", stripe=True):
    """Clipped-corner plinth over the whole (inset) footprint, main block set back `B.front`
    from the south edge so door details on the south face stay inside the footprint.
    Adds a sloped footing, a panel seam, corner pylons, wall ribs and a roof parapet."""
    hx, hy = B.hx, B.hy
    wx = hx - 0.04
    n, s = hy - 0.04, -(hy - B.front)
    yc, hh = (n + s) / 2, (n - s) / 2
    P.cbox(0, 0, 0, 0.05, hx, hy, 0.09, "Steel", bevel=0.008)  # plinth
    P.box(0, yc, 0.05, 0.11, wx + 0.012, hh + 0.012, "Steel", bevel=0.0,
          taper=(wx / (wx + 0.012), hh / (hh + 0.012)))  # footing
    P.box(0, yc, 0.05, ht, wx, hh, wall)
    P.box(0, yc, 0.135, 0.145, wx + 0.003, hh + 0.003, "Dark", bevel=0.0)  # panel seam
    if stripe:
        P.box(0, yc, ht - 0.1, ht - 0.04, wx + 0.006, hh + 0.006, "Team", bevel=0.0)
    P.box(0, yc, ht, ht + 0.012, wx - 0.035, hh - 0.035, roof, bevel=0.0)  # roof deck
    for a in (-1, 1):
        P.box(0, yc + a * (hh - 0.02), ht, ht + 0.04, wx, 0.02, "Hull", bevel=0.005)  # parapet
        P.box(a * (wx - 0.02), yc, ht, ht + 0.04, 0.02, hh, "Hull", bevel=0.005)
        for cy in (n - 0.035, s + 0.035):  # corner pylons
            cx = a * (wx - 0.035)
            P.cbox(cx, cy, 0.05, ht + 0.07, 0.065, 0.065, 0.02, "Hull", top=(0.8, 0.8))
            P.box(cx, cy, ht + 0.07, ht + 0.082, 0.035, 0.035, "Team", bevel=0.0)
    rib_top = ht - 0.12
    if rib_top > 0.12:  # wall ribs on the north, east and west faces
        kx = int(B.W * 1.5) - 1
        for i in range(kx):
            P.box(-wx + 2 * wx * (i + 1) / (kx + 1), n, 0.05, rib_top, 0.04, 0.022, "Hull", taper=(0.6, 1.0))
        ky = int(B.H * 1.5) - 1
        for i in range(ky):
            y = s + 2 * hh * (i + 1) / (ky + 1)
            for a in (-1, 1):
                P.box(a * wx, y, 0.05, rib_top, 0.022, 0.04, "Hull", taper=(1.0, 0.6))


def south_face(B):
    return -(B.hy - B.front)


def bay_door(P, x, sf, hw, h, slats=4, light="Lamp"):
    """Recessed door on the south face with jambs, a hazard-striped lintel and a light strip."""
    P.box(x, sf - 0.005, 0.05, h, hw, 0.02, "Dark", bevel=0.0)
    for k in range(slats):
        z = 0.05 + (k + 1) * (h - 0.05) / (slats + 1)
        P.box(x, sf - 0.026, z, z + 0.012, hw - 0.015, 0.004, "Steel", bevel=0.0)
    for a in (-1, 1):
        P.box(x + a * (hw + 0.03), sf - 0.02, 0.05, h, 0.03, 0.035, "Hull", bevel=0.006)
    P.box(x, sf - 0.02, h, h + 0.07, hw + 0.06, 0.035, "Hull", bevel=0.006)
    n = max(4, 2 * round(hw / 0.09))
    for i in range(n):
        cx = x - hw + (i + 0.5) * 2 * hw / n
        P.box(cx, sf - 0.056, h + 0.03, h + 0.055, hw / n, 0.003, "Hazard" if i % 2 == 0 else "Dark", bevel=0.0)
    P.box(x, sf - 0.056, h + 0.008, h + 0.022, hw, 0.003, light, bevel=0.0)


def fan(P, x, y, z, r):
    P.cylz(x, y, z, z + 0.06, r, "Steel", seg=20)
    P.cylz(x, y, z + 0.06, z + 0.066, r * 0.84, "Dark", seg=20)
    for i in range(3):
        P.box(x, y, z + 0.064, z + 0.078, r * 0.8, r * 0.13, "Hull", bevel=0.0, rz=0.3 + i * math.pi / 3)
    P.cylz(x, y, z + 0.06, z + 0.09, r * 0.22, "Steel", seg=10)


def vent_box(P, x, y, z, hx, hy, h=0.07):
    P.box(x, y, z, z + h, hx, hy, "Steel", taper=(0.9, 0.9))
    n = max(2, round(hx / 0.035))
    w = 1.6 * hx / n
    for i in range(n):
        P.box(x - 0.8 * hx + (i + 0.5) * w, y, z + h, z + h + 0.008, w * 0.28, hy * 0.75, "Dark", bevel=0.0)


def mast(P, x, y, z0, z1, lamp="RedLamp", bars=3):
    P.cbox(x, y, z0, z0 + 0.05, 0.065, 0.065, 0.02, "Steel", top=(0.7, 0.7))
    P.cylz(x, y, z0 + 0.05, z1, 0.018, "Steel", r1=0.01, seg=8)
    for k in range(bars):
        z = z0 + (z1 - z0) * (0.45 + 0.17 * k)
        P.box(x, y, z, z + 0.014, 0.1 - 0.025 * k, 0.01, "Hull", bevel=0.0)
    P.sphere(x, y, z1 + 0.02, 0.03, lamp, seg=8, rings=4)


def dish(P, x, y, z, r, yaw=0.0, elev=0.9):
    """Radar dish on a pedestal, facing `yaw` (radians from +X) and tilted up by `elev`."""
    P.cylz(x, y, z, z + 0.08, r * 0.5, "Steel", r1=r * 0.3, seg=12)
    P.cylz(x, y, z + 0.08, z + r * 0.9, r * 0.12, "Steel", seg=8)
    c = Vector((x, y, z + r * 0.95))
    d = Vector((math.cos(yaw) * math.cos(elev), math.sin(yaw) * math.cos(elev), math.sin(elev)))
    back, front = c - d * r * 0.1, c + d * r * 0.3
    P.cyl(tuple(back), tuple(front), r * 0.18, "Paint", r1=r, seg=24)
    P.cyl(tuple(front), tuple(front + d * 0.005), r * 0.9, "Deck", seg=24)
    P.cyl(tuple(front), tuple(front + d * r * 0.5), r * 0.04, "Steel", seg=6)
    tip = front + d * r * 0.5
    P.sphere(tip.x, tip.y, tip.z, r * 0.1, "RedLamp", seg=8, rings=4)


def tank(P, x, y, z0, z1, r, m="Hull", bands=3, seg=24):
    P.cylz(x, y, z0, z1, r, m, seg=seg)
    P.sphere(x, y, z1, r, m, scale=(1, 1, 0.4), seg=seg, rings=8, hemi=True)
    for k in range(bands):
        z = z0 + (z1 - z0) * (k + 1) / (bands + 1)
        P.cylz(x, y, z - 0.012, z + 0.012, r + 0.006, "Steel", seg=seg)


def pipe(P, pts, r, m="Pipe", seg=10):
    for a, b in zip(pts, pts[1:]):
        P.cyl(a, b, r, m, seg=seg)
    for p in pts[1:-1]:
        P.sphere(p[0], p[1], p[2], r * 1.15, m, seg=seg, rings=6)


def build_yard():
    B = Bldg("yard")
    M = Model("yard")
    P = Part()
    ht = 0.5
    base_block(P, B, ht)
    sf = south_face(B)
    for i in range(4):  # office windows
        P.box(-1.08 + 0.375 * i, sf - 0.004, 0.2, 0.34, 0.14, 0.012, "Window", bevel=0.0)
    for z in (0.18, 0.34):
        P.box(-0.5175, sf - 0.006, z, z + 0.018, 0.74, 0.014, "Hull", bevel=0.0)
    bay_door(P, 0.88, sf, 0.3, 0.36)
    # Command tower: armoured base, flared glazed bridge, roof cap.
    tx, ty = -0.42, 0.06
    P.cbox(tx, ty, ht, ht + 0.24, 0.74, 0.78, 0.14, "Hull", top=(0.94, 0.94), bevel=0.006)
    P.cbox(tx, ty, ht + 0.12, ht + 0.17, 0.725, 0.765, 0.14, "Team")
    P.cbox(tx, ty, ht + 0.24, ht + 0.38, 0.62, 0.64, 0.12, "Glass", top=(1.08, 1.08))
    P.cbox(tx, ty, ht + 0.38, ht + 0.42, 0.68, 0.7, 0.13, "Hull", top=(0.8, 0.8), bevel=0.006)
    P.cbox(tx, ty, ht + 0.42, ht + 0.432, 0.5, 0.5, 0.1, "Deck")
    P.ring(tx + 0.1, ty - 0.1, ht + 0.43, ht + 0.438, 0.24, 0.28, "Paint", seg=28)
    P.cylz(tx + 0.1, ty - 0.1, ht + 0.43, ht + 0.445, 0.05, "RedLamp", seg=10)
    mast(P, tx - 0.34, ty + 0.34, ht + 0.42, 1.14)
    vent_box(P, tx - 0.3, ty - 0.32, ht + 0.42, 0.1, 0.08, h=0.05)
    # Roof kit: radar dish, fan, vents.
    dish(P, 0.99, 0.72, ht, 0.24, yaw=-0.7)
    fan(P, 0.58, 1.05, ht, 0.14)
    vent_box(P, 0.6, 0.3, ht, 0.14, 0.2)
    # Construction crane over the vehicle bay.
    cx, cy = 1.05, -0.2
    P.cbox(cx, cy, ht, ht + 0.42, 0.055, 0.055, 0.015, "Steel")
    P.cylz(cx, cy, ht + 0.42, ht + 0.45, 0.085, "Hull", seg=14)
    P.box(cx, cy - 0.38, ht + 0.45, ht + 0.5, 0.035, 0.68, "Hazard")
    P.box(cx, cy + 0.22, ht + 0.43, ht + 0.52, 0.07, 0.09, "Steel")  # counterweight
    P.cylz(cx, cy - 0.98, ht + 0.22, ht + 0.45, 0.006, "Steel", seg=6)
    P.box(cx, cy - 0.98, ht + 0.17, ht + 0.22, 0.03, 0.03, "Hazard")
    M.add("body", P)
    return M


def build_power():
    B = Bldg("power")
    M = Model("power")
    P = Part()
    ht = 0.28
    base_block(P, B, ht)
    sf = south_face(B)
    for a in (-1, 1):
        P.box(0.45 * a, sf - 0.004, 0.115, 0.13, 0.22, 0.008, "Reactor", bevel=0.0)
    cy = -0.02
    for dx in (-0.45, 0.45):  # reactor towers
        P.cylz(dx, cy, ht, ht + 0.06, 0.38, "Steel", seg=28)
        P.cylz(dx, cy, ht + 0.06, ht + 0.3, 0.35, "Hull", r1=0.27, seg=28)
        P.cylz(dx, cy, ht + 0.3, ht + 0.42, 0.27, "Hull", r1=0.31, seg=28)
        P.cylz(dx, cy, ht + 0.27, ht + 0.32, 0.284, "Team", seg=28)
        P.cylz(dx, cy, ht + 0.42, ht + 0.46, 0.325, "Steel", seg=28)
        P.cylz(dx, cy, ht + 0.44, ht + 0.467, 0.27, "Reactor", seg=28)  # glowing core
        P.ring(dx, cy, ht + 0.06, ht + 0.075, 0.35, 0.385, "Reactor", seg=28)
        for i in range(6):  # coolant fins
            a = i * math.pi / 3 + math.pi / 6
            P.box(dx + 0.36 * math.cos(a), cy + 0.36 * math.sin(a), ht, ht + 0.27, 0.05, 0.014, "Steel",
                  taper=(0.3, 1.0), shift=(-0.035, 0.0), rz=a, bevel=0.0)
    P.cyl((-0.16, cy, ht + 0.15), (0.16, cy, ht + 0.15), 0.05, "Steel", seg=12)  # conduit
    P.cyl((-0.06, cy, ht + 0.15), (0.06, cy, ht + 0.15), 0.06, "Reactor", seg=12)
    # Transformer bank at the back.
    P.box(0, 0.66, ht, ht + 0.1, 0.36, 0.12, "Steel")
    for i in range(3):
        ix = -0.24 + 0.24 * i
        P.cylz(ix, 0.66, ht + 0.1, ht + 0.24, 0.022, "Paint", seg=8)
        for k in range(3):
            P.cylz(ix, 0.66, ht + 0.12 + 0.04 * k, ht + 0.135 + 0.04 * k, 0.045, "Paint", seg=10)
        P.sphere(ix, 0.66, ht + 0.25, 0.025, "Reactor", seg=8, rings=4)
    # Capacitor cells and a control console at the front.
    for a in (-1, 1):
        P.cylz(0.56 * a, -0.64, ht, ht + 0.12, 0.085, "Hull", seg=16)
        P.cylz(0.56 * a, -0.64, ht + 0.05, ht + 0.07, 0.09, "Steel", seg=16)
        P.cylz(0.56 * a, -0.64, ht + 0.12, ht + 0.13, 0.06, "Reactor", seg=16)
    P.box(0, -0.66, ht, ht + 0.09, 0.2, 0.08, "Steel", taper=(1.0, 0.5), shift=(0.0, 0.04))
    P.box(0, -0.705, ht + 0.03, ht + 0.07, 0.16, 0.01, "Glass", bevel=0.0)
    M.add("body", P)
    return M


def build_refinery():
    B = Bldg("refinery")
    M = Model("refinery")
    P = Part()
    ht = 0.35
    base_block(P, B, ht)
    sf = south_face(B)
    # Ore hopper: flared funnel with a rim and a heap of ore.
    hcx, hcy, hhx, hhy = -0.66, -0.26, 0.54, 0.44
    P.cbox(hcx, hcy, ht, ht + 0.16, hhx - 0.1, hhy - 0.1, 0.06, "Steel", top=(hhx / (hhx - 0.1), hhy / (hhy - 0.1)))
    P.cbox(hcx, hcy, ht + 0.16, ht + 0.2, hhx, hhy, 0.075, "Hull", bevel=0.005)
    for a in (-1, 1):
        P.box(hcx, hcy + a * (hhy - 0.03), ht + 0.2, ht + 0.25, hhx - 0.07, 0.02, "Hull", bevel=0.005)
        P.box(hcx + a * (hhx - 0.03), hcy, ht + 0.2, ht + 0.25, 0.02, hhy - 0.07, "Hull", bevel=0.005)
    P.box(hcx, hcy, ht + 0.2, ht + 0.225, hhx - 0.05, hhy - 0.05, "Ore", bevel=0.0)
    P.box(hcx, hcy, ht + 0.225, ht + 0.29, hhx - 0.12, hhy - 0.12, "Ore", taper=(0.6, 0.55), bevel=0.0)
    for (x, y, h, rz) in ((-0.3, 0.12, 0.07, 0.4), (0.25, -0.15, 0.08, 1.1), (-0.1, -0.2, 0.06, 0.8)):
        P.box(hcx + x, hcy + y, ht + 0.22, ht + 0.22 + h, 0.07, 0.06, "Ore", taper=(0.3, 0.3), rz=rz, bevel=0.0)
    # Furnace stacks.
    for (cx, cy, r, top) in ((-1.08, 0.6, 0.12, 1.02), (-0.76, 0.66, 0.075, 0.84)):
        P.cylz(cx, cy, ht, ht + 0.05, r * 1.3, "Steel", seg=16)
        P.cylz(cx, cy, ht + 0.05, top, r, "Pipe", r1=r * 0.78, seg=16)
        P.cylz(cx, cy, top - 0.17, top - 0.11, r * 0.86, "Team", seg=16)
        P.cylz(cx, cy, top - 0.07, top - 0.05, r * 0.84, "Lamp", seg=16)
        P.cylz(cx, cy, top - 0.01, top + 0.012, r * 0.82, "Steel", seg=16)
        P.cylz(cx, cy, top + 0.012, top + 0.016, r * 0.62, "Dark", seg=16)
    # Main silo with ladder, and a smaller buffer tank.
    sx, sy = 0.84, -0.02
    tank(P, sx, sy, ht, 0.82, 0.4, seg=28)
    P.cylz(sx, sy, 0.7, 0.77, 0.408, "Team", seg=28)
    P.cylz(sx, sy, 0.96, 1.0, 0.1, "Steel", seg=12)
    P.cylz(sx, sy, 1.0, 1.03, 0.04, "Lamp", seg=8)
    for a in (-1, 1):
        P.box(sx + 0.04 * a, sy - 0.412, ht, 0.84, 0.006, 0.006, "Steel", bevel=0.0)
    for k in range(8):
        P.box(sx, sy - 0.412, ht + 0.04 + 0.06 * k, ht + 0.05 + 0.06 * k, 0.04, 0.005, "Steel", bevel=0.0)
    tank(P, 0.2, 0.55, ht, 0.58, 0.2, bands=2, seg=20)
    P.cylz(0.2, 0.55, 0.5, 0.54, 0.206, "Team", seg=20)
    pipe(P, [(-0.14, -0.3, ht + 0.1), (0.3, -0.3, ht + 0.1), (0.3, -0.12, ht + 0.1), (0.46, -0.12, ht + 0.1)], 0.045)
    pipe(P, [(0.2, 0.36, ht + 0.14), (0.2, 0.18, ht + 0.14), (0.48, 0.18, ht + 0.14)], 0.035)
    pipe(P, [(-0.64, 0.66, ht + 0.07), (0.0, 0.66, ht + 0.07)], 0.035)
    # Unloading dock and furnace glow slits on the south face.
    bay_door(P, -0.66, sf, 0.4, 0.2, slats=3)
    for i in range(3):
        P.box(0.5 + 0.3 * i, sf - 0.004, 0.16, 0.22, 0.1, 0.012, "Ore", bevel=0.0)
    M.add("body", P)
    return M


def build_barracks():
    B = Bldg("barracks")
    M = Model("barracks")
    P = Part()
    ht = 0.42
    base_block(P, B, ht)
    sf = south_face(B)
    n = B.hy - 0.04
    # Vaulted roof: three low arches with ridge beams and hoops.
    for x in (-0.56, 0.0, 0.56):
        P.cyl((x, sf + 0.06, ht - 0.14), (x, n - 0.06, ht - 0.14), 0.3, "Hull", seg=24)
        for y in (sf + 0.1, (sf + n) / 2, n - 0.1):
            P.cyl((x, y - 0.025, ht - 0.14), (x, y + 0.025, ht - 0.14), 0.308, "Steel", seg=24)
    for x in (-0.28, 0.28):
        P.box(x, (sf + n) / 2, ht, ht + 0.06, 0.022, (n - sf) / 2 - 0.05, "Steel")
    # Door, slit windows, step.
    bay_door(P, 0, sf, 0.16, 0.3, slats=0, light="Window")
    for a in (-1, 1):
        P.box(0.52 * a, sf - 0.004, 0.2, 0.26, 0.16, 0.012, "Window", bevel=0.0)
    P.box(0, sf - 0.1, 0.05, 0.075, 0.26, 0.08, "Concrete")
    # Forecourt barriers and floodlights.
    for a in (-1, 1):
        P.cbox(0.56 * a, sf - 0.13, 0.05, 0.14, 0.2, 0.035, 0.01, "Concrete", top=(0.95, 0.45))
        P.box(0.56 * a, sf - 0.13, 0.14, 0.148, 0.18, 0.012, "Hazard", bevel=0.0)
        P.cylz(0.84 * a, sf - 0.14, 0.05, 0.3, 0.012, "Steel", seg=6)
        P.box(0.84 * a, sf - 0.14, 0.3, 0.33, 0.03, 0.025, "Lamp", bevel=0.0)
    # Pennant on the south-east pylon, comms mast on the north-west one.
    fx, fy = B.hx - 0.075, sf + 0.035
    P.cylz(fx, fy, ht + 0.08, 1.05, 0.012, "Paint", seg=6)
    P.box(fx - 0.15, fy, 0.88, 1.02, 0.14, 0.006, "Team", bevel=0.0, rz=0.12)
    mast(P, -fx, n - 0.035, ht + 0.08, 0.95, lamp="GreenLamp", bars=2)
    M.add("body", P)
    return M


def build_factory():
    B = Bldg("factory")
    M = Model("factory")
    P = Part()
    ht = 0.62
    base_block(P, B, ht)
    sf = south_face(B)
    # Main vehicle door, side windows, warning lamps.
    dx, dw, dh = -0.25, 0.9, 0.44
    bay_door(P, dx, sf, dw, dh, slats=5)
    for a in (-1, 1):
        P.cylz(dx + a * (dw + 0.03), sf - 0.03, dh + 0.07, dh + 0.11, 0.03, "RedLamp", seg=10)
    P.box(1.02, sf - 0.004, 0.28, 0.38, 0.2, 0.012, "Window", bevel=0.0)
    # Sawtooth skylight roof over the assembly hall.
    for i in range(4):
        cx = -1.1 + i * 0.42
        P.box(cx, 0.02, ht, ht + 0.17, 0.2, 0.7, "Hull", taper=(0.08, 1.0), shift=(0.18, 0.0), bevel=0.006)
        P.box(cx + 0.2, 0.02, ht + 0.025, ht + 0.15, 0.004, 0.64, "Glass", bevel=0.0)
    # Service roof: fans, vents, exhaust stacks.
    for vx in (0.82, 1.14):
        fan(P, vx, -0.5, ht, 0.14)
        P.cylz(vx, 0.62, ht, ht + 0.05, 0.1, "Steel", seg=14)
        P.cylz(vx, 0.62, ht + 0.05, 1.06, 0.07, "Pipe", r1=0.058, seg=14)
        P.cylz(vx, 0.62, 0.92, 0.97, 0.066, "Team", seg=14)
        P.cylz(vx, 0.62, 1.0, 1.02, 0.064, "Lamp", seg=14)
        P.cylz(vx, 0.62, 1.06, 1.065, 0.045, "Dark", seg=14)
    vent_box(P, 0.98, 0.08, ht, 0.24, 0.16)
    pipe(P, [(0.82, 0.52, ht + 0.05), (0.82, 0.28, ht + 0.05)], 0.03)
    M.add("body", P)
    return M


def build_airpad():
    B = Bldg("airpad")
    M = Model("airpad")
    P = Part()
    ht = 0.1
    P.cbox(0, 0, 0, 0.045, B.hx, B.hy, 0.2, "Steel", bevel=0.008)
    P.cbox(0, 0, 0.045, ht, B.hx - 0.01, B.hy - 0.01, 0.2, "Asphalt", top=(0.97, 0.97))
    z0, z1 = ht - 0.002, ht + 0.006
    # Team border following the clipped corners.
    E, c, d, t = (B.hx - 0.01) * 0.97, 0.2 * 0.97, 0.08, 0.015
    e, ci = E - d, c - d * (2 - math.sqrt(2))
    for s in (-1, 1):
        P.box(0, s * e, z0, z1, e - ci, t, "Team", bevel=0.0)
        P.box(s * e, 0, z0, z1, t, e - ci, "Team", bevel=0.0)
        for b in (-1, 1):
            P.box(s * (e - ci / 2), b * (e - ci / 2), z0, z1, ci * math.sqrt(0.5) + t * 0.4, t, "Team", bevel=0.0,
                  rz=-s * b * math.pi / 4)
            P.cylz(s * (e - ci / 2 + 0.03), b * (e - ci / 2 + 0.03), ht - 0.01, ht + 0.05, 0.024, "Steel", seg=10)
            P.cylz(s * (e - ci / 2 + 0.03), b * (e - ci / 2 + 0.03), ht + 0.05, ht + 0.075, 0.02, "RedLamp", seg=10)
    P.ring(0, 0, z0, z1, 0.54, 0.6, "Paint", seg=40)
    for s in (-1, 1):  # "H"
        P.box(0.17 * s, 0, z0, z1, 0.035, 0.24, "Paint", bevel=0.0)
    P.box(0, 0, z0, z1, 0.14, 0.035, "Paint", bevel=0.0)
    for i in range(12):  # flush landing lights
        a = i * math.pi / 6
        P.cylz(0.69 * math.cos(a), 0.69 * math.sin(a), z0, ht + 0.01, 0.022, "GreenLamp", seg=8)
    for i in range(3):  # approach chevron on the south edge
        P.box(-0.1 + 0.1 * i, -0.755, z0, z1, 0.03, 0.012, "Hazard", bevel=0.0, rz=0.0)
    M.add("body", P)
    return M


def build_wall():
    M = Model("wall")
    P = Part()
    P.box(0, 0, 0, 0.1, 0.5, 0.5, "Wall", bevel=0.01)  # footing (full tile, so runs join up)
    P.box(0, 0, 0.1, 0.3, 0.47, 0.47, "Concrete", bevel=0.01)
    for a in (-1, 1):  # armour plates
        P.box(0, a * 0.475, 0.12, 0.28, 0.34, 0.02, "Hull", taper=(0.94, 1.0), bevel=0.006)
        P.box(a * 0.475, 0, 0.12, 0.28, 0.02, 0.34, "Hull", taper=(1.0, 0.94), bevel=0.006)
    P.box(0, 0, 0.3, 0.36, 0.495, 0.495, "Team", bevel=0.0)
    P.box(0, 0, 0.36, 0.46, 0.5, 0.5, "Concrete", taper=(0.92, 0.92), bevel=0.01)
    P.box(0, 0, 0.46, 0.5, 0.4, 0.4, "Wall", bevel=0.008)
    P.cbox(0, 0, 0.5, 0.52, 0.2, 0.2, 0.06, "Steel")
    for a in (-1, 1):
        for b in (-1, 1):
            P.cbox(a * 0.32, b * 0.32, 0.5, 0.53, 0.05, 0.05, 0.015, "Hull")
    M.add("body", P)
    return M


def turret_base(P, B, ht=0.18):
    P.cbox(0, 0, 0, 0.04, B.hx, B.hy, 0.12, "Steel", bevel=0.008)
    P.cbox(0, 0, 0.04, ht, B.hx - 0.02, B.hy - 0.02, 0.12, "Wall", top=(0.86, 0.86))
    for a in (-1, 1):
        for b in (-1, 1):
            P.box(a * 0.347, b * 0.347, 0.06, 0.11, 0.07, 0.012, "Team", bevel=0.0, rz=-a * b * math.pi / 4)
            P.cylz(a * 0.29, b * 0.29, ht, ht + 0.012, 0.02, "Hull", seg=8)
    P.ring(0, 0, ht - 0.002, ht + 0.008, 0.3, 0.314, "Reactor", seg=28)
    P.cylz(0, 0, ht, ht + 0.025, 0.3, "Steel", seg=28)


def build_turret():
    B = Bldg("turret")
    M = Model("turret")
    P = Part()
    turret_base(P, B)
    M.add("body", P)
    T = Part()
    z = 0.205
    T.cylz(0, 0, z, z + 0.04, 0.25, "Hull", seg=24)
    T.cbox(-0.03, 0, z + 0.03, z + 0.19, 0.2, 0.17, 0.05, "Team", top=(0.82, 0.8), bevel=0.006)
    T.box(0.17, 0, z + 0.05, z + 0.15, 0.045, 0.12, "Hull", taper=(0.8, 0.85))  # mantlet
    T.box(-0.22, 0, z + 0.05, z + 0.14, 0.04, 0.12, "Steel")  # ammo box
    for s in (-1, 1):
        T.box(-0.03, 0.172 * s, z + 0.05, z + 0.15, 0.14, 0.018, "Hull", taper=(0.85, 1.0))  # cheek armour
        T.cyl((0.2, 0.065 * s, z + 0.1), (0.46, 0.065 * s, z + 0.1), 0.022, "Steel", seg=10)
        T.cyl((0.2, 0.065 * s, z + 0.1), (0.31, 0.065 * s, z + 0.1), 0.036, "Steel", seg=10)  # barrel jacket
        T.cyl((0.42, 0.065 * s, z + 0.1), (0.47, 0.065 * s, z + 0.1), 0.032, "Steel", seg=10)
        T.cyl((0.47, 0.065 * s, z + 0.1), (0.472, 0.065 * s, z + 0.1), 0.018, "Dark", seg=10)
    T.box(-0.06, 0.07, z + 0.19, z + 0.215, 0.045, 0.035, "Glass", bevel=0.0)  # sensor
    T.cylz(-0.13, -0.08, z + 0.18, z + 0.36, 0.006, "Steel", seg=6)
    M.add("turret", T, pivot=(0.0, 0.0, z))
    return M


def build_rocket_turret():
    B = Bldg("rocket_turret")
    M = Model("rocket_turret")
    P = Part()
    turret_base(P, B)
    M.add("body", P)
    T = Part()
    z = 0.205
    T.cylz(0, 0, z, z + 0.03, 0.22, "Hull", seg=24)
    T.cbox(0, 0, z + 0.03, z + 0.14, 0.11, 0.1, 0.03, "Steel", top=(0.85, 0.85))
    T.box(0.1, 0, z + 0.06, z + 0.11, 0.02, 0.045, "Glass", bevel=0.0)  # sensor
    T.cyl((0, -0.21, z + 0.17), (0, 0.21, z + 0.17), 0.03, "Steel", seg=10)  # pod axle
    for s in (-1, 1):
        cy = 0.145 * s
        T.box(-0.025, cy, z + 0.09, z + 0.27, 0.25, 0.09, "Team", bevel=0.02)
        T.box(-0.025, cy, z + 0.27, z + 0.285, 0.2, 0.06, "Hull", bevel=0.0)
        T.box(0.222, cy, z + 0.108, z + 0.252, 0.006, 0.074, "Steel", bevel=0.0)
        T.box(-0.278, cy, z + 0.12, z + 0.24, 0.006, 0.07, "Dark", bevel=0.0)
        for k in (-1, 1):
            for zz in (z + 0.145, z + 0.215):
                T.cyl((0.2, cy + 0.037 * k, zz), (0.238, cy + 0.037 * k, zz), 0.029, "Dark", seg=10)
                T.cyl((0.21, cy + 0.037 * k, zz), (0.25, cy + 0.037 * k, zz), 0.018, "Red", r1=0.0, seg=10)
    M.add("turret", T, pivot=(0.0, 0.0, z))
    return M


def build_armory():
    B = Bldg("armory")
    M = Model("armory")
    P = Part()
    ht = 0.4
    base_block(P, B, ht)
    sf = south_face(B)
    # Blast door with warning beacons.
    bay_door(P, 0, sf, 0.36, 0.26, slats=3, light="RedLamp")
    for a in (-1, 1):
        P.cylz(0.39 * a, sf - 0.03, 0.33, 0.37, 0.028, "RedLamp", seg=10)
    # Ammunition crates and an armoured roof hatch.
    for (cx, cy, hx, hy, h, m) in ((-0.5, 0.48, 0.22, 0.2, 0.14, "Olive"), (-0.5, 0.02, 0.2, 0.18, 0.12, "Crate"),
                                   (-0.46, 0.46, 0.13, 0.12, 0.24, "Crate")):
        P.box(cx, cy, ht + 0.01, ht + h, hx, hy, m)
        P.box(cx, cy, ht + h * 0.55, ht + h * 0.55 + 0.02, hx + 0.004, hy + 0.004, "Steel", bevel=0.0)
    P.cbox(-0.48, -0.48, ht, ht + 0.03, 0.2, 0.18, 0.04, "Steel", bevel=0.005)
    P.box(-0.48, -0.48, ht + 0.03, ht + 0.036, 0.15, 0.02, "Hazard", bevel=0.0)
    # Vertical missile cells under a loading gantry.
    mx = 0.42
    P.box(mx, 0.03, ht, ht + 0.06, 0.16, 0.56, "Steel")
    for my in (0.4, 0.03, -0.34):
        P.cylz(mx, my, ht + 0.06, ht + 0.3, 0.095, "Hull", seg=16)
        P.cylz(mx, my, ht + 0.2, ht + 0.25, 0.1, "Team", seg=16)
        P.cylz(mx, my, ht + 0.3, ht + 0.305, 0.08, "Dark", seg=16)
        P.cylz(mx, my, ht + 0.3, ht + 0.37, 0.06, "Red", seg=12)
        P.cylz(mx, my, ht + 0.37, ht + 0.47, 0.06, "Paint", r1=0.0, seg=12)
    for gy in (0.62, -0.56):
        P.box(0.7, gy, ht, ht + 0.56, 0.025, 0.025, "Steel")
    P.box(0.7, 0.03, ht + 0.56, ht + 0.6, 0.03, 0.64, "Hazard")
    P.box(0.56, 0.03, ht + 0.565, ht + 0.595, 0.14, 0.03, "Steel")  # trolley arm
    M.add("body", P)
    return M


def build_tech_lab():
    B = Bldg("tech_lab")
    M = Model("tech_lab")
    P = Part()
    ht = 0.45
    base_block(P, B, ht)
    sf = south_face(B)
    for i in range(4):  # glazed front
        P.box(-0.6 + 0.4 * i, sf - 0.004, 0.17, 0.33, 0.15, 0.012, "Glass", bevel=0.0)
    for z in (0.155, 0.33):
        P.box(0, sf - 0.006, z, z + 0.016, 0.78, 0.014, "Hull", bevel=0.0)
    # Research dome with frame hoops and an emitter spike.
    dx, dy, R = -0.2, -0.05, 0.4
    P.cylz(dx, dy, ht, ht + 0.08, 0.46, "Hull", seg=32)
    P.cylz(dx, dy, ht + 0.045, ht + 0.075, 0.465, "Team", seg=32)
    P.cylz(dx, dy, ht + 0.08, ht + 0.1, 0.43, "Steel", seg=32)
    P.sphere(dx, dy, ht + 0.1, R, "Research", scale=(1, 1, 0.85), seg=32, rings=12, hemi=True)
    for deg in (28, 55):
        a = math.radians(deg)
        r, zc = R * math.cos(a), ht + 0.1 + R * 0.85 * math.sin(a)
        P.ring(dx, dy, zc - 0.008, zc + 0.008, r - 0.01, r + 0.012, "Steel", seg=32)
    top = ht + 0.1 + R * 0.85
    P.cylz(dx, dy, top - 0.015, top + 0.03, 0.07, "Steel", seg=12)
    P.cylz(dx, dy, top + 0.03, top + 0.17, 0.02, "Steel", r1=0.006, seg=8)
    P.sphere(dx, dy, top + 0.18, 0.025, "Reactor", seg=8, rings=4)
    for deg in (60, 180, 300):  # field projectors
        a = math.radians(deg)
        px, py = dx + 0.57 * math.cos(a), dy + 0.57 * math.sin(a)
        P.cbox(px, py, ht, ht + 0.2, 0.045, 0.045, 0.012, "Hull", top=(0.55, 0.55))
        P.sphere(px, py, ht + 0.22, 0.03, "Reactor", seg=8, rings=4)
    # Comms mast, uplink dish, coolant tank.
    mast(P, 0.64, 0.5, ht, 1.08, lamp="GreenLamp")
    dish(P, 0.62, -0.5, ht, 0.17, yaw=-0.9)
    tank(P, 0.62, 0.02, ht, ht + 0.16, 0.12, bands=1, seg=16)
    pipe(P, [(0.5, 0.02, ht + 0.06), (0.24, 0.02, ht + 0.06)], 0.03)
    M.add("body", P)
    return M


BUILDERS = {
    "trooper": lambda: build_infantry("trooper", False),
    "rocketeer": lambda: build_infantry("rocketeer", True),
    "buggy": build_buggy,
    "tank": build_tank,
    "harvester": build_harvester,
    "carrier": build_carrier,
    "gunship": build_gunship,
    "explorer": build_explorer,
    "mcv": build_mcv,
    "yard": build_yard,
    "power": build_power,
    "refinery": build_refinery,
    "barracks": build_barracks,
    "factory": build_factory,
    "airpad": build_airpad,
    "wall": build_wall,
    "turret": build_turret,
    "armory": build_armory,
    "tech_lab": build_tech_lab,
    "rocket_turret": build_rocket_turret,
}

REQUIRED_PARTS = {"tank": ["turret"], "turret": ["turret"], "rocket_turret": ["turret"],
                  "gunship": ["rotor"], "carrier": ["rotor", "rotor2"], "harvester": ["load"]}


# ---------------------------------------------------------------------------
# Export
# ---------------------------------------------------------------------------

def _read_accessor(doc, binary, ai):
    acc = doc["accessors"][ai]
    bv = doc["bufferViews"][acc["bufferView"]]
    ncomp = {"SCALAR": 1, "VEC2": 2, "VEC3": 3, "VEC4": 4}[acc["type"]]
    fmt = {5126: "f", 5125: "I", 5123: "H", 5121: "B"}[acc["componentType"]]
    off = bv.get("byteOffset", 0) + acc.get("byteOffset", 0)
    n = acc["count"] * ncomp
    flat = struct.unpack_from("<%d%s" % (n, fmt), binary, off)
    return [tuple(flat[k:k + ncomp]) for k in range(0, n, ncomp)] if ncomp > 1 else list(flat)


def _stable_keys(pos, nrm, idx):
    """Per-vertex (position, normal) keys that do not depend on float noise.

    Corners at the same position whose normals agree (same smooth fan) are grouped, and the
    group's normal is recomputed in Python from the triangles that touch it (angle weighted,
    summed in sorted order), so it is reproducible to the last bit."""
    P = [tuple(round(c, 6) + 0.0 for c in p) for p in pos]
    tris_of = {}
    for t in range(0, len(idx), 3):
        tri = idx[t:t + 3]
        for v in tri:
            tris_of.setdefault(v, []).append(tri)
    by_pos = {}
    for i, p in enumerate(P):
        by_pos.setdefault(p, []).append(i)
    keys = [None] * len(P)
    for p, members in by_pos.items():
        clusters = []
        for i in members:
            n = nrm[i]
            for cl in clusters:
                m = nrm[cl[0]]
                if n[0] * m[0] + n[1] * m[1] + n[2] * m[2] > 0.995:
                    cl.append(i)
                    break
            else:
                clusters.append([i])
        for cl in clusters:
            contrib = []
            for i in cl:
                for tri in tris_of.get(i, []):
                    a, b, c = (Vector(P[v]) for v in tri)
                    fn = (b - a).cross(c - a)
                    if fn.length < 1e-12:
                        continue
                    fn.normalize()
                    k = tri.index(i)
                    o = (a, b, c)
                    e1, e2 = o[(k + 1) % 3] - o[k], o[(k + 2) % 3] - o[k]
                    ang = e1.angle(e2, 0.0)
                    contrib.append((round(fn.x * ang, 9), round(fn.y * ang, 9), round(fn.z * ang, 9)))
            contrib.sort()
            sx = sum(c[0] for c in contrib)
            sy = sum(c[1] for c in contrib)
            sz = sum(c[2] for c in contrib)
            ln = math.sqrt(sx * sx + sy * sy + sz * sz)
            if ln < 1e-9:
                n = tuple(round(c, 3) + 0.0 for c in nrm[cl[0]])
            else:
                n = (round(sx / ln, 5) + 0.0, round(sy / ln, 5) + 0.0, round(sz / ln, 5) + 0.0)
            for i in cl:
                keys[i] = (p, n)
    return keys


def _canonical_buffers(doc, binary):
    """Rebuild every primitive's vertex/index data in a geometry-defined order.

    The glTF exporter de-duplicates vertices by sorting their raw bytes, and Blender's
    normals carry last-bit float noise between runs, so the vertex order (not the shape)
    can differ run to run. Quantising and sorting here makes the GLB byte-identical."""
    out = bytearray()
    views, accessors = [], []

    def push(data, target, acc):
        while len(out) % 4:
            out.append(0)
        views.append({"buffer": 0, "byteOffset": len(out), "byteLength": len(data), "target": target})
        out.extend(data)
        acc["bufferView"] = len(views) - 1
        accessors.append(acc)
        return len(accessors) - 1

    for mesh in doc["meshes"]:
        for prim in mesh["primitives"]:
            assert set(prim["attributes"]) == {"POSITION", "NORMAL"}, prim["attributes"].keys()
            assert prim.get("mode", 4) == 4
            pos = _read_accessor(doc, binary, prim["attributes"]["POSITION"])
            nrm = _read_accessor(doc, binary, prim["attributes"]["NORMAL"])
            idx = _read_accessor(doc, binary, prim["indices"])
            keys = _stable_keys(pos, nrm, idx)
            uniq = sorted(set(keys))
            remap = {k: i for i, k in enumerate(uniq)}
            tri_list = []
            for t in range(0, len(idx), 3):
                a, b, c = (remap[keys[idx[t + q]]] for q in range(3))
                m = min(a, b, c)
                while a != m:
                    a, b, c = b, c, a
                tri_list.append((a, b, c))
            tri_list.sort()
            P = [k[0] for k in uniq]
            N = [k[1] for k in uniq]
            pa = push(struct.pack("<%df" % (3 * len(P)), *[c for p in P for c in p]), 34962,
                      {"componentType": 5126, "count": len(P), "type": "VEC3",
                       "min": [min(p[i] for p in P) for i in range(3)],
                       "max": [max(p[i] for p in P) for i in range(3)]})
            na = push(struct.pack("<%df" % (3 * len(N)), *[c for nn in N for c in nn]), 34962,
                      {"componentType": 5126, "count": len(N), "type": "VEC3"})
            flat = [v for t in tri_list for v in t]
            ct, f = (5123, "H") if len(P) < 65536 else (5125, "I")
            ia = push(struct.pack("<%d%s" % (len(flat), f), *flat), 34963,
                      {"componentType": ct, "count": len(flat), "type": "SCALAR"})
            prim["attributes"] = {"POSITION": pa, "NORMAL": na}
            prim["indices"] = ia
    while len(out) % 4:
        out.append(0)
    doc["accessors"], doc["bufferViews"] = accessors, views
    doc["buffers"] = [{"byteLength": len(out)}]
    return bytes(out)


def patch_glb(path, model_name):
    """Strip the '<model>:' prefix from node and mesh names inside the GLB, and rewrite the
    binary chunk in canonical order so repeated runs give byte-identical files."""
    with open(path, "rb") as f:
        data = f.read()
    magic, version, _ = struct.unpack_from("<4sII", data, 0)
    assert magic == b"glTF", path
    jlen, jtype = struct.unpack_from("<I4s", data, 12)
    assert jtype == b"JSON"
    doc = json.loads(data[20:20 + jlen].decode("utf-8"))
    blen, btype = struct.unpack_from("<I4s", data, 20 + jlen)
    assert btype == b"BIN\x00"
    binary = data[28 + jlen:28 + jlen + blen]
    prefix = model_name + ":"
    for key in ("nodes", "meshes"):
        for n in doc.get(key, []):
            if n.get("name", "").startswith(prefix):
                n["name"] = n["name"][len(prefix):]
    for n in doc.get("nodes", []):
        n.pop("extras", None)
    binary = _canonical_buffers(doc, binary)
    js = json.dumps(doc, separators=(",", ":")).encode("utf-8")
    js += b" " * ((4 - len(js) % 4) % 4)
    total = 12 + 8 + len(js) + 8 + len(binary)
    out = struct.pack("<4sII", b"glTF", version, total)
    out += struct.pack("<I4s", len(js), b"JSON") + js
    out += struct.pack("<I4s", len(binary), b"BIN\x00") + binary
    with open(path, "wb") as f:
        f.write(out)
    return doc


def export_model(M):
    path = os.path.join(MODEL_DIR, M.name + ".glb")
    bpy.ops.object.select_all(action='DESELECT')
    for ob in M.objects():
        ob.select_set(True)
    bpy.context.view_layer.objects.active = M.root
    bpy.ops.export_scene.gltf(filepath=path, export_format='GLB', use_selection=True, export_apply=True,
                              export_yup=True, export_extras=False, export_cameras=False, export_lights=False,
                              export_animations=False)
    return patch_glb(path, M.name)


# ---------------------------------------------------------------------------
# Icons
# ---------------------------------------------------------------------------

ICON_W, ICON_H = 128, 96
PITCH = math.radians(50)


def setup_render():
    sc = bpy.context.scene
    sc.render.engine = 'BLENDER_EEVEE'
    sc.render.resolution_x = ICON_W
    sc.render.resolution_y = ICON_H
    sc.render.resolution_percentage = 100
    sc.render.film_transparent = True
    sc.render.image_settings.file_format = 'PNG'
    sc.render.image_settings.color_mode = 'RGBA'
    try:
        sc.eevee.taa_render_samples = 16
    except AttributeError:
        pass
    try:
        sc.view_settings.view_transform = 'Standard'
    except TypeError:
        print('WARN: Standard view transform unavailable')
    world = sc.world or bpy.data.worlds.new("World")
    sc.world = world
    bg = world.node_tree.nodes.get("Background")
    bg.inputs["Color"].default_value = (0.55, 0.58, 0.65, 1.0)
    bg.inputs["Strength"].default_value = 0.45

    sun_data = bpy.data.lights.new("IconSun", 'SUN')
    sun_data.energy = 2.4
    sun_data.angle = math.radians(8)
    sun = bpy.data.objects.new("IconSun", sun_data)
    sc.collection.objects.link(sun)
    # Light comes from the top-left of the icon (west, a little north, high).
    d = Vector((0.55, -0.25, -0.8)).normalized()  # direction the light travels
    sun.rotation_euler = Vector((0, 0, -1)).rotation_difference(d).to_euler()

    cam_data = bpy.data.cameras.new("IconCam")
    cam_data.type = 'ORTHO'
    cam_data.clip_start = 0.01
    cam_data.clip_end = 100.0
    cam = bpy.data.objects.new("IconCam", cam_data)
    sc.collection.objects.link(cam)
    cam.rotation_euler = Euler((math.pi / 2 - PITCH, 0.0, 0.0))
    sc.camera = cam
    return cam


def frame_and_render(cam, M, all_models):
    for other in all_models:
        hide = other is not M
        other.coll.hide_render = hide
    bpy.context.view_layer.update()
    rot = cam.rotation_euler.to_matrix()
    right, up, fwd = rot.col[0], rot.col[1], -rot.col[2]
    pts = []
    for ob in M.objects():
        if ob.type != 'MESH':
            continue
        mw = ob.matrix_world
        pts += [mw @ v.co for v in ob.data.vertices]
    xs = [p.dot(right) for p in pts]
    ys = [p.dot(up) for p in pts]
    cx, cy = (min(xs) + max(xs)) / 2, (min(ys) + max(ys)) / 2
    ex, ey = max(xs) - min(xs), max(ys) - min(ys)
    cam.data.ortho_scale = max(ex, ey * ICON_W / ICON_H) / 0.8
    centre = right * cx + up * cy
    zs = [p.dot(fwd) for p in pts]
    cam.location = centre + fwd * (min(zs) - 5.0)
    sc = bpy.context.scene
    sc.render.filepath = os.path.join(ICON_DIR, M.name + ".png")
    bpy.ops.render.render(write_still=True)


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def reset_scene():
    bpy.ops.wm.read_factory_settings(use_empty=True)
    for coll in (bpy.data.objects, bpy.data.meshes, bpy.data.materials, bpy.data.collections, bpy.data.lights,
                 bpy.data.cameras):
        for item in list(coll):
            coll.remove(item)


def main():
    os.makedirs(MODEL_DIR, exist_ok=True)
    os.makedirs(ICON_DIR, exist_ok=True)
    reset_scene()
    make_materials()

    models = []
    report = []
    for name in UNITS + STRUCTS:
        M = BUILDERS[name]()
        for req in REQUIRED_PARTS.get(name, []):
            assert req in M.parts, f"{name} missing {req}"
        models.append(M)

    for M in models:
        doc = export_model(M)
        node_names = [n["name"] for n in doc["nodes"]]
        mat_names = [m["name"] for m in doc.get("materials", [])]
        assert M.name in node_names
        for req in REQUIRED_PARTS.get(M.name, []):
            assert req in node_names, (M.name, req, node_names)
        assert "Team" in mat_names, (M.name, mat_names)
        t = sum(tris(o) for o in M.objects() if o.type == 'MESH')
        report.append((M.name, t, node_names, mat_names))

    cam = setup_render()
    for M in models:
        frame_and_render(cam, M, models)

    # Lay models out on a grid in the .blend so they can be browsed.
    for i, M in enumerate(models):
        M.root.location = ((i % 5) * 4.0, -(i // 5) * 4.0, 0.0)
        M.coll.hide_render = False
    blend = os.path.join(MODEL_DIR, "source.blend")
    bpy.ops.wm.save_as_mainfile(filepath=blend, compress=True)
    if os.path.exists(blend + "1"):
        os.remove(blend + "1")  # no backup copies in assets/

    print("\n=== Cinderfront models ===")
    for name, t, nodes, mats in report:
        print(f"{name:14s} tris={t:5d} nodes={nodes} materials={mats}")
    print("=== done ===")


if __name__ == "__main__":
    main()
