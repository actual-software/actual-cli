# Research Notes: ADR-Worthy Decisions for Generic TS/Next.js Bank

> Extracted from Next.js 16.1.6 official docs, Matt Pocock's Total TypeScript, and Kent C. Dodds' architecture posts. Each section identifies non-obvious decisions that meet the quality criteria from GOOD_ADRS.md.

## Sources

| Source | Version | Content |
|--------|---------|---------|
| Next.js Docs: Server/Client Components | 16.1.6 | RSC model, `'use client'` boundary, composition patterns |
| Next.js Docs: Data Fetching | 16.1.6 | fetch in SC, `use()` API, streaming, parallel fetching |
| Next.js Docs: Server Actions & Mutations | 16.1.6 | `'use server'`, form actions, revalidation, `refresh()` |
| Next.js Docs: Caching | 16.1.6 | 4-layer cache model, revalidation APIs, opt-out patterns |
| Next.js Docs: Error Handling | 16.1.6 | `error.js`, `global-error.js`, expected vs uncaught errors |
| Next.js Docs: Proxy (formerly Middleware) | 16.1.6 | `proxy.ts` convention, matcher config, CORS, cookies |
| Next.js Docs: TypeScript | 16.1.6 | IDE plugin, typed routes, `next-env.d.ts`, strict config |
| Next.js Docs: Environment Variables | 16.1.6 | `NEXT_PUBLIC_` prefix, load order, runtime vs build-time |
| Next.js Docs: Project Structure | 16.1.6 | File conventions, route groups, private folders, colocation |
| Next.js Docs: Route Handlers | 16.1.6 | `route.ts`, HTTP methods, streaming, segment config |
| Next.js Docs: Forms | 16.1.6 | `useActionState`, `useFormStatus`, Zod validation |
| Matt Pocock: Total TypeScript Essentials Ch. 14 | 2025 | tsconfig base, `strict`, `noUncheckedIndexedAccess`, `module` |
| Kent C. Dodds: Modern Website 2021 | 2021 | MSW mocking, Prisma patterns, magic link auth, caching |

---

## Category 1: React Server Components & Rendering

### ADR Candidate: Default to Server Components, Use `'use client'` Only at Leaf Nodes

**Source**: Next.js docs - Server/Client Components

**Decision**: Components are Server Components by default. Only add `'use client'` to specific interactive leaf components (buttons, forms, search bars), not to layouts or parent containers.

**Why it's non-obvious**: Beginners often add `'use client'` to entire pages or layouts. The docs explicitly say: "add `'use client'` to specific interactive components instead of marking large parts of your UI as Client Components."

**Key details**:
- Once a file is marked `'use client'`, all its imports and child components become part of the client bundle
- Layouts should remain Server Components even if they contain one interactive child
- Use the `children` prop pattern to nest Server Components inside Client Components

### ADR Candidate: Use `children` Prop Pattern to Compose Server Components Inside Client Components

**Source**: Next.js docs - Interleaving Server and Client Components

**Decision**: When you need client-side behavior wrapping server-rendered content, pass Server Components as `children` to Client Components rather than importing Server Components inside Client Components.

**Why it's non-obvious**: Importing a Server Component inside a `'use client'` file makes it a Client Component. The slot/children pattern avoids this.

**Key pattern**:
```tsx
// Client Component with slot
'use client'
export function Modal({ children }: { children: React.ReactNode }) {
  return <div>{children}</div>
}

// Server Component composes them
import Modal from './ui/modal'
import Cart from './ui/cart' // Server Component
export default function Page() {
  return <Modal><Cart /></Modal>
}
```

### ADR Candidate: Create Client Component Wrappers for Third-Party Components Without `'use client'`

**Source**: Next.js docs - Third-party components

**Decision**: When a third-party component uses client features but lacks `'use client'`, create a one-line wrapper file that re-exports it with the directive.

**Why it's non-obvious**: You'd expect third-party components to "just work". They don't in Server Components if they use hooks/state internally.

### ADR Candidate: Wrap Context Providers in Client Components and Render Them as Deep as Possible

**Source**: Next.js docs - Context providers

**Decision**: Context is not supported in Server Components. Wrap providers in `'use client'` components that accept `children`, and place them as deep in the tree as possible (not wrapping `<html>`).

**Why it's non-obvious**: Docs explicitly say "render providers as deep as possible in the tree" so Next.js can optimize static parts of Server Components above the provider.

---

## Category 2: Data Fetching

### ADR Candidate: Fetch Data Directly in Server Components, Not Through API Routes

