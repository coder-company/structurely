# Representative repository semantic acceptance — 2026-07-28

This gate exercises framework and project-resolution behavior on detached,
pinned copies of three public repositories. It is a semantic acceptance run,
not a comparison of raw edge counts.

| Repository | Pinned revision | Files | Fresh index | Semantic seam |
|---|---|---:|---:|---|
| expressjs/express | `a3714473` | 141 | 255.610 ms | repeated routes and middleware chains |
| HKUDS/LightRAG | `44db36fe` | 514 | 4,517.198 ms | FastAPI, React Router, TS aliases |
| getzep/graphiti | `526dcad7` | 255 | 637.366 ms | FastAPI |

All six assertions passed. In particular, the Express corpus originally
exposed a duplicate route semantic-key crash. The acceptance run verifies both
the corrected repeated-route identity and the `count` → `users` middleware
chain for `GET /middleware`.

Reproduce from existing local clones or mirrors:

```bash
python3 scripts/acceptance_repositories.py \
  --structurely target/release/structurely \
  --repository express=/path/to/express \
  --repository lightrag=/path/to/LightRAG \
  --repository graphiti=/path/to/graphiti \
  --output /tmp/structurely-real-repositories.json
```

The runner creates detached temporary clones and refuses revisions that do not
match the manifest. Checked-in raw measurements are in `results.json`.
