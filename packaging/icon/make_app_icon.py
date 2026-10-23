#!/usr/bin/env python3
"""Builds the program icon (app.ico, app.png, app-256.png) and the README logo
(logo-small.png) from the logo (portal.png).

    python3 packaging/icon/make_app_icon.py

At icon sizes the "portal" wordmark is unreadable, so this keeps only the
mark, crops it tightly and centers it on a rounded tile in the logo's
background color. Needs Pillow and numpy.
"""

import pathlib

import numpy as np
from PIL import Image, ImageDraw

HERE = pathlib.Path(__file__).resolve().parent
SIZE = 1024
# The mark fills this share of the tile.
FILL = 0.74


def main():
    logo = Image.open(HERE / "portal.png").convert("RGB")
    pixels = np.asarray(logo).astype(int)
    background = tuple(int(c) for c in np.median(pixels[:40, :40].reshape(-1, 3), axis=0))
    ink = np.abs(pixels - background).sum(axis=2) > 120

    # The mark is the top block of ink; the wordmark starts after the first
    # empty band of rows below it.
    rows = np.flatnonzero(ink.any(axis=1))
    gaps = np.flatnonzero(np.diff(rows) > 20)
    mark_bottom = rows[gaps[0]] if len(gaps) else rows[-1]
    mark = ink[: mark_bottom + 1]
    cols = np.flatnonzero(mark.any(axis=0))
    top = rows[0]
    box = (cols[0], top, cols[-1] + 1, mark_bottom + 1)
    cropped = logo.crop(box)

    scale = SIZE * FILL / max(cropped.size)
    w, h = round(cropped.width * scale), round(cropped.height * scale)
    cropped = cropped.resize((w, h), Image.LANCZOS)

    tile = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    big = SIZE * 4
    mask = Image.new("L", (big, big), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, big - 1, big - 1], radius=big * 0.22, fill=255)
    mask = mask.resize((SIZE, SIZE), Image.LANCZOS)
    tile.paste(Image.new("RGB", (SIZE, SIZE), background), (0, 0), mask)
    tile.paste(cropped, ((SIZE - w) // 2, (SIZE - h) // 2))

    tile.save(HERE / "app.png")
    # Built into the Linux binary for its right-click menu entries.
    tile.resize((256, 256), Image.LANCZOS).save(HERE / "app-256.png", optimize=True)
    tile.save(
        HERE / "app.ico",
        sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
    )
    logo.resize((480, 480), Image.LANCZOS).save(HERE / "logo-small.png", optimize=True)
    print(f"Wrote app.png, app-256.png, app.ico and logo-small.png (mark at {box})")


if __name__ == "__main__":
    main()