**Source**: Next.js docs - Data Fetching

**Decision**: In the App Router, fetch data directly in async Server Components (via `fetch`, ORM, or database client). Do not create separate API routes just to feed data to your own pages.

**Why it's non-obvious**: Developers from Pages Router or SPA backgrounds instinctively create `/api/` endpoints for everything. App Router eliminates the need for this pattern for page data.

### ADR Candidate: Use `Promise.all()` for Parallel Data Fetching in Server Components

**Source**: Next.js docs - Parallel data fetching

**Decision**: When a Server Component needs multiple independent data sources, initiate all fetches before awaiting any, then use `Promise.all()` to resolve them in parallel.

**Why it's non-obvious**: Sequential `await` is the default developer instinct. The docs show that `getAlbums` is blocked until `getArtist` resolves unless you initiate both first.

### ADR Candidate: Use React `use()` API to Stream Data from Server to Client Components

**Source**: Next.js docs - Streaming data with the `use` API

**Decision**: Pass un-awaited promises from Server Components to Client Components as props. The Client Component resolves them with `use()` inside a `<Suspense>` boundary.

**Why it's non-obvious**: Most devs await everything in the Server Component. Passing promises enables streaming: the server sends HTML immediately and data arrives later.

### ADR Candidate: Wrap Data-Fetching Components in `<Suspense>` with Meaningful Skeleton Fallbacks

**Source**: Next.js docs - Streaming, loading.js

**Decision**: Use `<Suspense>` boundaries with skeleton/loading fallbacks around components that fetch data. Use `loading.js` for full-page loading states and `<Suspense>` for granular streaming.

**Why it's non-obvious**: Without Suspense boundaries, slow data requests block the entire page. Granular Suspense allows partial rendering.

### ADR Candidate: Use `React.cache()` to Deduplicate Non-Fetch Data Requests

**Source**: Next.js docs - Deduplicate requests and cache data

**Decision**: When using an ORM or database client (not `fetch`), wrap data access functions with `React.cache()` to memoize results within the same request.

**Why it's non-obvious**: `fetch` GET requests are auto-memoized by React, but ORM/database calls are not. Without `React.cache()`, calling `getUser()` in both a layout and page results in two database queries.

---

## Category 3: Server Actions & Mutations

### ADR Candidate: Define Server Actions in Separate `'use server'` Files, Not Inline in Client Components

**Source**: Next.js docs - Server Actions - Client Components

**Decision**: Server Actions cannot be defined inside Client Components. Define them in files with `'use server'` at the top and import them.

**Why it's non-obvious**: The inline `'use server'` syntax works in Server Components, leading devs to expect it works everywhere.

### ADR Candidate: Return Error State from Server Actions Instead of Throwing

**Source**: Next.js docs - Error Handling - Expected Errors

**Decision**: For expected errors (validation failures, API errors), return an error object from the Server Action. Don't use `try/catch` and don't throw. Use `useActionState` to display errors.

**Why it's non-obvious**: The traditional pattern is try/catch/throw. The docs explicitly say "avoid using `try`/`catch` blocks and throw errors. Instead, model expected errors as return values."

### ADR Candidate: Call `revalidatePath()` or `revalidateTag()` Before `redirect()` in Server Actions

**Source**: Next.js docs - Server Actions - Redirecting

**Decision**: `redirect()` throws a control-flow exception and stops execution. Always call revalidation functions before `redirect()`.

**Why it's non-obvious**: Code after `redirect()` never executes. If you put `revalidatePath()` after it, your cache won't be updated.

### ADR Candidate: Validate Server Action Inputs with Zod Schemas

**Source**: Next.js docs - Forms - Form Validation

**Decision**: Use Zod for server-side form validation in Server Actions. Use `schema.safeParse()` and return early with errors if validation fails.

**Why it's non-obvious**: `FormData` values are all strings. Without validation, you'll have type mismatches and potential security issues.

---

## Category 4: Caching

### ADR Candidate: Understand Next.js's 4-Layer Cache Model Before Adding Custom Caching

**Source**: Next.js docs - Caching

**Decision**: Next.js has 4 caching layers: Request Memoization (per-request, React), Data Cache (persistent, server), Full Route Cache (persistent, server), Router Cache (client). Know which layer applies before adding custom caching.

**Why it's non-obvious**: Most developers think of "caching" as one thing. Next.js has four distinct mechanisms with different lifetimes, locations, and invalidation strategies.

