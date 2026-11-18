"""Draw the installer artwork in the app's style (dark surfaces, pink accent, Inter).

    python build\\installer_art.py        (dev only; needs Pillow)

Writes build/installer/wizard-<w>x<h>.png (the tall image on the Welcome / Finished pages) and
build/installer/small-<w>x<h>.png (the icon in the top-right of the other pages), one per size Inno Setup
picks from for 100-250% display scaling. Colours are the app's theme tokens (crates/vl-app/src/theme.rs).
"""
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "build" / "installer"
DEEP = (0x0F, 0x0F, 0x10)
BG = (0x14, 0x14, 0x15)
TEXT = (0xED, 0xED, 0xED)
SECONDARY = (0xA1, 0xA1, 0xA6)
ACCENT = (0xF4, 0xA6, 0xC4)

# Inno Setup's recommended sizes for 100, 125, 150, 175, 200, 225 and 250% scaling
WIZARD = [(164, 314), (192, 386), (246, 459), (273, 556), (328, 604), (355, 700), (410, 797)]
SMALL = [(55, 55), (64, 68), (69, 69), (83, 80), (92, 97), (110, 106), (119, 123)]


def inter(size: int, weight: str) -> ImageFont.FreeTypeFont:
    f = ImageFont.truetype(str(ROOT / "assets" / "fonts" / "Inter-Variable.ttf"), size)
    try:
        f.set_variation_by_name(weight)
    except Exception:
        pass
    return f


def wizard(w: int, h: int) -> Image.Image:
    s = w / 164.0
    img = Image.new("RGB", (w, h), DEEP)
    # a soft pink glow behind the mark
    glow = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    gd = ImageDraw.Draw(glow)
    cx, cy, r = w // 2, int(h * 0.36), int(w * 0.55)
    gd.ellipse((cx - r, cy - r, cx + r, cy + r), fill=ACCENT + (46,))
    glow = glow.filter(ImageFilter.GaussianBlur(radius=int(38 * s)))
    img.paste(glow, (0, 0), glow)
    # the app icon
    icon = Image.open(ROOT / "assets" / "icon.png").convert("RGBA")
    isz = int(84 * s)
    icon = icon.resize((isz, isz), Image.LANCZOS)
    img.paste(icon, (cx - isz // 2, cy - isz // 2), icon)
    d = ImageDraw.Draw(img)
    # wordmark and tagline
    title = inter(int(21 * s), "SemiBold")
    tb = d.textbbox((0, 0), "ValoLights", font=title)
    ty = cy + isz // 2 + int(20 * s)
    d.text((cx - (tb[2] - tb[0]) // 2, ty), "ValoLights", font=title, fill=TEXT)
    sub = inter(int(10.5 * s), "Regular")
    for i, line in enumerate(["Valorant highlights,", "edited on your PC"]):
        sb = d.textbbox((0, 0), line, font=sub)
        d.text((cx - (sb[2] - sb[0]) // 2, ty + int(34 * s) + i * int(15 * s)), line, font=sub, fill=SECONDARY)
    # a thin accent line at the bottom
    d.rectangle((int(w * 0.38), h - int(28 * s), int(w * 0.62), h - int(28 * s) + max(2, int(2 * s))), fill=ACCENT)
    # the right edge meets the page background
    d.rectangle((w - max(1, int(s)), 0, w, h), fill=BG)
    return img


def small(w: int, h: int) -> Image.Image:
    img = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    icon = Image.open(ROOT / "assets" / "icon.png").convert("RGBA")
    n = min(w, h)
    icon = icon.resize((n, n), Image.LANCZOS)
    img.paste(icon, ((w - n) // 2, (h - n) // 2), icon)
    return img


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    for w, h in WIZARD:
        wizard(w, h).save(OUT / f"wizard-{w}x{h}.png")
    for w, h in SMALL:
        small(w, h).save(OUT / f"small-{w}x{h}.png")
    print("written to", OUT)


if __name__ == "__main__":
    main()
