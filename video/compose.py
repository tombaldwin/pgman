"""Compose pgman's videos from the VHS scene recordings.

    python3 video/compose.py [web] [square] [loops]

`web` and `square` are the overview, in poly.io's look: the site's navy
page with its colour stripe, the terminal in the site's demo window,
captions in Space Grotesk, a brand footer, cards between, crossfades.
16:9 for the site, 1:1 for LinkedIn. `loops` are the feature sections'
short silent loops: one scene each, the terminal alone (the site puts
its own window around them). Reads target/video/scenes/*.mp4
(video/render.sh records them) and target/video/brand/ (render.sh
fetches the site's fonts and the pgman sprite from www.poly.io), and
writes target/video/out/. Needs ffmpeg and Pillow.
"""
import os
import re
import subprocess
import sys

from PIL import Image, ImageDraw, ImageFilter, ImageFont

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
WORK = os.path.join(ROOT, "target", "video")
SCENES = os.path.join(WORK, "scenes")
OUT = os.path.join(WORK, "out")
BRAND = os.path.join(WORK, "brand")
SANS = {w: os.path.join(BRAND, f"space-grotesk-{w}.woff2") for w in (300, 400, 600, 700)}
DISPLAY = os.path.join(BRAND, "neuropolitical.woff")
SPRITE = os.path.join(BRAND, "pgman-elephant-pixel.svg")
# The install commands, in the terminal's font when it is installed
# (the tapes need it too), else Space Grotesk.
MONO = next((p for p in (
    os.path.expanduser("~/Library/Fonts/MesloLGSNerdFontMono-Regular.ttf"),
    "/usr/share/fonts/truetype/meslo/MesloLGSNerdFontMono-Regular.ttf",
) if os.path.exists(p)), SANS[400])

# poly.io's colours (the site's dark theme and its stripe).
PAGE = (35, 42, 68)          # --bg, dark
PAGE_LOW = (28, 34, 56)      # the same, darker, for the page's gradient
TEXT = (238, 240, 247)
MUTED = (154, 163, 189)      # --muted, dark
CODE_BG = (27, 33, 51)       # .install, dark
CODE_BORDER = (51, 59, 84)
STRIPE = [(47, 95, 179), (14, 94, 111), (61, 214, 163), (255, 210, 94), (255, 90, 54)]
NAVY, TEAL, MINT, BUTTER, ORANGE = STRIPE
# The site's demo window (.demo-frame, .demo-bar).
WINDOW_BAR = (22, 27, 34)
WINDOW_BORDER = (58, 64, 84)
DOTS = [(255, 95, 86), (255, 189, 46), (39, 201, 63)]
TERM_BG = (30, 30, 46)       # Catppuccin Mocha's base, the recordings' background

FPS = 30
XFADE = 0.5     # the crossfade between segments
CAP_FADE = 0.25

FORMATS = {
    # canvas; the terminal window's box (x, y, width): its height follows
    # the recording's shape; the caption's box (x, y, w, h) and size; the
    # footer's baseline and size.
    "web": dict(size=(1920, 1080), win=(180, 46, 1560), cap=(120, 930, 1680, 80), cap_px=42,
                foot_y=1030, foot_px=24, stripe=8),
    "square": dict(size=(1080, 1080), win=(28, 290, 1024), cap=(60, 54, 960, 200), cap_px=50,
                   foot_y=988, foot_px=34, stripe=10),
}
TERM_W, TERM_H = 1680, 880   # the recordings' size (video/scenes/_terminal.tape)

# Seconds cut from a recording's start: the start card before the key
# that opens the scene's panel.
SKIP = {"envs": 1.0, "sessions": 1.0, "slow": 1.0, "safety": 0.9, "patches": 1.0}