**Key table**:
| Mechanism | Where | Duration | Opt-out |
|-----------|-------|----------|---------|
| Request Memoization | Server (React) | Per-request | AbortController signal |
| Data Cache | Server | Persistent | `{ cache: 'no-store' }` |
| Full Route Cache | Server | Persistent | Dynamic APIs or `dynamic = 'force-dynamic'` |
| Router Cache | Client | Session | `router.refresh()` or revalidation in Server Action |

### ADR Candidate: Use `{ cache: 'no-store' }` to Opt Out of Data Caching for Dynamic Data

**Source**: Next.js docs - Caching - Opting Out

**Decision**: `fetch` responses are not cached by default in dynamic rendering. To explicitly prevent caching in static rendering, use `{ cache: 'no-store' }`. To explicitly cache, use `{ cache: 'force-cache' }`.

**Why it's non-obvious**: The default depends on whether the route is statically or dynamically rendered. Developers need to be explicit.

### ADR Candidate: Use `revalidateTag()` with Tagged Fetches for Fine-Grained Cache Invalidation

**Source**: Next.js docs - Caching - fetch options.next.tags

**Decision**: Tag fetch requests with `{ next: { tags: ['posts'] } }` and invalidate with `revalidateTag('posts')` in Server Actions. This is more granular than `revalidatePath()`.

**Why it's non-obvious**: `revalidatePath()` purges everything for a path. Tags let you invalidate specific data without re-rendering unrelated parts.

---

## Category 5: Error Handling

### ADR Candidate: Use `error.js` Boundaries for Uncaught Exceptions, Not for Expected Errors

**Source**: Next.js docs - Error Handling

**Decision**: `error.js` files create React error boundaries for uncaught exceptions. Expected errors (validation, API failures) should be handled with return values and UI state, not by throwing into error boundaries.

**Why it's non-obvious**: Error boundaries must be Client Components. They don't catch errors in event handlers. Overusing them for expected errors leads to poor UX.

### ADR Candidate: Add `global-error.js` to Handle Root Layout Errors

**Source**: Next.js docs - Error Handling - Global errors

**Decision**: `error.js` doesn't catch errors in the root layout. Add `global-error.js` at the app root which must define its own `<html>` and `<body>` tags.

**Why it's non-obvious**: If the root layout throws and there's no `global-error.js`, the user sees a blank page.

---

## Category 6: Proxy (Middleware)

### ADR Candidate: Use `proxy.ts` Instead of `middleware.ts` in Next.js 16+

**Source**: Next.js docs - proxy.js

**Decision**: Next.js 16 renamed `middleware.ts` to `proxy.ts`. The function export is `proxy()` instead of `middleware()`. Run the codemod: `npx @next/codemod@canary middleware-to-proxy .`

**Why it's non-obvious**: This is a breaking naming change in Next.js 16. Old tutorials all reference `middleware.ts`.

### ADR Candidate: Use Proxy Only for Edge-Appropriate Logic (Auth Checks, Redirects, Headers)

**Source**: Next.js docs - proxy.js - Migration to Proxy

**Decision**: Proxy (formerly middleware) should be used as a last resort. It runs on every matched route and is meant for network-boundary logic: auth checks, redirects, header manipulation, CORS. Don't use it for business logic.

**Why it's non-obvious**: Docs explicitly say "we recommend users avoid relying on Middleware unless no other options exist." The rename to "proxy" reinforces it's for network-boundary work.

### ADR Candidate: Always Configure a Matcher Pattern for Proxy

**Source**: Next.js docs - proxy.js - Matcher

**Decision**: Proxy runs on every route by default. Always configure `export const config = { matcher: [...] }` to limit it to relevant paths. Exclude `_next/static`, `_next/image`, and `favicon.ico`.

**Why it's non-obvious**: Without a matcher, proxy runs on every single request including static assets, hurting performance.

---

## Category 7: Environment Variables

### ADR Candidate: Prefix Client-Exposed Environment Variables with `NEXT_PUBLIC_`

**Source**: Next.js docs - Environment Variables

**Decision**: Only variables prefixed with `NEXT_PUBLIC_` are inlined into the client bundle at build time. Server-only secrets must NOT have this prefix.

**Why it's non-obvious**: Developers from other frameworks may not realize that non-prefixed variables are completely invisible on the client (replaced with empty strings).

### ADR Candidate: Use `server-only` Package to Prevent Accidental Client Import of Server Code

**Source**: Next.js docs - Server/Client Components - Preventing environment poisoning

**Decision**: Add `import 'server-only'` to files that use secrets or server-only logic. This causes a build-time error if the file is imported into a Client Component.

**Why it's non-obvious**: Without this, server code silently fails on the client (env vars become empty strings) rather than erroring at build time.

