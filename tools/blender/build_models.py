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

    def to_object(self, name, parent, pivot=(0.0, 0.0, 0.0), coll=None):
        """Create the mesh object. `pivot` is in final (tile) coordinates relative to the parent."""
        bm = self.bm
        pv = Vector(pivot)
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

    def add(self, part_name, part, pivot=(0.0, 0.0, 0.0)):
        # Blender object names are global, so non-root objects carry a "<model>:" prefix
        # that is stripped from the GLB afterwards (see patch_glb).
        ob = part.to_object(f"{self.name}:{part_name}", self.root, pivot, self.coll)
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
    M.add("rotor", rotor_part(11.0, CK, blades=3), pivot=(12.0 * CK, 0.0, 14.0 * CK))
    M.add("rotor2", rotor_part(11.0, CK, blades=3), pivot=(-12.0 * CK, 0.0, 17.0 * CK))
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
    M.add("rotor", rotor_part(16.0, PX, blades=2), pivot=(1.0 * PX, 0.0, 10.5 * PX))
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


def base_block(P, B, ht, wall="Wall", roof="Hull", stripe=True):
    """Plinth over the whole (inset) footprint, main block set back `B.front` from the
    south edge so door details on the south face stay inside the footprint."""
    hx, hy = B.hx, B.hy
    n, s = hy - 0.02, -(hy - B.front)
    yc, hh = (n + s) / 2, (n - s) / 2
    P.box(0, 0, 0, 0.05, hx, hy, "Steel", bevel=0.01)  # plinth
    P.box(0, yc, 0.05, ht, hx - 0.02, hh, wall)
    P.box(0, yc, ht, ht + 0.03, hx - 0.06, hh - 0.04, roof, bevel=0.008)
    if stripe:
        P.box(0, yc, ht - 0.09, ht - 0.02, hx - 0.012, hh + 0.008, "Team", bevel=0.0)


def south_face(B):
    return -(B.hy - B.front)


def build_yard():
    B = Bldg("yard")
    M = Model("yard")
    P = Part()
    ht = 0.55
    base_block(P, B, ht)
    sf = south_face(B)
    for i in range(5):
        wx = B.X(0.14 + 0.18 * i)
        P.box(wx, sf, 0.16, 0.34, 0.11, 0.015, "Window", bevel=0.0)
    # Command tower.
    tx, ty = B.X(0.39), B.Y(0.48)
    P.box(tx, ty, ht, ht + 0.36, 0.75, 0.8, "Hull", taper=(0.95, 0.95))
    P.box(tx, ty, ht + 0.25, ht + 0.32, 0.725, 0.775, "Team", bevel=0.0)
    for i in range(4):
        P.box(tx - 0.52 + 0.35 * i, ty - 0.8, ht + 0.08, ht + 0.21, 0.11, 0.02, "Glass", bevel=0.0)
    P.box(tx, ty, ht + 0.36, ht + 0.39, 0.45, 0.42, "Steel", bevel=0.005)  # roof pad
    P.box(tx, ty, ht + 0.39, ht + 0.40, 0.3, 0.06, "Paint", bevel=0.0)
    P.box(tx, ty, ht + 0.39, ht + 0.40, 0.06, 0.3, "Paint", bevel=0.0)
    # Radar mast with dish.
    mx, my = B.X(0.82), B.Y(0.4)
    P.cylz(mx, my, ht, ht + 0.12, 0.12, "Steel", seg=10)
    P.cylz(mx, my, ht + 0.12, 1.0, 0.03, "Steel", seg=6)
    P.cyl((mx - 0.04, my, 0.92), (mx + 0.07, my - 0.04, 1.0), 0.03, "Paint", r1=0.2, seg=12)
    P.sphere(mx, my, 1.08, 0.035, "RedLamp", seg=6, rings=4)
    P.cylz(mx, my, 1.0, 1.06, 0.015, "Steel", seg=6)
    # Vehicle bay door on the east part of the south face.
    P.box(B.X(0.85), sf - 0.005, 0.05, 0.4, 0.28, 0.02, "Dark", bevel=0.0)
    for i in range(4):
        P.box(B.X(0.85) - 0.24 + i * 0.16, sf - 0.03, 0.41, 0.45, 0.08, 0.015, "Hazard" if i % 2 == 0 else "Dark",
              bevel=0.0)
    M.add("body", P)
    return M


