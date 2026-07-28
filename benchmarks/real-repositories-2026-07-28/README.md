# Representative repository semantic acceptance — 2026-07-28

This gate exercises framework and project-resolution behavior on detached,
pinned copies of seven public repositories. It is a semantic acceptance run,
not a comparison of raw edge counts.

| Repository | Pinned revision | Files | Fresh index | Semantic seam |
|---|---|---:|---:|---|
| expressjs/express | `a3714473` | 141 | 255.610 ms | repeated routes and middleware chains |
| HKUDS/LightRAG | `44db36fe` | 514 | 4,517.198 ms | FastAPI, React Router, TS aliases |
| getzep/graphiti | `526dcad7` | 255 | 637.366 ms | FastAPI |
| django/django | `92470ad3` | 2,972 | 48,759.187 ms | Django URLs and ambiguity pressure |
| nestjs/nest | `fafe503b` | 1,727 | 4,904.063 ms | exported controllers and decorator routes |
| vuejs/core | `b5f85183` | 535 | 5,546.435 ms | SFC scripts and template event flow |
| sveltejs/svelte | `44a78137` | 7,927 | 60,888.237 ms | embedded scripts and template event flow under duplicate-fixture pressure |

All fourteen assertions passed. In particular, the Express corpus originally
exposed a duplicate route semantic-key crash. The acceptance run verifies both
the corrected repeated-route identity and the `count` → `users` middleware
chain for `GET /middleware`. Django initially exposed 1.43 million speculative
relationships; bounded call fanout and scope-aware heritage resolution reduced
that to 262,090 while preserving the pinned route-to-view edge. NestJS exposed
decorators attached to exported class wrappers; the gate verifies that
`GET /users/:id` resolves precisely to `UsersController.findOne`. Vue and
Svelte verify exact source positions and template-to-handler edges. The Svelte
corpus also exposed unbounded external-import fanout; canonical relative imports
and an eight-target structural cap reduced a 500-file diagnostic from 83,660
false import edges to 29 valid relative-import edges.

Reproduce from existing local clones or mirrors:

```bash
python3 scripts/acceptance_repositories.py \
  --structurely target/release/structurely \
  --repository express=/path/to/express \
  --repository lightrag=/path/to/LightRAG \
  --repository graphiti=/path/to/graphiti \
  --repository django=/path/to/django \
  --repository nest=/path/to/nest \
  --repository vue=/path/to/vue \
  --repository svelte=/path/to/svelte \
  --output /tmp/structurely-real-repositories.json
```

The runner creates detached temporary clones and refuses revisions that do not
match the manifest. Checked-in raw measurements are in `results.json`.
