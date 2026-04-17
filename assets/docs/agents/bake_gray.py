#!/usr/bin/env python3
"""Bake a light-gray background behind every agent logo for the README grid.

Why: GitHub-flavored markdown strips `bgcolor`/`style` attrs, so we can't
apply a background via HTML/CSS in the README. Instead we bake the gray
canvas directly into each image.

Usage:
    # Drop a new logo (PNG or SVG) into assets/docs/agents/, then run:
    python3 assets/docs/agents/bake_gray.py

    # Outputs land in assets/docs/agents/gray/ — reference those in README.md.
    # In README, render at `width="160"` to match the 2:1 canvas.

Tweakables below (BG color, canvas size, logo max size). Source logos are
never upscaled past their native size, so give it a high-res PNG when
possible (or an SVG).
"""
from pathlib import Path

from PIL import Image

SRC = Path(__file__).resolve().parent
DST = SRC / "gray"
DST.mkdir(exist_ok=True)

BG = (234, 238, 242, 255)   # #eaeef2 — tweak if too light/dark
CANVAS_W = 600              # rectangular canvas; 2x retina of 300x150 display
CANVAS_H = 300
LOGO_MAX_W = 480            # logo padding = (CANVAS - LOGO_MAX) / 2 per side
LOGO_MAX_H = 200


def bake_png(src: Path) -> None:
    img = Image.open(src).convert("RGBA")
    w, h = img.size
    # Fill the logo box; allow upscaling small sources so padding is consistent.
    scale = min(LOGO_MAX_W / w, LOGO_MAX_H / h)
    nw, nh = max(1, int(w * scale)), max(1, int(h * scale))
    if (nw, nh) != (w, h):
        img = img.resize((nw, nh), Image.LANCZOS)
    canvas = Image.new("RGBA", (CANVAS_W, CANVAS_H), BG)
    canvas.paste(img, ((CANVAS_W - nw) // 2, (CANVAS_H - nh) // 2), img)
    out = DST / src.name
    canvas.convert("RGB").save(out, "PNG", optimize=True)
    print(f"wrote {out.relative_to(SRC.parent.parent.parent)} ({nw}x{nh})")


def bake_svg_to_png(src: Path) -> bool:
    """Rasterize an SVG to PNG via cairosvg, then bake like any PNG.
    Returns True if handled, False if cairosvg isn't installed.
    """
    try:
        import cairosvg  # type: ignore
    except ImportError:
        return False
    tmp = DST / (src.stem + "._tmp.png")
    cairosvg.svg2png(url=str(src), write_to=str(tmp),
                     output_width=LOGO_MAX_W * 2)
    try:
        img = Image.open(tmp).convert("RGBA")
        w, h = img.size
        scale = min(LOGO_MAX_W / w, LOGO_MAX_H / h, 1.0)
        nw, nh = max(1, int(w * scale)), max(1, int(h * scale))
        if (nw, nh) != (w, h):
            img = img.resize((nw, nh), Image.LANCZOS)
        canvas = Image.new("RGBA", (CANVAS_W, CANVAS_H), BG)
        canvas.paste(img, ((CANVAS_W - nw) // 2, (CANVAS_H - nh) // 2), img)
        out = DST / (src.stem + ".png")
        canvas.convert("RGB").save(out, "PNG", optimize=True)
        print(f"wrote {out.relative_to(SRC.parent.parent.parent)} "
              f"({nw}x{nh}, from svg)")
    finally:
        tmp.unlink(missing_ok=True)
    return True


def bake_svg(src: Path) -> None:
    # Extract the source SVG's viewBox + inner markup, then re-emit inside
    # an outer SVG with a gray background. We avoid nesting <svg> tags
    # because GitHub's sanitizer / some renderers drop the inner one;
    # instead we scale the source's contents via a <g transform>.
    import re

    raw = src.read_text()
    vb = re.search(r'viewBox\s*=\s*"([^"]+)"', raw)
    if not vb:
        raise ValueError(f"{src} has no viewBox; add one or bake a PNG")
    vx, vy, vw, vh = [float(x) for x in vb.group(1).split()]

    # Strip <?xml?>, outer <svg ...> and </svg> to get just the contents.
    body = re.sub(r'<\?xml[^>]*\?>', '', raw)
    body = re.sub(r'<svg[^>]*>', '', body, count=1)
    body = re.sub(r'</svg>\s*$', '', body.strip())

    # Fit (vw x vh) into (LOGO_MAX_W x LOGO_MAX_H), centered on canvas.
    scale = min(LOGO_MAX_W / vw, LOGO_MAX_H / vh)
    dw, dh = vw * scale, vh * scale
    tx = (CANVAS_W - dw) / 2 - vx * scale
    ty = (CANVAS_H - dh) / 2 - vy * scale

    wrapped = (
        f'<svg xmlns="http://www.w3.org/2000/svg" '
        f'viewBox="0 0 {CANVAS_W} {CANVAS_H}" '
        f'width="{CANVAS_W}" height="{CANVAS_H}">\n'
        f'  <rect width="100%" height="100%" '
        f'fill="#{BG[0]:02x}{BG[1]:02x}{BG[2]:02x}"/>\n'
        f'  <g transform="translate({tx:.3f} {ty:.3f}) scale({scale:.6f})">\n'
        f'    {body.strip()}\n'
        f'  </g>\n'
        f'</svg>\n'
    )
    out = DST / src.name
    out.write_text(wrapped)
    print(f"wrote {out.relative_to(SRC.parent.parent.parent)} (svg)")


def main() -> None:
    for p in sorted(SRC.glob("*.png")):
        bake_png(p)
    for p in sorted(SRC.glob("*.svg")):
        # Prefer PNG output for consistency. Falls back to an SVG wrapper
        # if cairosvg isn't installed (pip install cairosvg to enable).
        if not bake_svg_to_png(p):
            print(f"note: cairosvg not installed — wrapping {p.name} as SVG "
                  f"instead. `pip install cairosvg` for PNG output.")
            bake_svg(p)


if __name__ == "__main__":
    main()