# (scene, [(start, end, caption)], hold seconds after the recording
# ends, and optionally (start, end): the part of the recording to use,
# for a recording cut into several segments). Caption times are within
# the part used.
SEGMENTS = [
    ("card:intro", 3.5),
    ("envs", [(0, 4.0, "Every environment on one screen: replicas, lag, sessions, pending migrations"),
              (4.0, 99, "Flyway, Liquibase, Alembic, or your own version table: who is behind")], 0.6),
    ("db-masking", [(0, 99, "What a masked copy does to every column, and why")], 0.3, (0, 5.0)),
    ("db-masking", [(0, 99, "Production asks why first, then copies it masked and encrypted")], 0.3, (5.0, 13.2)),
    ("db-masking", [(0, 99, "Real customers in production. Nobody real in the local copy.")], 0.3, (19.9, 27.3)),
    ("db-masking", [(0, 99, "Every local copy on record: why it was taken, where it went, until it is deleted")], 0.8, (27.6, 99)),
    ("patches", [(0, 99, "Migrations dry-run in one transaction, then roll back")], 0.6, (3.3, 99)),
    ("safety", [(0, 99, "Read-only by default. Guard rails on every write.")], 0.4),
    ("db-spring", [(0, 99, "In a Spring project it finds your datasources; nothing connects until you say")], 0.4, (0, 6.3)),
    ("logs", [(0.0, 3.3, "Point it at a Hibernate log"),
              (3.3, 10.6, "It rebuilds the SQL and flags the N+1"),
              (10.6, 99, "Binds filled in. Run it.")], 0.3, (0, 13.5)),
    ("db-tap", [(0, 99, "Your app's live queries, from the OpenTelemetry Java agent")], 0.6, (0.3, 99)),
    ("card:agents", 4.5),
    ("card:end", 5.0),
]

# The feature loops: name, (scene, part of it or None, seconds held on
# its last frame, so a short scene still reads before it starts over).
LOOPS = {
    "envs": ("envs", None, 2.0),
    "masking": ("db-masking", (0, 27.3), 1.5),
    "copies": ("db-masking", (27.6, 99), 2.0),
    "patches": ("patches", None, 2.0),
    "safety": ("safety", None, 2.0),
    "sessions": ("sessions", None, 3.0),
    "slow": ("slow", None, 2.0),
    "spring": ("db-spring", None, 1.5),
    "logs": ("logs", None, 1.5),
    "tap": ("db-tap", (0.3, 99), 1.5),
}


def sans(weight, px):
    return ImageFont.truetype(SANS[weight], int(px))


def wrap(draw, text, f, width):
    words, lines, line = text.split(), [], ""
    for w in words:
        trial = f"{line} {w}".strip()
        if draw.textlength(trial, font=f) <= width or not line:
            line = trial
        else:
            lines.append(line)
            line = w
    lines.append(line)
    return lines


