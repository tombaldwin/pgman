"""Compose pgman's videos from the VHS scene recordings.

    python3 video/compose.py [web] [square] [loops]

`web` and `square` are the overview: title cards and captions rendered
with PIL, one clip per segment built with ffmpeg (the terminal with its
caption beside it, or a card), each faded in and out, concatenated.
16:9 for the site, 1:1 for LinkedIn. `loops` are the feature sections'
short silent loops: one scene each, the terminal alone. Reads
target/video/scenes/*.mp4 (video/render.sh records them) and writes
target/video/out/. Needs ffmpeg and Pillow; video/render.sh fetches
the Inter font into target/video/fonts.
"""
import os
import subprocess
import sys

from PIL import Image, ImageDraw, ImageFont

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
WORK = os.path.join(ROOT, "target", "video")
SCENES = os.path.join(WORK, "scenes")
OUT = os.path.join(WORK, "out")
FONT = {w: os.path.join(WORK, "fonts", f"inter-{w}.woff") for w in (400, 600, 800)}
# The install commands on the end card, in the terminal's font when it
# is installed (the tapes need it too), else Inter.
MONO = next((p for p in (
    os.path.expanduser("~/Library/Fonts/MesloLGSNerdFontMono-Regular.ttf"),
    "/usr/share/fonts/truetype/meslo/MesloLGSNerdFontMono-Regular.ttf",
) if os.path.exists(p)), FONT[400])

# Catppuccin Mocha, the terminal's theme.
CRUST = (17, 17, 27)
SURFACE = (49, 50, 68)
TEXT = (205, 214, 244)
SUBTEXT = (166, 173, 200)
BLUE = (137, 180, 250)
GREEN = (166, 227, 161)
PEACH = (250, 179, 135)

FPS = 30
FADE = 0.35

# Seconds cut from a recording's start: the start card before the key
# that opens the scene's panel.
SKIP = {"envs": 1.0, "sessions": 1.0, "slow": 1.0, "safety": 0.9, "patches": 1.0}

FORMATS = {
    # name: canvas, terminal box (x, y, w, h), caption box (x, y, w, h), caption size
    "web": dict(size=(1920, 1080), term=(120, 28, 1680, 880), cap=(60, 920, 1800, 150), cap_px=46),
    "square": dict(size=(1080, 1080), term=(20, 300, 1040, 545), cap=(50, 40, 980, 240), cap_px=52),
}

# (scene, [(start, end, caption)], hold seconds after the recording ends)
SEGMENTS = [
    ("card:intro", 3.5),
    ("logs", [(0.0, 3.6, "Point pgman at your Spring app's Hibernate log"),
              (3.6, 10.9, "It rebuilds the SQL and flags the N+1"),
              (10.9, 99, "Binds filled in. Run it, get rows.")], 1.0),
    ("envs", [(0, 99, "Every environment at a glance: replicas, lag, blocked sessions, pending migrations")], 0.8),
    ("sessions", [(0, 99, "Who is blocking whom, right now")], 0.8),
    ("slow", [(0, 99, "Your slowest queries, straight from pg_stat_statements")], 0.6),
    ("safety", [(0, 99, "Read-only by default. Guard rails on every write.")], 0.6),
    ("patches", [(0, 99, "Migrations dry-run in one transaction, then roll back")], 1.0),
    ("card:agents", 4.5),
    ("card:end", 4.5),
]

# The feature loops: scene, seconds held on its last frame (so a short
# scene still reads before it starts over).
LOOPS = {
    "logs": 1.5,
    "envs": 2.0,
    "sessions": 3.0,
    "slow": 2.0,
    "safety": 2.0,
    "patches": 2.0,
}


def font(weight, px):
    return ImageFont.truetype(FONT[weight], px)


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
    lo, hi = width // 3, width
    while len(lines) > 1 and lo < hi:
        mid = (lo + hi) // 2
        if len(wrap(draw, text, f, mid)) == len(lines):
            hi = mid
        else:
            lo = mid + 1
    return wrap(draw, text, f, hi) if len(lines) > 1 else lines