def build_power():
    B = Bldg("power")
    M = Model("power")
    P = Part()
    ht = 0.28
    base_block(P, B, ht)
    cy = -0.08
    P.cyl((-0.46, cy, 0.42), (0.46, cy, 0.42), 0.07, "Steel", seg=8)  # connector pipe
    for dx in (-0.46, 0.46):
        P.cylz(dx, cy, ht, 0.72, 0.36, "Hull", r1=0.30, seg=20)  # cooling tower
        P.cylz(dx, cy, 0.50, 0.57, 0.335, "Team", seg=20)
        P.cylz(dx, cy, 0.72, 0.76, 0.31, "Steel", seg=20)  # rim
        P.cylz(dx, cy, 0.735, 0.765, 0.25, "Reactor", seg=20)  # glowing core
    P.box(0, B.Y(0.15), ht, ht + 0.08, 0.2, 0.12, "Steel")  # transformer
    for i in range(3):
        P.cylz(-0.12 + 0.12 * i, B.Y(0.15), ht + 0.08, ht + 0.16, 0.025, "Paint", seg=6)
    M.add("body", P)
    return M


def build_refinery():
    B = Bldg("refinery")
    M = Model("refinery")
    P = Part()
    ht = 0.35
    base_block(P, B, ht)
    # Ore hopper on the roof.
    hx0, hx1, hy0, hy1 = B.X(0.08), B.X(0.48), B.Y(0.88), B.Y(0.38)
    hcx, hcy, hhx, hhy = (hx0 + hx1) / 2, (hy0 + hy1) / 2, (hx1 - hx0) / 2, (hy1 - hy0) / 2
    P.box(hcx, hcy, ht + 0.02, ht + 0.06, hhx - 0.05, hhy - 0.05, "Ore", bevel=0.0)
    P.box(hcx, hcy, ht + 0.06, ht + 0.11, hhx - 0.12, hhy - 0.12, "Ore", taper=(0.7, 0.6), bevel=0.0)
    for s in (-1, 1):
        P.box(hcx, hcy + s * (hhy - 0.025), ht, ht + 0.12, hhx, 0.025, "Steel", bevel=0.005)
        P.box(hcx + s * (hhx - 0.025), hcy, ht, ht + 0.12, 0.025, hhy, "Steel", bevel=0.005)
    # Chimney.
    cx, cy = B.X(0.12), B.Y(0.2)
    P.cylz(cx, cy, ht, 1.0, 0.11, "Pipe", r1=0.09, seg=12)
    P.cylz(cx, cy, 0.86, 0.92, 0.1, "Team", seg=12)
    P.cylz(cx, cy, 0.99, 1.01, 0.07, "Dark", seg=12)
    # Silo with team band and lamp.
    sx, sy = B.X(0.78), -0.08
    P.cylz(sx, sy, ht, 0.85, 0.42, "Hull", seg=20)
    P.sphere(sx, sy, 0.85, 0.42, "Hull", scale=(1, 1, 0.35), seg=20, rings=6, hemi=True)
    P.cylz(sx, sy, 0.72, 0.8, 0.425, "Team", seg=20)
    for k in range(3):
        P.cylz(sx, sy, 0.45 + 0.08 * k, 0.47 + 0.08 * k, 0.425, "Steel", seg=20)
    P.cylz(sx, sy, 0.98, 1.02, 0.04, "Lamp", seg=8)
    # Pipe from hopper to silo.
    P.cyl((hx1, 0.1, ht + 0.1), (sx - 0.4, 0.1, ht + 0.1), 0.05, "Pipe", seg=8)
    # Unloading dock on the south face.
    P.box(B.X(0.3), south_face(B) - 0.005, 0.05, 0.25, 0.35, 0.02, "Dark", bevel=0.0)
    P.box(B.X(0.3), south_face(B) - 0.01, 0.26, 0.29, 0.38, 0.02, "Lamp", bevel=0.0)
    M.add("body", P)
    return M