def balanced(draw, text, f, width):
    """`wrap`, with the lines as even as the same line count allows."""
    lines = wrap(draw, text, f, width)
    lo, hi = int(width // 3), int(width)
    while len(lines) > 1 and lo < hi:
        mid = (lo + hi) // 2
        if len(wrap(draw, text, f, mid)) == len(lines):
            hi = mid
        else:
            lo = mid + 1
    return wrap(draw, text, f, hi) if len(lines) > 1 else lines


def centred(d, cx, y, text, f, fill):
    d.text((cx - d.textlength(text, font=f) / 2, y), text, font=f, fill=fill)


def page(size, stripe):
    """The site's page: navy, a little darker towards the bottom, the
    five-colour stripe along the top."""
    W, H = size
    img = Image.new("RGB", size, PAGE)
    d = ImageDraw.Draw(img)
    for y in range(H):
        t = y / (H - 1)
        d.line([(0, y), (W, y)], fill=tuple(round(a + (b - a) * t) for a, b in zip(PAGE, PAGE_LOW)))
    for i, colour in enumerate(STRIPE):
        d.rectangle((W * i // 5, 0, W * (i + 1) // 5, stripe - 1), fill=colour)
    return img


def sprite(px):
    """pgman's pixel elephant on its butter tile, `px` square, drawn from
    the site's SVG (a grid of rects) with hard pixel edges."""
    svg = open(SPRITE).read()
    vb = [float(v) for v in re.search(r'viewBox="([^"]+)"', svg).group(1).replace(",", " ").split()]
    art = Image.new("RGBA", (int(vb[2]), int(vb[3])), (0, 0, 0, 0))
    d = ImageDraw.Draw(art)
    for x, y, w, h, fill in re.findall(
            r'<rect x="([\d.]+)" y="([\d.]+)" width="([\d.]+)" height="([\d.]+)" fill="(#[0-9a-fA-F]+)"', svg):
        x, y, w, h = (float(v) for v in (x, y, w, h))
        d.rectangle((x, y, x + w - 1, y + h - 1), fill=fill)
    tile = Image.new("RGBA", (px, px), (0, 0, 0, 0))
    ImageDraw.Draw(tile).rounded_rectangle((0, 0, px - 1, px - 1), radius=round(px * 0.22), fill=BUTTER)
    inner = round(px * 0.66)
    scaled = art.resize((inner, round(inner * art.height / art.width)), Image.NEAREST)
    tile.alpha_composite(scaled, ((px - scaled.width) // 2, (px - scaled.height) // 2))
    return tile


def window_box(fmt):
    x, y, w = FORMATS[fmt]["win"]
    bar = round(w * 0.026)
    body_h = round(w * TERM_H / TERM_W)
    return x, y, w, bar, body_h


def chrome(fmt):
    """The frame every scene sits in, as two layers: under the terminal
    (the page, the window's shadow, border and title bar, the footer) and
    over it (the window's bottom corners, rounded off)."""
    spec = FORMATS[fmt]
    W, H = spec["size"]
    img = page(spec["size"], spec["stripe"]).convert("RGBA")
    x, y, w, bar, body_h = window_box(fmt)
    r = round(w * 0.0077)
    shadow = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    ImageDraw.Draw(shadow).rounded_rectangle((x, y + 18, x + w, y + bar + body_h + 18), radius=r, fill=(8, 10, 24, 150))
    img.alpha_composite(shadow.filter(ImageFilter.GaussianBlur(26)))
    d = ImageDraw.Draw(img)
    d.rounded_rectangle((x - 1, y - 1, x + w, y + bar + body_h), radius=r + 1, fill=WINDOW_BORDER)
    d.rounded_rectangle((x, y, x + w - 1, y + bar + body_h - 1), radius=r, fill=TERM_BG)
    d.rounded_rectangle((x, y, x + w - 1, y + bar + r), radius=r, fill=WINDOW_BAR)
    d.rectangle((x, y + bar, x + w - 1, y + bar + r + 1), fill=TERM_BG)
    dot = max(6, round(bar * 0.3))
    for i, colour in enumerate(DOTS):
        cx = x + round(bar * 0.55) + i * round(dot * 1.7)
        d.ellipse((cx, y + bar / 2 - dot / 2, cx + dot, y + bar / 2 + dot / 2), fill=colour)
    footer(img, fmt)
    # Over the terminal: everything transparent but the bottom corners
    # outside the window's rounding, which take the page's pixels.
    over = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    mask = Image.new("L", (W, H), 0)
    md = ImageDraw.Draw(mask)
    top, bottom = y + bar + body_h - r - 1, y + bar + body_h
    md.rectangle((x - 1, top, x + w, bottom), fill=255)
    md.rounded_rectangle((x - 1, y - 1, x + w, bottom), radius=r + 1, fill=0)
    over.paste(img, (0, 0), mask)
    d2 = ImageDraw.Draw(over)
    d2.rounded_rectangle((x - 1, y - 1, x + w, bottom), radius=r + 1, outline=WINDOW_BORDER, width=1)
    clear = Image.new("L", (W, H), 255)
    ImageDraw.Draw(clear).rectangle((x, y, x + w - 1, top - 1), fill=0)
    over.putalpha(Image.composite(over.getchannel("A"), Image.new("L", (W, H), 0), clear))
    return img, over


def footer(img, fmt):
    """pgman's tile and name on the left, the polymorphism wordmark on
    the right."""
    spec = FORMATS[fmt]
    W, _ = spec["size"]
    x = FORMATS[fmt]["win"][0]
    px, base = spec["foot_px"], spec["foot_y"]
    d = ImageDraw.Draw(img)
    tile = sprite(round(px * 1.35))
    img.alpha_composite(tile, (x, base - tile.height + round(px * 0.25)))
    d.text((x + tile.width + round(px * 0.45), base), "pgman", font=sans(700, px), fill=TEXT, anchor="ls")
    mark = ImageFont.truetype(DISPLAY, round(px * 1.05))
    d.text((W - x, base), "polymorphism", font=mark, fill=MUTED, anchor="rs")


def caption_png(fmt, text, path):
    spec = FORMATS[fmt]
    img = Image.new("RGBA", spec["size"], (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    x, y, w, h = spec["cap"]
    f = sans(600, spec["cap_px"])
    lines = balanced(d, text, f, w)
    lh = round(spec["cap_px"] * 1.22)
    top = y + (h - lh * len(lines)) / 2
    for i, line in enumerate(lines):
        centred(d, x + w / 2, top + i * lh, line, f, TEXT)
    img.save(path)


def install_box(img, cx, y, cmd, f, s):
    """A command in the site's .install box: dark, bordered, a muted
    prompt."""
    d = ImageDraw.Draw(img)
    prompt = "$ "
    tw = d.textlength(prompt + cmd, font=f)
    pad_x, pad_y = 26 * s, 14 * s
    box = (cx - tw / 2 - pad_x, y, cx + tw / 2 + pad_x, y + f.size + 2 * pad_y)
    d.rounded_rectangle(box, radius=round(10 * s), fill=CODE_BG, outline=CODE_BORDER, width=max(1, round(1.5 * s)))
    d.text((box[0] + pad_x, y + pad_y), prompt, font=f, fill=MUTED)
    d.text((box[0] + pad_x + d.textlength(prompt, font=f), y + pad_y), cmd, font=f, fill=TEXT)
    return box[3]


def card_png(fmt, kind, path):
    spec = FORMATS[fmt]
    W, H = spec["size"]
    s = W / 1920 if fmt == "web" else 0.86
    img = page(spec["size"], spec["stripe"]).convert("RGBA")
    d = ImageDraw.Draw(img)
    cx = W / 2
    if kind in ("intro", "end"):
        tile = sprite(round(150 * s))
        name = sans(700, 150 * s)
        gap = 36 * s
        total = tile.width + gap + d.textlength("pgman", font=name)
        top = H * (0.26 if kind == "intro" else 0.2)
        img.alpha_composite(tile, (round(cx - total / 2), round(top)))
        d.text((cx - total / 2 + tile.width + gap, top + tile.height / 2), "pgman", font=name, fill=TEXT, anchor="lm")
        y = top + tile.height + 60 * s
        if kind == "intro":
            f = sans(600, 60 * s)
            for line in balanced(d, "Every Postgres database you run, on one screen", f, W * 0.84):
                centred(d, cx, y, line, f, TEXT)
                y += 76 * s
            y += 26 * s
            f = sans(400, 36 * s)
            sub = "Masked copies  ·  an audit trail  ·  safe migrations  ·  made for Java teams"
            for line in balanced(d, sub, f, W * 0.84):
                centred(d, cx, y, line, f, BUTTER)
                y += 50 * s
        else:
            mono = ImageFont.truetype(MONO, round(36 * s))
            for cmd in ("brew trust tombaldwin/tap", "brew install tombaldwin/tap/pgman",
                        "cargo install pgman --locked"):
                y = install_box(img, cx, y, cmd, mono, s) + 22 * s
            y += 34 * s
            centred(d, cx, y, "poly.io/pgman", sans(700, 48 * s), BUTTER)
            y += 78 * s
            centred(d, cx, y, "Open source  ·  MIT or Apache-2.0", sans(400, 32 * s), MUTED)
            mark = ImageFont.truetype(DISPLAY, round(54 * s))
            d.text((cx, H - 80 * s), "polymorphism", font=mark, fill=TEXT, anchor="ms")
    elif kind == "agents":
        f = sans(700, 70 * s)
        lines = balanced(d, "All of it drivable by your coding agent", f, W * 0.84)
        y = H * 0.27
        for line in lines:
            centred(d, cx, y, line, f, TEXT)
            y += 86 * s
        centred(d, cx, y + 6 * s, "over MCP, with the same guards in place", sans(600, 44 * s), BUTTER)
        y += 120 * s
        items = [
            (MINT, "Read-only by default, the same guard rails"),
            (MINT, "Masked dumps, journaled like yours"),
            (ORANGE, "Every write waits for your confirmation"),
        ]
        f = sans(400, 40 * s)
        widest = max(d.textlength(t, font=f) for _, t in items)
        left = cx - (widest + 40 * s) / 2
        for colour, text in items:
            for line in balanced(d, text, f, W * 0.8):
                d.rounded_rectangle((left, y + 12 * s, left + 16 * s, y + 28 * s), radius=round(4 * s), fill=colour)
                d.text((left + 40 * s, y), line, font=f, fill=TEXT)
                y += 58 * s
            y += 10 * s
    img.convert("RGB").save(path)


def run(args):
    r = subprocess.run(args, capture_output=True, text=True)
    if r.returncode:
        sys.exit(f"ffmpeg failed: {' '.join(args)}\n{r.stderr[-2000:]}")


def duration(path):
    out = subprocess.run(["ffprobe", "-v", "error", "-show_entries", "format=duration",
                          "-of", "csv=p=0", path], capture_output=True, text=True).stdout
    return float(out)


def scene_filter(name, src, part=None):
    """The filter prefix that plays scene `name` (input 1), from `part`
    when given, else with its SKIP applied, and how long it then runs."""
    length = duration(src)
    a, b = part if part else (SKIP.get(name, 0), length)
    b = min(b, length)
    if a or b < length:
        return f"[1:v]trim=start={a}:end={b},setpts=PTS-STARTPTS[src];[src]", b - a
    return "[1:v]", length


def build(fmt):
    spec = FORMATS[fmt]
    W, H = spec["size"]
    work = os.path.join(WORK, "build", fmt)
    os.makedirs(work, exist_ok=True)
    under, over = chrome(fmt)
    under_png, over_png = os.path.join(work, "chrome-under.png"), os.path.join(work, "chrome-over.png")
    under.convert("RGB").save(under_png)
    over.save(over_png)
    x, y, w, bar, body_h = window_box(fmt)
    clips = []
    for i, seg in enumerate(SEGMENTS):
        out = os.path.join(work, f"{i:02d}.mp4")
        if seg[0].startswith("card:"):
            kind, secs = seg[0][5:], seg[1]
            png = os.path.join(work, f"{i:02d}-card.png")
            card_png(fmt, kind, png)
            run(["ffmpeg", "-y", "-v", "error", "-loop", "1", "-framerate", str(FPS), "-t", str(secs),
                 "-i", png, "-vf", "format=yuv420p", "-r", str(FPS),
                 "-c:v", "libx264", "-crf", "16", "-pix_fmt", "yuv420p", out])
            clips.append((out, secs))
            continue
        name, captions, hold, *part = seg
        src = os.path.join(SCENES, f"{name}.mp4")
        pre, length = scene_filter(name, src, part[0] if part else None)
        total = length + hold
        inputs = ["-loop", "1", "-framerate", str(FPS), "-t", f"{total:.3f}", "-i", under_png, "-i", src,
                  "-loop", "1", "-framerate", str(FPS), "-t", f"{total:.3f}", "-i", over_png]
        graph = [f"{pre}tpad=stop_mode=clone:stop_duration={hold},scale={w}:{body_h}:flags=lanczos[t]",
                 f"[0:v][t]overlay={x}:{y + bar}:shortest=1[v0]",
                 "[v0][2:v]overlay=0:0[v1]"]
        last = "v1"
        for j, (a, b, text) in enumerate(captions):
            png = os.path.join(work, f"{i:02d}-cap{j}.png")
            caption_png(fmt, text, png)
            b = min(b, total)
            inputs += ["-loop", "1", "-framerate", str(FPS), "-t", f"{total:.3f}", "-i", png]
            k = j + 3
            # Fade each caption in and out where it changes; not at a
            # segment's edges, which the crossfade already covers.
            fades = []
            if a > 0:
                fades.append(f"fade=t=in:st={a}:d={CAP_FADE}:alpha=1")
            if b < total:
                fades.append(f"fade=t=out:st={b - CAP_FADE:.3f}:d={CAP_FADE}:alpha=1")
            graph.append(f"[{k}:v]format=rgba{''.join(',' + f for f in fades)}[c{j}]")
            graph.append(f"[{last}][c{j}]overlay=0:0:enable='between(t,{a},{b})'[v{j + 2}]")
            last = f"v{j + 2}"
        graph.append(f"[{last}]format=yuv420p[out]")
        run(["ffmpeg", "-y", "-v", "error", *inputs, "-filter_complex", ";".join(graph),
             "-map", "[out]", "-t", f"{total:.3f}", "-r", str(FPS),
             "-c:v", "libx264", "-crf", "16", "-pix_fmt", "yuv420p", out])
        clips.append((out, total))
    # Crossfade each segment into the next; fade in from and out to the page.
    inputs, graph, last, offset = [], [], "0:v", 0.0
    for j, (path, secs) in enumerate(clips):
        inputs += ["-i", path]
        if j:
            offset += clips[j - 1][1] - XFADE
            graph.append(f"[{last}][{j}:v]xfade=transition=fade:duration={XFADE}:offset={offset:.3f}[x{j}]")
            last = f"x{j}"
    total = offset + clips[-1][1]
    page_hex = "0x%02x%02x%02x" % PAGE
    graph.append(f"[{last}]fade=t=in:st=0:d=0.4:c={page_hex},"
                 f"fade=t=out:st={total - 0.6:.3f}:d=0.6:c={page_hex},format=yuv420p[out]")
    os.makedirs(OUT, exist_ok=True)
    final = os.path.join(OUT, f"pgman-overview-{fmt}.mp4")
    run(["ffmpeg", "-y", "-v", "error", *inputs, "-filter_complex", ";".join(graph), "-map", "[out]",
         "-r", str(FPS), "-c:v", "libx264", "-crf", "19", "-preset", "slow", "-pix_fmt", "yuv420p",
         "-movflags", "+faststart", final])
    # A poster for the page to show before the video plays: the title card.
    run(["ffmpeg", "-y", "-v", "error", "-ss", "2", "-i", final, "-frames:v", "1", "-q:v", "3",
         final.replace(".mp4", ".jpg")])
    print(f"{final}  {duration(final):.1f}s")


def loops():
    """Each feature loop: the scene alone at its recorded size, held on
    its last frame, faded from and to the terminal's background so the
    jump back to the start is soft; and a poster from its last frame,
    for the page to show before the video loads."""
    os.makedirs(OUT, exist_ok=True)
    bg = "0x%02x%02x%02x" % TERM_BG
    fade = 0.35
    for name, (scene, part, hold) in LOOPS.items():
        src = os.path.join(SCENES, f"{scene}.mp4")
        pre, length = scene_filter(scene, src, part)
        total = length + hold
        out = os.path.join(OUT, f"pgman-{name}-loop.mp4")
        graph = (f"{pre}tpad=stop_mode=clone:stop_duration={hold},"
                 f"fade=t=in:st=0:d={fade}:c={bg},fade=t=out:st={total - fade:.3f}:d={fade}:c={bg},"
                 f"format=yuv420p[out]")
        run(["ffmpeg", "-y", "-v", "error", "-f", "lavfi", "-i", "nullsrc", "-i", src,
             "-filter_complex", graph, "-map", "[out]", "-t", f"{total:.3f}", "-r", str(FPS),
             "-an", "-c:v", "libx264", "-crf", "22", "-preset", "slow", "-pix_fmt", "yuv420p",
             "-movflags", "+faststart", out])
        run(["ffmpeg", "-y", "-v", "error", "-sseof", f"-{hold / 2:.2f}", "-i", out,
             "-frames:v", "1", "-q:v", "3", os.path.join(OUT, f"pgman-{name}-loop.jpg")])
        print(f"{out}  {duration(out):.1f}s")


if __name__ == "__main__":
    for what in sys.argv[1:] or [*FORMATS, "loops"]:
        loops() if what == "loops" else build(what)
