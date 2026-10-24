#!/usr/bin/env python3
"""Generate assets/icon/tandem.iconset and tandem.icns (needs Pillow + macOS iconutil)."""
import os, subprocess, sys
from PIL import Image, ImageDraw

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "assets", "icon")
SET = os.path.join(ROOT, "tandem.iconset")
BG, BLUE, GREEN = "#1E1F22", "#3574F0", "#5FB865"
N, SS = 1024, 2  # master size, supersample


def ring(size, box, radius, width, color):
    layer = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    ImageDraw.Draw(layer).rounded_rectangle(box, radius=radius, outline=color, width=width)
    return layer


def master():
    s = N * SS
    k = s / N
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    m = int(100 * k)  # macOS icon grid margin
    ImageDraw.Draw(img).rounded_rectangle((m, m, s - m, s - m), radius=int(185 * k), fill=BG)
    w = int(76 * k)
    r = int(150 * k)
    # two interleaved rounded-rect links, offset diagonally
    blue_box = tuple(int(v * k) for v in (250, 300, 640, 700))
    green_box = tuple(int(v * k) for v in (384, 324, 774, 724))
    blue = ring(s, blue_box, r, w, BLUE)
    green = ring(s, green_box, r, w, GREEN)
    img.alpha_composite(blue)
    img.alpha_composite(green)
    # blue passes over green on the upper crossing only -> interleaved
    clip = Image.new("L", (s, s), 0)
    ImageDraw.Draw(clip).rectangle((int(384 * k), 0, int(640 * k), int(512 * k)), fill=255)
    over = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    over.paste(blue, (0, 0), clip)
    img.alpha_composite(over)
    return img.resize((N, N), Image.LANCZOS)


def main():
    os.makedirs(SET, exist_ok=True)
    base = master()
    for pt in (16, 32, 128, 256, 512):
        base.resize((pt, pt), Image.LANCZOS).save(os.path.join(SET, f"icon_{pt}x{pt}.png"))
        base.resize((pt * 2, pt * 2), Image.LANCZOS).save(os.path.join(SET, f"icon_{pt}x{pt}@2x.png"))
    base.save(os.path.join(ROOT, "tandem-1024.png"))
    subprocess.check_call(["iconutil", "-c", "icns", SET, "-o", os.path.join(ROOT, "tandem.icns")])
    print("wrote", os.path.join(ROOT, "tandem.icns"))


if __name__ == "__main__":
    sys.exit(main())
