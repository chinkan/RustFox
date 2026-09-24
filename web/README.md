# RustFox Web Portal — Prototype

A runnable **TanStack Router + TanStack Query** skeleton for the RustFox Web
Portal described in `obsidian-vault/ideas/incubating/rustfox-web-portal.md`.

The point of this prototype is to prove out the **frontend architecture** —
routing, URL state, auth guards, nested layouts, data layer — against the
existing RustFox **Axum** backend. It ships with a mock API so it runs with
zero backend setup.

---

## Quick start

```bash
npm install
npm run dev          # http://localhost:5173
```

Open the app → you land on `/login`. Sign in as:

| Username | Role | What you can see |
|---|---|---|
| `kan` | `admin` | Everything, including `/settings` |
| `guest` | `user` | Everything **except** `/settings` (RBAC guard redirects) |

### Scripts

| Command | What it does |
|---|---|
| `npm run dev` | Vite dev server + HMR, proxies `/api` → `http://127.0.0.1:8080` |
| `npm run build` | Production build to `dist/` |
| `npm run preview` | Serve the built `dist/` locally |
| `npm run typecheck` | `tsc --noEmit` — the real type-safety gate |
| `npm test` | Vitest integration tests against the real router |

---

## What this prototype demonstrates

### 1. File-based, fully type-safe routing

Routes live in `src/routes/`. The TanStack Router Vite plugin generates
`src/routeTree.gen.ts` on the fly — never edit it.

```
src/routes/
├── __root.tsx              # root + notFoundComponent
├── index.tsx               # / → redirect to /dashboard or /login
├── login.tsx               # /login  (validateSearch: redirect)
└── _auth.tsx               # pathless layout: sidebar shell + auth guard
    ├── dashboard.tsx       # /dashboard
    ├── chat.tsx            # /chat  (layout — thread sidebar)
    ├── chat.index.tsx      # /chat/
    ├── chat.$threadId.tsx  # /chat/$threadId  (loader + notFound)
    ├── agents.tsx          # /agents  (type-safe search params)
    ├── memory.tsx          # /memory  (type-safe search params)
    ├── tasks.tsx           # /tasks
    └── settings.tsx        # /settings (admin-only)
```

**Prove it's real type-safety** — break a link on purpose and run
`npm run typecheck`:

```tsx
<Link to="/dashbord">typo</Link>       // TS2820: not assignable… Did you mean "/dashboard"?
<Link to="/chat/$threadId">x</Link>    // TS2741: Property 'params' is missing
```

Both are **compile errors**, not runtime surprises. That is the core reason
this stack was chosen over SvelteKit / Next.js.

### 2. Route-level auth + RBAC guards

`beforeLoad` runs *before* any component renders:

- `_auth.tsx` → unauthenticated users are redirected to `/login?redirect=<current-url>`
- `settings.tsx` → non-admins are redirected to `/dashboard`

No per-component `if (!user) return <Redirect/>` scattered around.

### 3. Type-safe search params (the killer feature)

`agents.tsx` and `memory.tsx` define a zod schema with `validateSearch`:

```ts
const agentSearchSchema = z.object({
  q: z.string().default('').catch(''),
  status: z.enum(['all', 'idle', 'running', 'error']).default('all').catch('all'),
  sort: z.enum(['name', 'status']).default('name').catch('name'),
})
```

Every filter lives in the URL → shareable, bookmarkable, back-button friendly.
`.catch()` means a hand-edited garbage URL degrades to defaults instead of
crashing. Try `/agents?status=bogus` — it just works.

### 4. Nested layout persistence

`/chat` renders the thread sidebar; `/chat/$threadId` renders only the
conversation pane. Navigating between threads **never re-mounts the sidebar**.

### 5. TanStack Query as the single data layer

All data access goes through `useQuery` keyed by route + params
(`['memory', q, kind]`), with a shared `QueryClient`. Switching routes within
`staleTime` (30s) serves from cache.

---

## Going live against the RustFox Axum backend

Everything in `src/api/client.ts` mirrors the intended REST shape. To switch
from mock to real:

```ts
// src/api/client.ts
export const api = {
  listWorkspaces: () => fetch('/api/workspaces').then(r => r.json()),
  getWorkspace:   (id) => fetch(`/api/workspaces/${id}`).then(r => r.json()),
  // …
}
```

The signatures don't change, so **no component or route code has to change**.

In production, `vite build` emits `dist/` and Axum serves it with `ServeDir`
alongside `/api/*` — so the runtime stays a **single Rust binary**, no Node.js:

```rust
let app = Router::new()
    .nest("/api", api_routes())
    .fallback_service(ServeDir::new("web-portal-prototype/dist")
        .fallback(ServeFile::new("web-portal-prototype/dist/index.html")));
```

---

## Project layout

```
rustfox-portal/
├── index.html
├── vite.config.ts            # tanstackRouter() MUST precede react()
├── tsconfig.json
├── src/
│   ├── main.tsx              # React entry
│   ├── App.tsx               # providers
│   ├── router.tsx            # router + queryClient singletons (shared w/ tests)
│   ├── auth.ts               # auth store + RouterContext types
│   ├── styles.css
│   ├── api/
│   │   ├── client.ts         # mock API (swap for fetch)
│   │   └── types.ts          # shared API types
│   ├── routeTree.gen.ts      # GENERATED — do not edit
│   ├── router.test.tsx       # 9 integration tests
│   └── routes/               # file-based routes
```

## Test coverage

`npm test` mounts the **real router** with a memory history and asserts:

- auth guard redirects unauthenticated `/dashboard` → `/login`
- authenticated users reach `/dashboard`
- RBAC blocks `user` from `/settings`, allows `admin`
- search params parse, update on input, and fall back on garbage input
- nested chat layout renders sidebar + thread together
- `notFound()` from a loader renders the 404 component

## Known prototype limitations

- **Auth is fake** — `localStorage` session, no JWT. Wire to Axum auth later.
- **Chat send is disabled** — no SSE/WebSocket streaming yet.
- **Mock data only** — `src/api/client.ts` returns fixtures after a 220ms delay.
- **No virtualised lists / Monaco editor** — deferred until the architecture is
  validated.

---

## Screenshots

Captured by `npm run smoke` (Playwright, real Chrome) into `screenshots/`:

| File | What it shows |
|---|---|
| `01-login.png` | Login screen (admin / user role picker) |
| `02-dashboard.png` | Dashboard — health stats, agents table, workspaces |
| `03-agents-filtered.png` | `/agents?q=exp&status=running` — URL-driven filtering |
| `04-chat.png` | `/chat/t2` — sidebar layout + conversation pane |
| `05-memory.png` | `/memory?q=HKT` — search-driven memory browser |
| `06-settings.png` | `/settings` as **admin** |
| `07-rbac-blocked.png` | `/settings` as **user** → redirected to dashboard |

Run it yourself (dev server must be up on :5199):

```bash
npx vite --port 5199 &
npm run smoke
```
