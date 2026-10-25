# Subspace console

Web console for `subspace-server` (React + Vite + TypeScript).

```bash
npm install
npm run dev        # http://localhost:5173, proxies /api to http://127.0.0.1:7700
```

Run the server alongside it (`cargo run -p subspace-server` from the repo root). Point the
proxy elsewhere with `SUBSPACE_API=http://127.0.0.1:8080 npm run dev`.

## Scripts

| Script | What it does |
|--------|--------------|
| `npm run dev` | Dev server with the API proxy |
| `npm run build` | Type-check and build to `dist/` |
| `npm run typecheck` | Type-check only |
| `npm run lint` | Lint with oxlint |
| `npm run gen:types` | Regenerate `src/api/generated/` from the Rust wire types (ts-rs) |

`src/api/generated/` is generated; don't edit it by hand. A Rust test fails when it is stale.
