"""Builds each Ember mod's icon and Steam artwork, in the mod's own colour, from Ember's own artwork (Pillow).

The layout is SFxT Ember's (its scripts/brand/build-sfxt-brand.py): Ember's mark (火 in an open brush circle) and
the ink-landscape menu background, both drawn for SF4 Ember, recoloured per game, then laid out at the sizes Steam
uses for a non-Steam shortcut, plus a 16-256 .ico. Each game's recolour is the one its own menus use:

  usf4   Ember's own orange, unchanged
  sfxt   orange to red, hue turned and lightness kept (SFxT's RoomScreen.cxx UseLook); matches SFxT's files
  umvc3  red and blue channels swapped, so orange becomes cobalt and the cream a cool ivory (UMvC3's Theme.hxx)
  sfv    orange to SFV's gold and the warm greys to charcoal (SFV Ember's Band theme); SFV's own mark export

  python tools/brand/build-brand.py --ember <SF4 Ember checkout> [--sfv-mark <sfv-ember-mark.png>] [--only usf4,...]
  python tools/brand/build-brand.py ... --check     rebuild in memory, fail when a checked-in file differs

Output: app/ui/brand/<id>/<id>-ember-{icon,wide,portrait,hero,logo,mark}.png and <id>-ember.ico.
The artwork uses no Capcom artwork or logos; the game names appear only as part of each project's name.
"""
import argparse
import colorsys
import io
import sys
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFont, ImageStat

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "app" / "ui" / "brand"

WIDE, PORTRAIT, HERO = (920, 430), (600, 900), (1920, 620)
LOGO_HEIGHT, ICON = 400, 256
ICO_SIZES = (16, 24, 32, 48, 64, 128, 256)
CHECK_TOLERANCE = 1.5
CREAM = (243, 235, 221)  # ember-mark.svg #F3EBDD


def hsv_map(fn, min_saturation=0.35):
    """A per-colour recolour through HSV: fn(h, s, v, saturated) -> (h, s, v)."""
    def apply(rgb):
        r, g, b = rgb
        h, s, v = colorsys.rgb_to_hsv(r / 255, g / 255, b / 255)
        h, s, v = fn(h, s, v, s >= min_saturation)
        r2, g2, b2 = colorsys.hsv_to_rgb(h % 1.0, max(0, min(1, s)), max(0, min(1, v)))
        return round(r2 * 255), round(g2 * 255), round(b2 * 255)
    return apply


def identity(rgb):
    return rgb


def swap_red_blue(rgb):
    return rgb[2], rgb[1], rgb[0]


# SFxT: hue 24 -> 0 on clearly coloured pixels only, as its own script does.
sfxt_map = hsv_map(lambda h, s, v, sat: (h - 24 / 360, s, v) if sat else (h, s, v))

# SFV: Ember's orange #FF8738 (24 deg, s .78, v 1) lands exactly on SFV's gold #DDB961 (43 deg, s .56, v .87);
# the warm greys lose most of their tint, towards SFV's neutral charcoal bands.
_ORANGE = colorsys.rgb_to_hsv(255 / 255, 135 / 255, 56 / 255)
_GOLD = colorsys.rgb_to_hsv(221 / 255, 185 / 255, 97 / 255)
sfv_map = hsv_map(lambda h, s, v, sat: (h + _GOLD[0] - _ORANGE[0], s * _GOLD[1] / _ORANGE[1], v * _GOLD[2] / _ORANGE[2])
                  if sat else (h, s * 0.25, v))

LOOKS = {
    "usf4": dict(word="USF4", accent_at=3, map=identity, accent=(255, 135, 56), light=(255, 177, 111),
                 ground=(58, 42, 31), text=CREAM, radius=0.18),
    "sfxt": dict(word="SFxT", accent_at=2, map=sfxt_map, accent=(255, 85, 85), light=(255, 120, 104),
                 ground=(58, 32, 31), text=CREAM, radius=0.18),
    "umvc3": dict(word="UMvC3", accent_at=2, map=swap_red_blue, accent=(56, 135, 255), light=(111, 177, 255),
                  ground=(31, 42, 58), text=(221, 235, 243), radius=0.18),
    # SFV's corners are square (Band theme); its tile is the theme's button charcoal.
    "sfv": dict(word="SFV", accent_at=2, map=sfv_map, accent=(221, 185, 97), light=(242, 221, 157),
                ground=(47, 46, 42), text=(237, 236, 224), radius=0.04),
}


