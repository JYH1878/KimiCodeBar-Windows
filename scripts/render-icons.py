#!/usr/bin/env python3
"""Render KimiCodeBar app icons from a vector master.

The original icon.png (256x256) was traced into vector form:
  - outer ring: arc, center (127.5,127.5), conic (sweep) color ramp
  - inner crescent: traced contour, linear color ramp along (x - y)

Outputs:
  src-tauri/icons/icon.svg      vector master (human-editable)
  src-tauri/icons/icon.ico      full size set 16/24/32/48/64/128/256 (PNG-compressed)
  src-tauri/icons/{32x32,128x128,128x128@2x,icon}.png

Run:  py scripts/render-icons.py        (from the repo root; needs numpy + Pillow)
"""

import math
import os

import numpy as np
from PIL import Image, ImageDraw
from scipy import ndimage

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ICONS = os.path.join(ROOT, "src-tauri", "icons")
MASTER = os.path.join(ICONS, "icon.png")
CENTER = (127.5, 127.5)
ARC_END = 274.0  # ring arc sweeps 0..274 deg (0 = east, clockwise on screen)


# ---------------------------------------------------------------- contours

def trace_loops(mask):
    """Trace closed boundary loops of a boolean mask via directed edges
    (solid kept on the left, i.e. clockwise on screen)."""
    h, w = mask.shape
    edges = {}

    def add(a, b):
        edges.setdefault(a, []).append(b)

    for y in range(h):
        for x in range(w):
            if not mask[y, x]:
                continue
            if y == 0 or not mask[y - 1, x]:
                add((x + 1, y), (x, y))
            if y == h - 1 or not mask[y + 1, x]:
                add((x, y + 1), (x + 1, y + 1))
            if x == 0 or not mask[y, x - 1]:
                add((x, y), (x, y + 1))
            if x == w - 1 or not mask[y, x + 1]:
                add((x + 1, y + 1), (x + 1, y))

    loops = []
    while edges:
        start = next(iter(edges))
        loop = [start]
        cur = start
        while True:
            outs = edges.get(cur)
            if not outs:
                break
            if len(outs) == 1:
                nxt = outs.pop()
            else:
                # ambiguous vertex (diagonal touch): sharpest right turn
                d_in = np.array(cur) - np.array(loop[-1] if len(loop) > 1 else start)
                best, best_score = None, None
                for cand in outs:
                    d_out = np.array(cand) - np.array(cur)
                    cross = d_in[0] * d_out[1] - d_in[1] * d_out[0]
                    dot = float(d_in @ d_out)
                    score = math.atan2(cross, dot)  # y-down: positive = right turn
                    if best_score is None or score > best_score:
                        best, best_score = cand, score
                outs.remove(best)
                nxt = best
            if not outs:
                del edges[cur]
            cur = nxt
            if cur == start:
                break
            loop.append(cur)
        if len(loop) > 2:
            loops.append(loop)
    return loops


def chaikin(pts, iters=2):
    """Chaikin corner-cutting smoothing for a closed polygon."""
    for _ in range(iters):
        new = []
        n = len(pts)
        for i in range(n):
            px, py = pts[i]
            qx, qy = pts[(i + 1) % n]
            new.append((0.75 * px + 0.25 * qx, 0.75 * py + 0.25 * qy))
            new.append((0.25 * px + 0.75 * qx, 0.25 * py + 0.75 * qy))
        pts = new
    return pts


# ---------------------------------------------------------------- gradients

def fit_gradients(img, ring_mask, cres_mask):
    """Fit color ramps from the original bitmap.
    Ring:     channel = a + b * theta        (theta = conic angle, deg)
    Crescent: channel = a + b * (x - y)      (linear ramp)
    Returns (ring_coefs, cres_coefs) as {channel: (a, b)} dicts.
    """
    ys, xs = np.nonzero(ring_mask)
    th = np.degrees(np.arctan2(ys - CENTER[1], xs - CENTER[0])) % 360.0
    th = np.where(th > ARC_END + 4.6, 0.0, th)  # round cap at theta=0
    ring = {}
    for ch in range(3):
        a, b = np.polyfit(th, img[ys, xs, ch], 1)
        ring[ch] = (b, a)  # value = a + b*theta -> store (intercept, slope)
    ys, xs = np.nonzero(cres_mask)
    t = (xs - ys).astype(float)
    cres = {}
    for ch in range(3):
        slope, intercept = np.polyfit(t, img[ys, xs, ch], 1)
        cres[ch] = (intercept, slope)
    return ring, cres


def ramp_colors(coefs, t):
    """Evaluate fitted ramps -> (N,3) uint8 colors."""
    cols = np.stack([coefs[ch][0] + coefs[ch][1] * t for ch in range(3)], 1)
    return np.clip(cols, 0, 255)


# ---------------------------------------------------------------- rendering

def render(size, ring_pts, cres_pts, ring_coefs, cres_coefs, ss=8):
    """Render the icon at `size` px with `ss`x supersampling."""
    S = size * ss
    k = S / 256.0
    out = np.zeros((S, S, 4), dtype=np.uint8)

    for pts, kind in ((ring_pts, "ring"), (cres_pts, "cres")):
        m = Image.new("L", (S, S), 0)
        ImageDraw.Draw(m).polygon([(x * k, y * k) for x, y in pts], fill=255)
        mask = np.array(m) > 0
        ys, xs = np.nonzero(mask)
        if kind == "ring":
            t = np.degrees(np.arctan2(ys - CENTER[1] * k, xs - CENTER[0] * k)) % 360.0
            t = np.where(t > ARC_END + 4.6, 0.0, t)
            coefs = ring_coefs
        else:
            t = (xs - ys) / k  # ramp coordinate is in 256-space units
            coefs = cres_coefs
        cols = ramp_colors(coefs, t)
        out[ys, xs, :3] = cols.astype(np.uint8)
        out[ys, xs, 3] = 255

    im = Image.fromarray(out, "RGBA")
    return im.resize((size, size), Image.LANCZOS)