def caption_png(fmt, text, path):
    spec = FORMATS[fmt]
    img = Image.new("RGBA", spec["size"], (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    x, y, w, h = spec["cap"]
    f = font(600, spec["cap_px"])
    lines = balanced(d, text, f, w)
    lh = int(spec["cap_px"] * 1.25)
    top = y + (h - lh * len(lines)) // 2
    for i, line in enumerate(lines):
        lw = d.textlength(line, font=f)
        d.text((x + (w - lw) / 2, top + i * lh), line, font=f, fill=TEXT)
    img.save(path)


def centred(d, cx, y, text, f, fill):
    d.text((cx - d.textlength(text, font=f) / 2, y), text, font=f, fill=fill)


def card_png(fmt, kind, path):
    W, H = FORMATS[fmt]["size"]
    s = W / 1920 if fmt == "web" else W / 1080 * 0.82
    img = Image.new("RGB", (W, H), CRUST)
    d = ImageDraw.Draw(img)
    cx = W / 2
    mono = ImageFont.truetype(MONO, int(40 * s))
    if kind == "intro":
        centred(d, cx, H * 0.30, "pgman", font(800, int(150 * s)), BLUE)
        centred(d, cx, H * 0.30 + 200 * s, "A Postgres terminal for Java teams", font(600, int(60 * s)), TEXT)
        sub = "Spring logs to SQL  ·  read-only by default  ·  built for agents too"
        f = font(400, int(36 * s))
        for i, line in enumerate(wrap(d, sub, f, W * 0.86)):
            centred(d, cx, H * 0.30 + 300 * s + i * 48 * s, line, f, SUBTEXT)
    elif kind == "agents":
        centred(d, cx, H * 0.22, "And an MCP server for coding agents", font(800, int(64 * s)), TEXT)
        items = [
            ("read-only queries, EXPLAIN and schema diff", GREEN),
            ("masked dumps, so real data stays put", GREEN),
            ("writes only when you confirm them", PEACH),
        ]
        f = font(600, int(44 * s))
        y = H * 0.22 + 150 * s
        for text, colour in items:
            for line in wrap(d, "›  " + text, f, W * 0.86):
                centred(d, cx, y, line, f, colour)
                y += 64 * s
            y += 14 * s
    elif kind == "end":
        centred(d, cx, H * 0.24, "pgman", font(800, int(130 * s)), BLUE)
        y = H * 0.24 + 210 * s
        for cmd in ("brew install tombaldwin/tap/pgman", "cargo install pgman"):
            box_w = d.textlength(cmd, font=mono) + 60 * s
            d.rounded_rectangle((cx - box_w / 2, y - 12 * s, cx + box_w / 2, y + 62 * s),
                                radius=int(12 * s), fill=SURFACE)
            centred(d, cx, y, cmd, mono, TEXT)
            y += 100 * s
        centred(d, cx, y + 30 * s, "poly.io/products/pgman", font(600, int(44 * s)), TEXT)
        centred(d, cx, y + 100 * s, "Open source · MIT or Apache-2.0", font(400, int(32 * s)), SUBTEXT)
    img.save(path)


def run(args):
    r = subprocess.run(args, capture_output=True, text=True)
    if r.returncode:
        sys.exit(f"ffmpeg failed: {' '.join(args)}\n{r.stderr[-2000:]}")


def duration(path):
    out = subprocess.run(["ffprobe", "-v", "error", "-show_entries", "format=duration",
                          "-of", "csv=p=0", path], capture_output=True, text=True).stdout
    return float(out)


def fades(d):
    return f"fade=t=in:st=0:d={FADE},fade=t=out:st={d - FADE:.3f}:d={FADE}"


def scene_filter(name, src):
    """The filter prefix that plays scene `name` (input 1) with its
    SKIP applied, and how long it then runs."""
    length = duration(src)
    if name in SKIP:
        return (f"[1:v]trim=start={SKIP[name]},setpts=PTS-STARTPTS[src];[src]",
                length - SKIP[name])
    return "[1:v]", length


def build(fmt):
    spec = FORMATS[fmt]
    W, H = spec["size"]
    work = os.path.join(WORK, "build", fmt)
    os.makedirs(work, exist_ok=True)
    clips = []
    for i, seg in enumerate(SEGMENTS):
        out = os.path.join(work, f"{i:02d}.mp4")
        if seg[0].startswith("card:"):
            kind, secs = seg[0][5:], seg[1]
            png = os.path.join(work, f"{i:02d}-card.png")
            card_png(fmt, kind, png)
            run(["ffmpeg", "-y", "-v", "error", "-loop", "1", "-framerate", str(FPS), "-t", str(secs),
                 "-i", png, "-vf", f"format=yuv420p,{fades(secs)}", "-r", str(FPS),
                 "-c:v", "libx264", "-crf", "18", "-pix_fmt", "yuv420p", out])
        else:
            name, captions, hold = seg
            src = os.path.join(SCENES, f"{name}.mp4")
            pre, length = scene_filter(name, src)
            total = length + hold
            x, y, w, h = spec["term"]
            inputs = ["-f", "lavfi", "-i", f"color=c=0x{'%02x%02x%02x' % CRUST}:s={W}x{H}:r={FPS}:d={total:.3f}",
                      "-i", src]
            graph = [f"{pre}tpad=stop_mode=clone:stop_duration={hold},scale={w}:{h}:flags=lanczos[t]",
                     f"[0:v][t]overlay={x}:{y}:shortest=1[v0]"]
            last = "v0"
            for j, (a, b, text) in enumerate(captions):
                png = os.path.join(work, f"{i:02d}-cap{j}.png")
                caption_png(fmt, text, png)
                inputs += ["-loop", "1", "-framerate", str(FPS), "-i", png]
                b = min(b, total)
                graph.append(f"[{last}][{j + 2}:v]overlay=0:0:shortest=1:enable='between(t,{a},{b})'[v{j + 1}]")
                last = f"v{j + 1}"
            graph.append(f"[{last}]{fades(total)},format=yuv420p[out]")
            run(["ffmpeg", "-y", "-v", "error", *inputs, "-filter_complex", ";".join(graph),
                 "-map", "[out]", "-t", f"{total:.3f}", "-r", str(FPS),
                 "-c:v", "libx264", "-crf", "18", "-pix_fmt", "yuv420p", out])
        clips.append(out)
    listing = os.path.join(work, "clips.txt")
    with open(listing, "w") as f:
        f.writelines(f"file '{c}'\n" for c in clips)
    os.makedirs(OUT, exist_ok=True)
    final = os.path.join(OUT, f"pgman-overview-{fmt}.mp4")
    run(["ffmpeg", "-y", "-v", "error", "-f", "concat", "-safe", "0", "-i", listing,
         "-c:v", "libx264", "-crf", "20", "-preset", "slow", "-pix_fmt", "yuv420p",
         "-movflags", "+faststart", final])
    print(f"{final}  {duration(final):.1f}s")


def loops():
    """Each feature loop: the scene alone at its recorded size, held on
    its last frame, faded from and to the terminal's background so the
    jump back to the start is soft; and a poster from its last frame,
    for the page to show before the video loads."""
    os.makedirs(OUT, exist_ok=True)
    bg = "0x1e1e2e"  # Catppuccin Mocha base, the terminal's background
    for name, hold in LOOPS.items():
        src = os.path.join(SCENES, f"{name}.mp4")
        pre, length = scene_filter(name, src)
        total = length + hold
        out = os.path.join(OUT, f"pgman-{name}-loop.mp4")
        graph = (f"{pre}tpad=stop_mode=clone:stop_duration={hold},"
                 f"fade=t=in:st=0:d={FADE}:c={bg},fade=t=out:st={total - FADE:.3f}:d={FADE}:c={bg},"
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
