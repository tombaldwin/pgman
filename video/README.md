# pgman's videos

The overview (title cards, captioned scenes) for the site's hero and
LinkedIn, and the short silent loops for the product page's feature
sections. Every frame is the real binary running `pgman --demo`'s
synthetic data, recorded with [VHS](https://github.com/charmbracelet/vhs).

```bash
video/render.sh            # build, record every scene, compose
video/render.sh patches    # re-record one scene after a UI change
video/render.sh --compose  # re-compose after editing captions or timing
```

Output in `target/video/out/`:

| file | what |
|---|---|
| `pgman-overview-web.mp4` | 16:9, 1920×1080, ~56s |
| `pgman-overview-square.mp4` | 1:1, 1080×1080, the same cut for LinkedIn |
| `pgman-<scene>-loop.mp4` / `.jpg` | a feature loop and its poster |

- `scenes/*.tape`: one recording each; `_setup.tape` is the shared
  terminal and the off-screen launch.
- `compose.py`: the overview's segments and captions (`SEGMENTS`), the
  loops (`LOOPS`), and the speed-ups and trims applied to recordings.

Re-render before a release that changes a screen the videos show, and
look at the result before it goes anywhere: a timing change in pgman
can leave a caption describing the wrong screen.