def build_barracks():
    B = Bldg("barracks")
    M = Model("barracks")
    P = Part()
    ht = 0.42
    base_block(P, B, ht)
    # Corrugated roof ribs (half cylinders running north-south).
    for i in range(6):
        rx = B.X(0.13 + 0.15 * i)
        P.cyl((rx, B.Y(0.15), ht + 0.02), (rx, B.Y(0.85), ht + 0.02), 0.075, "Hull", seg=10)
    # Door.
    sf = south_face(B)
    P.box(0, sf - 0.005, 0.05, 0.33, 0.18, 0.02, "Dark", bevel=0.0)
    P.box(0, sf - 0.01, 0.33, 0.36, 0.2, 0.02, "Window", bevel=0.0)
    P.box(0, sf - 0.09, 0.0, 0.04, 0.24, 0.08, "Concrete")  # step
    for s in (-1, 1):  # sandbags
        P.box(0.55 * s, sf - 0.1, 0.0, 0.1, 0.18, 0.06, "Crate", taper=(0.9, 0.7))
    # Flag.
    fx, fy = B.X(0.88), B.Y(0.3)
    P.cylz(fx, fy, ht, 1.05, 0.015, "Paint", seg=6)
    P.box(fx - 0.17, fy, 0.86, 1.02, 0.16, 0.008, "Team", bevel=0.0, rz=0.12)
    M.add("body", P)
    return M


def build_factory():
    B = Bldg("factory")
    M = Model("factory")
    P = Part()
    ht = 0.7
    base_block(P, B, ht)
    sf = south_face(B)
    # Main vehicle door with hazard header.
    dw, dh = B.W * 0.64 / 2, ht * 0.75
    P.box(0, sf - 0.005, 0.05, dh, dw, 0.02, "Dark", bevel=0.0)
    n = 12
    for i in range(n):
        x = -dw + (i + 0.5) * 2 * dw / n
        P.box(x, sf - 0.03, dh, dh + 0.05, dw / n, 0.02, "Hazard" if i % 2 == 0 else "Dark", bevel=0.0)
    for k in range(4):
        P.box(0, sf - 0.03, 0.12 + 0.11 * k, 0.13 + 0.11 * k, dw - 0.03, 0.006, "Steel", bevel=0.0)
    for s in (-1, 1):
        P.cylz(s * (dw + 0.1), sf - 0.03, dh + 0.04, dh + 0.1, 0.035, "Lamp", seg=8)
    # Upper workshop block.
    px0, px1, py0, py1 = B.X(0.1), B.X(0.65), B.Y(0.72), B.Y(0.22)
    P.box((px0 + px1) / 2, (py0 + py1) / 2, ht, ht + 0.16, (px1 - px0) / 2, (py1 - py0) / 2, "Hull")
    P.box((px0 + px1) / 2, (py0 + py1) / 2, ht + 0.08, ht + 0.13, (px1 - px0) / 2 + 0.005,
          (py1 - py0) / 2 + 0.005, "Team", bevel=0.0)
    for i in range(5):  # skylights
        P.box(px0 + 0.15 + i * 0.29, (py0 + py1) / 2, ht + 0.16, ht + 0.18, 0.08, 0.3, "Glass", bevel=0.0)
    # Vent fans.
    for fx in (0.77, 0.9):
        vx, vy = B.X(fx), B.Y(0.45)
        P.cylz(vx, vy, ht, ht + 0.07, 0.17, "Steel", seg=16)
        P.cylz(vx, vy, ht + 0.07, ht + 0.075, 0.14, "Dark", seg=16)
        for a in (0.3, 0.3 + math.pi / 2):
            P.box(vx, vy, ht + 0.07, ht + 0.09, 0.14, 0.02, "Hull", bevel=0.0, rz=a)
    # Smoke stacks at the back.
    for x in (B.X(0.75), B.X(0.88)):
        P.cylz(x, B.Y(0.15), ht, 1.05, 0.06, "Pipe", seg=10)
        P.cylz(x, B.Y(0.15), 0.95, 1.0, 0.065, "Team", seg=10)
    M.add("body", P)
    return M