def recolour(image, fn):
    rgba = image.convert("RGBA")
    table = {}
    out = []
    for r, g, b, a in (rgba.get_flattened_data() if hasattr(rgba, "get_flattened_data") else rgba.getdata()):
        key = (r, g, b)
        if key not in table:
            table[key] = fn(key)
        out.append(table[key] + (a,))
    img = Image.new("RGBA", rgba.size)
    img.putdata(out)
    return img


def cover(image, size, anchor_x=0.5):
    scale = max(size[0] / image.width, size[1] / image.height)
    scaled = image.resize((round(image.width * scale), round(image.height * scale)), Image.Resampling.LANCZOS)
    left = round((scaled.width - size[0]) * anchor_x)
    top = (scaled.height - size[1]) // 2
    return scaled.crop((left, top, left + size[0], top + size[1]))


def fit(image, height):
    return image.resize((round(image.width * height / image.height), height), Image.Resampling.LANCZOS)


def shade_bottom(image, strength):
    w, h = image.size
    alpha = Image.linear_gradient("L").resize((w, h)).point(lambda v: round(v * strength))
    dark = Image.new("RGBA", (w, h), (0, 0, 0, 255))
    dark.putalpha(alpha)
    return Image.alpha_composite(image, dark)


def wordmark(look, font, height):
    """The game's short name over 'EMBER NETPLAY'; one letter in the accent colour, as SFxT's 'x'."""
    # Basic layout (no raqm kerning) on every machine: SFxT's files were built that way.
    top = ImageFont.truetype(str(font), round(height * 0.62), layout_engine=ImageFont.Layout.BASIC)
    bottom = ImageFont.truetype(str(font), round(height * 0.24), layout_engine=ImageFont.Layout.BASIC)
    gap = round(height * 0.06)
    word, at = look["word"], look["accent_at"]
    tb = top.getbbox(word)
    bb = bottom.getbbox("EMBER NETPLAY")
    width = max(tb[2] - tb[0], bb[2] - bb[0])
    out = Image.new("RGBA", (width, height), (0, 0, 0, 0))
    draw = ImageDraw.Draw(out)
    # As SFxT's script: the whole word, then the accent letter and the rest drawn again over it.
    draw.text((-tb[0], -tb[1]), word, font=top, fill=look["text"])
    draw.text((-tb[0] + top.getlength(word[:at]), -tb[1]), word[at], font=top, fill=look["accent"])
    if word[at + 1:]:
        draw.text((-tb[0] + top.getlength(word[:at + 1]), -tb[1]), word[at + 1:], font=top, fill=look["text"])
    draw.text((-bb[0], (tb[3] - tb[1]) + gap - bb[1]), "EMBER NETPLAY", font=bottom, fill=look["light"])
    return out.crop(out.getbbox())