# ---------------------------------------------------------------- SVG

def svg_arc_paths(ring_coefs, step=2.0):
    """Ring as overlapping arc segments with stepped conic colors."""
    cx, cy, r, sw = CENTER[0], CENTER[1], 87.5, 14.0
    parts = []
    th = 0.0
    while th < ARC_END:
        th2 = min(th + step, ARC_END)
        mid = (th + th2) / 2.0
        col = ramp_colors(ring_coefs, np.array([mid]))[0]
        # overlap neighbours by 0.6 deg to avoid AA seams
        a1 = math.radians(th - (0.6 if th > 0 else 0))
        a2 = math.radians(th2 + (0.6 if th2 < ARC_END else 0))
        x1, y1 = cx + r * math.cos(a1), cy + r * math.sin(a1)
        x2, y2 = cx + r * math.cos(a2), cy + r * math.sin(a2)
        cap = ' stroke-linecap="round"' if th == 0.0 or th2 >= ARC_END else ""
        parts.append(
            f'<path d="M {x1:.2f} {y1:.2f} A {r} {r} 0 0 1 {x2:.2f} {y2:.2f}" '
            f'fill="none" stroke="rgb({col[0]:.0f},{col[1]:.0f},{col[2]:.0f})" '
            f'stroke-width="{sw}"{cap}/>'
        )
        th = th2
    return parts


def write_svg(cres_pts, ring_coefs, cres_coefs, path):
    pts = " ".join(f"{x:.2f},{y:.2f}" for x, y in cres_pts)
    # linear ramp along (x - y): bbox diagonal of the crescent (60,195)->(172,83)
    c1 = ramp_colors(cres_coefs, np.array([60.0 - 195.0]))[0]
    c2 = ramp_colors(cres_coefs, np.array([172.0 - 83.0]))[0]
    segs = "\n    ".join(svg_arc_paths(ring_coefs))
    svg = f"""<svg xmlns="http://www.w3.org/2000/svg" width="256" height="256" viewBox="0 0 256 256">
  <defs>
    <linearGradient id="cres" x1="60" y1="195" x2="172" y2="83" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="rgb({c1[0]:.0f},{c1[1]:.0f},{c1[2]:.0f})"/>
      <stop offset="1" stop-color="rgb({c2[0]:.0f},{c2[1]:.0f},{c2[2]:.0f})"/>
    </linearGradient>
  </defs>
  <g>
    {segs}
  </g>
  <polygon points="{pts}" fill="url(#cres)"/>
</svg>
"""
    with open(path, "w", encoding="utf-8") as f:
        f.write(svg)


# ---------------------------------------------------------------- main

def main():
    img = np.array(Image.open(MASTER).convert("RGBA")).astype(float)
    solid = img[:, :, 3] > 128
    lab, n = ndimage.label(solid)
    assert n == 2, f"expected 2 shapes, got {n}"
    sizes = [(lab == i).sum() for i in (1, 2)]
    ring_mask = lab == (1 if sizes[0] > sizes[1] else 2)
    cres_mask = lab == (2 if sizes[0] > sizes[1] else 1)

    ring_coefs, cres_coefs = fit_gradients(img, ring_mask & (img[:, :, 3] > 200),
                                           cres_mask & (img[:, :, 3] > 200))
    print("ring   ramp (inter, slope/deg):", {c: tuple(round(v, 4) for v in ring_coefs[c]) for c in ring_coefs})
    print("crescent ramp (inter, slope)  :", {c: tuple(round(v, 4) for v in cres_coefs[c]) for c in cres_coefs})

    def contour(m):
        loops = trace_loops(m)
        return chaikin(max(loops, key=len), iters=2)

    ring_pts, cres_pts = contour(ring_mask), contour(cres_mask)

    # self-check: re-render at 256 and diff against the original
    chk = np.array(render(256, ring_pts, cres_pts, ring_coefs, cres_coefs)).astype(float)
    diff = np.abs(chk - img)
    both = (img[:, :, 3] > 10) | (chk[:, :, 3] > 10)
    print(f"self-check 256px: mean|dRGBA|={diff[both].mean():.2f} "
          f"max={diff[both].max():.0f} over {both.sum()} px")

    write_svg(cres_pts, ring_coefs, cres_coefs, os.path.join(ICONS, "icon.svg"))
    print("wrote icon.svg")

    for name, px in [("icon.png", 256), ("128x128@2x.png", 256),
                     ("128x128.png", 128), ("32x32.png", 32)]:
        render(px, ring_pts, cres_pts, ring_coefs, cres_coefs).save(os.path.join(ICONS, name))
        print("wrote", name)

    big = render(2048, ring_pts, cres_pts, ring_coefs, cres_coefs, ss=1)
    big.save(os.path.join(ICONS, "icon.ico"), format="ICO",
             sizes=[(s, s) for s in (16, 24, 32, 48, 64, 128, 256)])
    print("wrote icon.ico (16/24/32/48/64/128/256)")


if __name__ == "__main__":
    main()