def build_airpad():
    B = Bldg("airpad")
    M = Model("airpad")
    P = Part()
    ht = 0.1
    P.box(0, 0, 0, ht, B.hx, B.hy, "Asphalt", bevel=0.02)
    t, inset = 0.03, 0.09
    ex, ey = B.hx - inset, B.hy - inset
    z0, z1 = ht - 0.002, ht + 0.006
    for s in (-1, 1):  # team border
        P.box(0, s * ey, z0, z1, ex + t, t, "Team", bevel=0.0)
        P.box(s * ex, 0, z0, z1, t, ey - t, "Team", bevel=0.0)
    P.ring(0, 0, z0, z1, 0.56, 0.62, "Paint", seg=32)
    for s in (-1, 1):  # "H"
        P.box(0.17 * s, 0, z0, z1, 0.035, 0.24, "Paint", bevel=0.0)
    P.box(0, 0, z0, z1, 0.14, 0.035, "Paint", bevel=0.0)
    for a in (-1, 1):  # corner beacons
        for b in (-1, 1):
            x, y = a * (B.hx - 0.05), b * (B.hy - 0.05)
            P.cylz(x, y, ht, ht + 0.05, 0.03, "Steel", seg=8)
            P.cylz(x, y, ht + 0.05, ht + 0.08, 0.025, "GreenLamp", seg=8)
    M.add("body", P)
    return M


def build_wall():
    M = Model("wall")
    P = Part()
    h = 0.5
    P.box(0, 0, 0, 0.3, 0.5, 0.5, "Concrete", bevel=0.015)
    P.box(0, 0, 0.3, 0.36, 0.495, 0.495, "Team", bevel=0.0)
    P.box(0, 0, 0.36, h, 0.5, 0.5, "Concrete", bevel=0.015)
    P.box(0, 0, h, h + 0.015, 0.3, 0.25, "Wall", bevel=0.005)
    M.add("body", P)
    return M


def turret_base(P, B, ht=0.18):
    P.box(0, 0, 0, 0.04, B.hx, B.hy, "Steel", bevel=0.01)
    P.box(0, 0, 0.04, ht, B.hx - 0.02, B.hy - 0.02, "Wall", taper=(0.9, 0.9))
    for a in (-1, 1):
        for b in (-1, 1):
            P.box(a * (B.hx - 0.1), b * (B.hy - 0.1), ht - 0.06, ht - 0.03, 0.05, 0.05, "Team", bevel=0.0)
    P.cylz(0, 0, ht, ht + 0.025, 0.3, "Steel", seg=20)


def build_turret():
    B = Bldg("turret")
    M = Model("turret")
    P = Part()
    turret_base(P, B)
    M.add("body", P)
    T = Part()
    z = 0.205
    T.cylz(0, 0, z, z + 0.04, 0.24, "Hull", seg=16)
    T.box(-0.025, 0, z + 0.03, z + 0.18, 0.2, 0.16, "Team", taper=(0.85, 0.85))
    T.box(0.16, 0, z + 0.05, z + 0.15, 0.04, 0.12, "Hull")
    for s in (-1, 1):
        T.cyl((0.18, 0.065 * s, z + 0.1), (0.44, 0.065 * s, z + 0.1), 0.025, "Steel", seg=8)
        T.cyl((0.40, 0.065 * s, z + 0.1), (0.44, 0.065 * s, z + 0.1), 0.035, "Steel", seg=8)
    T.box(-0.08, 0.08, z + 0.18, z + 0.21, 0.04, 0.03, "Glass", bevel=0.0)
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
    T.cylz(0, 0, z, z + 0.03, 0.2, "Hull", seg=16)
    T.box(0, 0, z + 0.03, z + 0.13, 0.11, 0.11, "Steel")
    T.cyl((0, -0.21, z + 0.17), (0, 0.21, z + 0.17), 0.03, "Steel", seg=8)  # pod axle
    for s in (-1, 1):
        cy = 0.14 * s
        T.box(-0.025, cy, z + 0.1, z + 0.27, 0.25, 0.085, "Team")
        T.box(-0.025, cy, z + 0.27, z + 0.285, 0.2, 0.06, "Hull", bevel=0.0)
        for k in (-1, 1):
            for zz in (z + 0.145, z + 0.225):
                T.cyl((0.2, cy + 0.04 * k, zz), (0.235, cy + 0.04 * k, zz), 0.03, "Dark", seg=8)
                T.cyl((0.205, cy + 0.04 * k, zz), (0.245, cy + 0.04 * k, zz), 0.018, "Red", r1=0.0, seg=8)
    M.add("turret", T, pivot=(0.0, 0.0, z))
    return M