def lockup(look, font, mark, height):
    m = fit(mark, height)
    w = wordmark(look, font, round(height * 0.62))
    gap = round(height * 0.12)
    out = Image.new("RGBA", (m.width + gap + w.width, height), (0, 0, 0, 0))
    out.alpha_composite(m, (0, 0))
    out.alpha_composite(w, (m.width + gap, (height - w.height) // 2))
    return out


def tile_icon(look, mark, size):
    out = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    ImageDraw.Draw(out).rounded_rectangle((0, 0, size - 1, size - 1), radius=round(size * look["radius"]),
                                          fill=look["ground"] + (255,))
    inner = fit(mark, round(size * 0.84))
    out.alpha_composite(inner, ((size - inner.width) // 2, (size - inner.height) // 2))
    return out


def build(game, look, mark_src, background_src, font):
    with Image.open(mark_src) as m, Image.open(background_src) as b:
        mark = m.convert("RGBA") if game == "sfv" else recolour(m, look["map"])
        background = recolour(b, look["map"])
    mark = mark.crop(mark.getbbox())
    files = {}
    stem = f"{game}-ember"

    def png(name, image):
        buf = io.BytesIO()
        image.save(buf, "PNG", optimize=True)
        files[name] = buf.getvalue()

    icon = tile_icon(look, mark, ICON)
    png(f"{stem}-icon.png", icon)
    buf = io.BytesIO()
    icon.save(buf, "ICO", sizes=[(s, s) for s in ICO_SIZES])
    files[f"{stem}.ico"] = buf.getvalue()
    png(f"{stem}-mark.png", mark)
    png(f"{stem}-hero.png", cover(background, HERO, anchor_x=1.0).convert("RGB"))

    wide = cover(background, WIDE, anchor_x=1.0)
    lock = lockup(look, font, mark, round(WIDE[1] * 0.46))
    wide.alpha_composite(lock, (round(WIDE[0] * 0.06), (WIDE[1] - lock.height) // 2))
    png(f"{stem}-wide.png", wide.convert("RGB"))

    portrait = shade_bottom(cover(background, PORTRAIT, anchor_x=0.82), 0.92)
    m = fit(mark, round(PORTRAIT[0] * 0.62))
    portrait.alpha_composite(m, ((PORTRAIT[0] - m.width) // 2, round(PORTRAIT[1] * 0.12)))
    w = wordmark(look, font, round(PORTRAIT[1] * 0.17))
    if w.width > PORTRAIT[0] * 0.86:  # a longer name (UMvC3) still keeps its margins
        w = fit(w, round(w.height * PORTRAIT[0] * 0.86 / w.width))
    portrait.alpha_composite(w, ((PORTRAIT[0] - w.width) // 2, round(PORTRAIT[1] * 0.66)))
    png(f"{stem}-portrait.png", portrait.convert("RGB"))

    png(f"{stem}-logo.png", lockup(look, font, mark, LOGO_HEIGHT))
    return files


SIZES = {"icon": (ICON, ICON), "wide": WIDE, "portrait": PORTRAIT, "hero": HERO}


def check(game, files):
    problems = []
    for name, data in files.items():
        path = OUT / game / name
        if not path.exists():
            problems.append(f"{game}/{name} is missing")
            continue
        with Image.open(path) as have, Image.open(io.BytesIO(data)) as want:
            slot = name.rsplit("-", 1)[-1].split(".")[0]
            if slot in SIZES and have.size != SIZES[slot]:
                problems.append(f"{game}/{name} is {have.size}, Steam wants {SIZES[slot]}")
            if name.endswith(".ico"):
                continue
            if have.size != want.size:
                problems.append(f"{game}/{name} differs in size from the rebuild")
                continue
            diff = ImageStat.Stat(ImageChops.difference(have.convert("RGBA"), want.convert("RGBA"))).mean
            if max(diff) > CHECK_TOLERANCE:
                problems.append(f"{game}/{name} differs from the rebuild (mean {max(diff):.2f})")
    return problems


def main():
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--ember", required=True, type=Path, help="an SF4 Ember checkout (src/ui/ember.png, fonts, background)")
    p.add_argument("--sfv-mark", type=Path, help="SFV Ember's art/brand/sfv-ember-mark.png")
    p.add_argument("--only", default=",".join(LOOKS))
    p.add_argument("--check", action="store_true")
    a = p.parse_args()
    mark_src = a.ember / "src/ui/ember.png"
    background_src = a.ember / "assets/brand/ember-menu-background.png"
    font = a.ember / "src/ui/fonts/Inter-SemiBold.ttf"
    problems = []
    for game in a.only.split(","):
        src = a.sfv_mark if game == "sfv" else mark_src
        if src is None or not src.exists():
            sys.exit(f"{game}: mark source missing ({src}); sfv needs --sfv-mark")
        files = build(game, LOOKS[game], src, background_src, font)
        if a.check:
            problems += check(game, files)
            continue
        (OUT / game).mkdir(parents=True, exist_ok=True)
        for name, data in files.items():
            (OUT / game / name).write_bytes(data)
        print(f"{game}: wrote {len(files)} files")
    for line in problems:
        print("brand: " + line, file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
