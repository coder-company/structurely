# Representative repository semantic acceptance — 2026-07-28

This gate exercises framework and project-resolution behavior on detached,
pinned copies of three public repositories. It is a semantic acceptance run,
not a comparison of raw edge counts.

| Repository | Pinned revision | Files | Fresh index | Semantic seam |
|---|---|---:|---:|---|
| expressjs/express | `a3714473` | 141 | 255.610 ms | repeated routes and middleware chains |
| HKUDS/LightRAG | `44db36fe` | 514 | 4,517.198 ms | FastAPI, React Router, TS aliases |
| getzep/graphiti | `526dcad7` | 255 | 637.366 ms | FastAPI |
| django/django | `92470ad3` | 2,972 | 48,560 ms | Django URLs and ambiguity pressure |

All eight assertions passed. In particular, the Express corpus originally
exposed a duplicate route semantic-key crash. The acceptance run verifies both
the corrected repeated-route identity and the `count` → `users` middleware
chain for `GET /middleware`. Django initially exposed 1.43 million speculative
relationships; bounded call fanout and scope-aware heritage resolution reduced
that to 262,090 while preserving the pinned route-to-view edge.

Reproduce from existing local clones or mirrors:

```bash
python3 scripts/acceptance_repositories.py \
  --structurely target/release/structurely \
  --repository express=/path/to/express \
  --repository lightrag=/path/to/LightRAG \
  --repository graphiti=/path/to/graphiti \
  --repository django=/path/to/django \
  --output /tmp/structurely-real-repositories.json
```

The runner creates detached temporary clones and refuses revisions that do not
match the manifest. Checked-in raw measurements are in `results.json`.
