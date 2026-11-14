# pgman's videos

The overview (title cards, captioned scenes) for the site's hero and
LinkedIn, and the short silent loops for the product page's feature
sections. Every frame is the real binary, recorded with
[VHS](https://github.com/charmbracelet/vhs): most scenes against
`pgman --demo`'s synthetic data; the `db-*` scenes (masked copies, the
copies journal, Spring discovery) against two scratch Postgres
containers that `render.sh` starts on 127.0.0.1:55441 ("production",
seeded with made-up people by `seed/seed.py`) and 55442 ("local"), with
a throwaway age key. Needs docker for those.

```bash
video/render.sh            # build, record every scene, compose
video/render.sh patches    # re-record one scene after a UI change
video/render.sh --compose  # re-compose after editing captions or timing
```

Output in `target/video/out/`:

| file | what |
|---|---|
| `pgman-overview-web.mp4` | 16:9, 1920×1080, ~90s |
| `pgman-overview-square.mp4` | 1:1, 1080×1080, the same cut for LinkedIn |
| `pgman-<scene>-loop.mp4` / `.jpg` | a feature loop and its poster |

- `scenes/*.tape`: one recording each; `_setup.tape` is the shared
  terminal and the off-screen launch.
- `compose.py`: the overview's segments and captions (`SEGMENTS`, each
  optionally a part of a recording), the loops (`LOOPS`), and the trims.
- `spring-app/`: a Spring Boot project's datasource config, for
  discovery; `billing-api.log`: the Hibernate log the logs scene reads.

`docker rm -f pgman-video-prod pgman-video-local` removes the containers.

Re-render before a release that changes a screen the videos show, and
look at the result before it goes anywhere: a timing change in pgman
can leave a caption describing the wrong screen.