### ADR Candidate: Use Dynamic Rendering for Runtime Environment Variables

**Source**: Next.js docs - Environment Variables - Runtime Environment Variables

**Decision**: `NEXT_PUBLIC_` variables are frozen at build time. For runtime configuration that changes per-deployment, use server-side `process.env` in dynamically rendered pages (opt in with `connection()`, `cookies()`, etc.).

**Why it's non-obvious**: Many developers expect env vars to be evaluated at runtime. `NEXT_PUBLIC_` vars are baked in at build time.

---

## Category 8: TypeScript Configuration

### ADR Candidate: Enable `strict` and `noUncheckedIndexedAccess` in tsconfig.json

**Source**: Matt Pocock - Total TypeScript Essentials Ch. 14

**Decision**: Always enable `"strict": true` and `"noUncheckedIndexedAccess": true`. `strict` is the baseline for modern TS. `noUncheckedIndexedAccess` catches array/object index access that could be `undefined`.

**Why it's non-obvious**: `noUncheckedIndexedAccess` is not part of `strict` and catches a whole class of runtime errors (`TypeError: Cannot read property of undefined`) that `strict` alone misses.

**Recommended base**:
```json
{
  "compilerOptions": {
    "strict": true,
    "noUncheckedIndexedAccess": true,
    "skipLibCheck": true,
    "target": "es2022",
    "esModuleInterop": true,
    "isolatedModules": true,
    "moduleDetection": "force"
  }
}
```

### ADR Candidate: Use `module: "Preserve"` for Next.js Projects (Bundler Handles Resolution)

**Source**: Matt Pocock - Total TypeScript Essentials Ch. 14

**Decision**: When using a bundler (Next.js, Vite, etc.), set `module: "Preserve"` (implies `moduleResolution: "Bundler"`). Only use `NodeNext` when transpiling with `tsc` directly.

**Why it's non-obvious**: Many projects still use `module: "esnext"` or `module: "commonjs"`. `Preserve` is the correct choice for bundled apps.

### ADR Candidate: Enable `typedRoutes` for Type-Safe `next/link` and `next/navigation`

**Source**: Next.js docs - TypeScript - Statically Typed Links

**Decision**: Set `typedRoutes: true` in `next.config.ts`. This gives compile-time errors for invalid route strings in `<Link href="...">` and `router.push(...)`.

**Why it's non-obvious**: Most developers only discover broken links at runtime. This catches them at compile time.

### ADR Candidate: Use `import type` for Type-Only Imports

**Source**: Matt Pocock - Total TypeScript Essentials Ch. 14

**Decision**: Use `import type { X }` or `import { type X, regularImport }` for type-only imports. Enable `verbatimModuleSyntax: true` to enforce this.

**Why it's non-obvious**: Without `import type`, bundlers may include unnecessary module side effects. With `verbatimModuleSyntax`, TypeScript enforces explicit type imports.

---

## Category 9: Project Structure

### ADR Candidate: Use Route Groups for Layout Segmentation Without Affecting URLs

**Source**: Next.js docs - Project Structure - Route Groups

**Decision**: Use `(folderName)` route groups to organize routes by domain (e.g., `(marketing)`, `(dashboard)`) and apply different layouts without changing URL structure.

**Why it's non-obvious**: You can have multiple root layouts by removing the top-level `layout.js` and adding one inside each route group.

### ADR Candidate: Use Private Folders (`_folder`) for Non-Routable Code Inside `app/`

**Source**: Next.js docs - Project Structure - Private Folders

**Decision**: Prefix folders with `_` (e.g., `_components`, `_lib`) to opt them out of routing. This prevents accidental route creation from helper files.

**Why it's non-obvious**: Any folder in `app/` can become a route if someone adds a `page.tsx`. Private folders prevent this.

### ADR Candidate: Co-locate Route-Specific Code Inside Route Segments

**Source**: Next.js docs - Project Structure - Colocation

**Decision**: Files in `app/` folders are NOT publicly accessible until a `page.js` or `route.js` is added. You can safely co-locate components, tests, and utilities alongside route files.

**Why it's non-obvious**: Developers from Pages Router assume every file in the routes directory is a route. In App Router, only `page.js` and `route.js` create routes.

---

## Category 10: Route Handlers (API Routes)

### ADR Candidate: Use Web Standard Request/Response in Route Handlers

**Source**: Next.js docs - Route Handlers

**Decision**: Route Handlers in the App Router use the Web `Request` and `Response` APIs, not `req`/`res` from Express. Return `Response.json()` for JSON responses.

