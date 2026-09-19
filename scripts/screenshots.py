#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Renders the images under site/img/ from sources in the repository.

- OLED pages: `cargo run -p argon-device --example oled_pages` draws the status page with the
  same code argond uses, from made-up readings; each is scaled up with an OLED look.
- Terminal: every site/terminal/*.txt -- output captured from real machines, checked for
  identifying data before it was committed -- becomes a terminal-window SVG.

No image here is a screen capture of a real desktop, so none can show anything private.

    ./scripts/screenshots.py
"""
import html
import pathlib
import subprocess
import sys
import tempfile

from PIL import Image, ImageDraw, ImageFilter

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "site" / "img"

SCALE = 5                   # screen pixels per OLED pixel
LIT = (205, 232, 255)       # a white OLED, faintly blue
DARK = (6, 9, 13)
BEZEL = (28, 31, 36)


def oled_pngs(tmp: pathlib.Path) -> None:
    subprocess.run(
        ["cargo", "run", "-q", "-p", "argon-device", "--example", "oled_pages", "--", str(tmp)],
        cwd=ROOT, check=True,
    )
    for pbm in sorted(tmp.glob("oled-*.pbm")):
        img = Image.open(pbm).convert("1")
        w, h = img.size
        panel = Image.new("RGB", (w * SCALE, h * SCALE), DARK)
        draw = ImageDraw.Draw(panel)
        for y in range(h):
            for x in range(w):
                if img.getpixel((x, y)) == 0:   # PBM: 1 = black = lit segment
                    x0, y0 = x * SCALE, y * SCALE
                    draw.rectangle((x0, y0, x0 + SCALE - 2, y0 + SCALE - 2), fill=LIT)
        glow = panel.filter(ImageFilter.GaussianBlur(3))
        panel = Image.blend(glow, panel, 0.75)
        pad = 18
        framed = Image.new("RGB", (panel.width + 2 * pad, panel.height + 2 * pad), (0, 0, 0))
        fd = ImageDraw.Draw(framed)
        fd.rounded_rectangle((0, 0, framed.width - 1, framed.height - 1), radius=18, fill=BEZEL)
        framed.paste(panel, (pad, pad))
        target = OUT / (pbm.stem + ".png")
        framed.save(target, optimize=True)
        print(f"wrote {target.relative_to(ROOT)}")


COLS = 96                   # every terminal the same width, so all render at the same size


def wrap(line: str) -> list[str]:
    """Splits a line longer than COLS, continuing with an indent, at spaces where possible."""
    out, indent = [], "    "
    while len(line) > COLS:
        cut = line.rfind(" ", len(indent) + 1, COLS)
        cut = cut if cut > 0 else COLS
        out.append(line[:cut])
        line = indent + line[cut:].lstrip()
    out.append(line)
    return out


def terminal_svg(src: pathlib.Path) -> None:
    lines = [w for l in src.read_text().rstrip("\n").split("\n") for w in wrap(l)]
    char_w, line_h, pad, bar = 8.4, 19, 16, 30
    cols = COLS
    width = int(cols * char_w + 2 * pad)
    height = int(len(lines) * line_h + 2 * pad + bar)
    out = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" '
        f'viewBox="0 0 {width} {height}" role="img" aria-label="{html.escape(lines[0])}">',
        f'<rect width="{width}" height="{height}" rx="10" fill="#0f1419"/>',
        f'<rect width="{width}" height="{bar}" rx="10" fill="#1d232b"/>',
        f'<rect y="{bar - 10}" width="{width}" height="10" fill="#1d232b"/>',
    ]
    for i, colour in enumerate(("#ff5f57", "#febc2e", "#28c840")):
        out.append(f'<circle cx="{18 + i * 20}" cy="15" r="6" fill="{colour}"/>')
    out.append(
        '<g font-family="DejaVu Sans Mono, Menlo, Consolas, monospace" font-size="14" '
        'xml:space="preserve">'
    )
    for i, line in enumerate(lines):
        y = bar + pad + (i + 1) * line_h - 5
        colour = "#7ee787" if i == 0 else "#d6deeb"
        # Non-breaking spaces: renderers collapse runs of ordinary ones, and alignment is the point.
        text = html.escape(line).replace(" ", "\u00a0")
        out.append(f'<text x="{pad}" y="{y}" fill="{colour}">{text}</text>')
    out.append("</g></svg>")
    target = OUT / (src.stem + ".svg")
    target.write_text("\n".join(out) + "\n")
    print(f"wrote {target.relative_to(ROOT)}")


def main() -> int:
    OUT.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        oled_pngs(pathlib.Path(tmp))
    for src in sorted((ROOT / "site" / "terminal").glob("*.txt")):
        terminal_svg(src)
    return 0


if __name__ == "__main__":
    sys.exit(main())