def build_armory():
    B = Bldg("armory")
    M = Model("armory")
    P = Part()
    ht = 0.4
    base_block(P, B, ht)
    for i, (fx, fy) in enumerate(((0.12, 0.3), (0.42, 0.3), (0.12, 0.62), (0.42, 0.62))):
        cx, cy = B.X(fx + 0.12), B.Y(fy + 0.11)
        m = "Olive" if i % 2 == 0 else "Crate"
        P.box(cx, cy, ht + 0.03, ht + 0.15, 0.22, 0.2, m)
        P.box(cx, cy, ht + 0.08, ht + 0.1, 0.225, 0.205, "Steel", bevel=0.0)
    # Missile rack.
    mx = B.X(0.82)
    for k, my in enumerate((0.25, -0.05, -0.35)):
        P.cylz(mx, my, ht + 0.03, ht + 0.4, 0.07, "Red", seg=10)
        P.cylz(mx, my, ht + 0.4, ht + 0.5, 0.07, "Paint", r1=0.0, seg=10)
        P.cylz(mx, my, ht + 0.12, ht + 0.16, 0.072, "Team", seg=10)
    P.box(mx, -0.05, ht + 0.03, ht + 0.08, 0.12, 0.45, "Steel")
    # Blast door.
    sf = south_face(B)
    P.box(0, sf - 0.005, 0.05, 0.3, 0.38, 0.02, "Steel", bevel=0.0)
    P.box(0, sf - 0.03, 0.05, 0.3, 0.008, 0.008, "Hull", bevel=0.0)
    for k in range(3):
        P.box(0, sf - 0.03, 0.09 + 0.07 * k, 0.11 + 0.07 * k, 0.36, 0.006, "Hazard", bevel=0.0)
    P.cylz(0, sf - 0.03, 0.33, 0.37, 0.03, "Lamp", seg=8)
    M.add("body", P)
    return M


def build_tech_lab():
    B = Bldg("tech_lab")
    M = Model("tech_lab")
    P = Part()
    ht = 0.45
    base_block(P, B, ht)
    sf = south_face(B)
    for i in range(5):
        P.box(B.X(0.13 + 0.185 * i), sf - 0.005, 0.15, 0.32, 0.09, 0.015, "Glass", bevel=0.0)
    dx, dy = B.X(0.4), -0.08
    P.cylz(dx, dy, ht, ht + 0.1, 0.42, "Hull", seg=24)
    P.cylz(dx, dy, ht + 0.06, ht + 0.1, 0.425, "Team", seg=24)
    P.sphere(dx, dy, ht + 0.1, 0.36, "Research", scale=(1, 1, 0.8), seg=20, rings=8, hemi=True)
    for a in range(4):  # dome ribs
        ang = a * math.pi / 4
        P.box(dx, dy, ht + 0.1, ht + 0.115, 0.37, 0.012, "Steel", bevel=0.0, rz=ang)
    mx, my = B.X(0.84), 0.0
    P.box(mx, my, ht, ht + 0.06, 0.1, 0.1, "Steel")
    P.cylz(mx, my, ht + 0.06, 1.08, 0.022, "Steel", seg=6)
    for k in range(3):
        P.box(mx, my, ht + 0.22 + 0.15 * k, ht + 0.24 + 0.15 * k, 0.1 - 0.02 * k, 0.012, "Hull", bevel=0.0)
    P.sphere(mx, my, 1.1, 0.035, "GreenLamp", seg=8, rings=4)
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