**Why it's non-obvious**: Developers from Pages Router's `api/` routes expect `req.body` and `res.json()`. App Router uses standard web APIs.

### ADR Candidate: Route Handler GET Methods Are Statically Cached by Default

**Source**: Next.js docs - Route Handlers - version history

**Decision**: As of Next.js 15, GET handlers in Route Handlers are dynamic by default (changed from static). Use `export const revalidate = N` or `{ cache: 'force-cache' }` to opt into caching.

**Why it's non-obvious**: The default changed between versions. Developers may assume GET handlers are cached or uncached depending on what version docs they read.

---

## Category 11: Forms and Validation

### ADR Candidate: Use `useActionState` for Form State Management with Server Actions

**Source**: Next.js docs - Forms

**Decision**: Use React's `useActionState` hook (not `useState` + manual fetch) to manage form state when using Server Actions. It provides `state`, `formAction`, and `pending` values.

**Why it's non-obvious**: The old pattern of `useState` + `fetch` + `loading` state is replaced by a single hook that integrates with the Server Action lifecycle.

### ADR Candidate: Use `useFormStatus` in a Separate Component for Submit Button Loading State

**Source**: Next.js docs - Forms - Pending States

**Decision**: `useFormStatus` must be used in a child component of the `<form>`, not the component that renders the form. Create a separate `<SubmitButton>` component.

**Why it's non-obvious**: `useFormStatus` reads the status of the parent `<form>`. If used in the same component that renders the form, it won't have access to the form context.

### ADR Candidate: Use `.bind()` to Pass Additional Arguments to Server Actions

**Source**: Next.js docs - Forms - Passing additional arguments

**Decision**: Use `serverAction.bind(null, userId)` to pass extra arguments to Server Actions from Client Components. The bound argument becomes the first parameter.

**Why it's non-obvious**: Hidden form fields are the traditional approach. `.bind()` is more type-safe and doesn't expose values in rendered HTML.

---

## Category 12: MSW for Local Development (From Kent C. Dodds)

### ADR Candidate: Use MSW to Mock External APIs for Offline-Capable Development

**Source**: Kent C. Dodds - How I Built a Modern Website in 2021

**Decision**: Use MSW (Mock Service Worker) to intercept HTTP requests in Node.js during development. Start with `node --require ./mocks .` to enable mocking without code changes.

**Why it's non-obvious**: Most developers mock at the function level. MSW mocks at the network level, making it non-intrusive to the codebase and reusable for E2E tests.

---

## Candidate ADR Summary

Total candidates extracted: **~35 ADR-worthy decisions**

### By priority (non-obvious + high impact):

**Must-have (would prevent common mistakes)**:
1. Default to Server Components, `'use client'` only at leaf nodes
2. Return error state from Server Actions, don't throw
3. Enable `strict` + `noUncheckedIndexedAccess` in tsconfig
4. Use `server-only` package for server modules
5. Prefix client env vars with `NEXT_PUBLIC_`, understand build-time inlining
6. Call revalidation before `redirect()` in Server Actions
7. Use `proxy.ts` not `middleware.ts` in Next.js 16+
8. Validate Server Action inputs with Zod
9. Use `children` prop pattern for Server/Client composition
10. Use `React.cache()` for non-fetch data deduplication

**Important (improves architecture)**:
11. Fetch data in Server Components, not through API routes
12. Use `Promise.all()` for parallel data fetching
13. Understand the 4-layer cache model
14. Use `<Suspense>` with meaningful fallbacks
15. Use `useActionState` for form state management
16. Configure proxy matcher patterns
17. Use route groups for layout segmentation
18. Use `module: "Preserve"` for bundled projects
19. Enable `typedRoutes` for type-safe links
20. Use `import type` with `verbatimModuleSyntax`

**Nice-to-have (good practice, somewhat obvious)**:
21. Create Client Component wrappers for third-party libs
22. Define Server Actions in separate `'use server'` files
23. Use `revalidateTag()` for fine-grained cache invalidation
24. Add `global-error.js` for root layout errors
25. Use Web Standard Request/Response in Route Handlers
26. Co-locate route-specific code inside route segments
27. Use private folders for non-routable code
28. Use `.bind()` for additional Server Action arguments
29. Use `useFormStatus` in separate child component
30. Stream data with `use()` API
31. Render context providers as deep as possible
32. Use `error.js` for uncaught exceptions only
33. Use runtime env vars with dynamic rendering
34. Route Handler GET default caching behavior
35. Use MSW for offline-capable development
