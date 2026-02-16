-- Seed adr_bank with 20 curated generic ADRs for TypeScript / Next.js 16+
-- Run in Supabase SQL Editor (requires service_role or superuser)
--
-- Category IDs reference scripts/adr_category_list.json taxonomy (staging branch)
-- applies_to_languages / applies_to_frameworks use lowercase identifiers matching
-- the CLI's languages.json / frameworks.json

INSERT INTO public.adr_bank
  (title, context, policies, instructions, category_id, category_name, category_path, applies_to_languages, applies_to_frameworks)
VALUES

-- =============================================================================
-- TIER 1: Must-Have (Prevent Common Mistakes)
-- =============================================================================

-- ADR-001
(
  'Default to Server Components and Restrict ''use client'' to Leaf Interactive Components',
  'Next.js 15+ renders all components as Server Components by default. Adding ''use client'' to a file makes it AND all its imports part of the client bundle, increasing JS sent to the browser. Misplacing the directive on layouts or parent components unnecessarily bloats the client.',
  ARRAY[
    'Keep layouts, pages, and data-fetching components as Server Components (no ''use client'' directive)',
    'Add ''use client'' only to the smallest leaf components that require hooks, event handlers, or browser APIs',
    'Never add ''use client'' to a file just because it imports a Client Component — the import boundary handles this automatically'
  ],
  ARRAY[
    'Check if a component uses useState, useEffect, onClick, onChange, or browser APIs (localStorage, window) — only then add ''use client''',
    'Extract interactive parts into small dedicated Client Components (e.g., <SearchBar />, <LikeButton />) imported into Server Component parents',
    'Pass data from Server Components to Client Components via serializable props'
  ],
  '169', 'Rendering Model', 'UI/UX & Frontend Decisions > Rendering Model',
  '{typescript}', '{nextjs}'
),

-- ADR-002
(
  'Return Error State from Server Actions Instead of Throwing',
  'Server Actions handle form submissions and mutations. For expected errors (validation failure, API error), throwing exceptions triggers error boundaries which replace the form UI. Returning error state preserves the form and lets the user correct their input.',
  ARRAY[
    'Return an object with error information from Server Actions for expected failures instead of throwing',
    'Use useActionState hook in the Client Component to read the returned error state and display messages',
    'Reserve throwing/error boundaries for unexpected, unrecoverable errors only'
  ],
  ARRAY[
    'Structure Server Action return as { errors?: Record<string, string[]>, message?: string } for validation failures',
    'Use const [state, formAction, pending] = useActionState(serverAction, initialState) to consume the error state',
    'Display error messages conditionally: {state?.errors?.email && <p>{state.errors.email[0]}</p>}'
  ],
  '71', 'Error handling style', 'Language & Paradigm > Coding Conventions > Error handling style',
  '{typescript}', '{nextjs}'
),

-- ADR-003
(
  'Enable TypeScript Strict Mode and noUncheckedIndexedAccess',
  'TypeScript''s strict mode catches null/undefined issues and enforces stronger function parameter checks. noUncheckedIndexedAccess (not included in strict) adds T | undefined to array/object index access, catching a common class of runtime TypeError crashes.',
  ARRAY[
    'Set strict: true in tsconfig.json for all projects',
    'Set noUncheckedIndexedAccess: true to catch unsafe array/object index access'
  ],
  ARRAY[
    'Add to compilerOptions: { "strict": true, "noUncheckedIndexedAccess": true }',
    'Handle the resulting T | undefined types with narrowing: const item = arr[i]; if (!item) return;',
    'Do not start projects with strict: false — many libraries (zod, tRPC, xstate) assume strict mode'
  ],
  '167', 'Type systems', 'Build, Delivery & Tooling > Developer Tooling > Type systems',
  '{typescript}', '{}'
),

-- ADR-004
(
  'Use server-only Package to Prevent Server Code Leaking to Client',
  'Files with server secrets (API keys, database queries) can be accidentally imported into Client Components. Without protection, server-only env vars are silently replaced with empty strings on the client, causing silent failures instead of build errors.',
  ARRAY[
    'Add import ''server-only'' at the top of any file that accesses non-NEXT_PUBLIC_ environment variables, database clients, or secret keys'
  ],
  ARRAY[
    'Install the package: npm install server-only',
    'Add import ''server-only'' as the first import in data-access files, auth utilities, and any module using process.env.SECRET_*',
    'If a file needs both server and client exports, split it into two files'
  ],
  '143', 'Storage', 'Security & Compliance > Secrets & Credentials > Storage',
  '{typescript}', '{nextjs}'
),

-- ADR-005
(
  'Prefix Client-Exposed Environment Variables with NEXT_PUBLIC_',
  'Next.js only inlines environment variables prefixed with NEXT_PUBLIC_ into the client JavaScript bundle at build time. Non-prefixed variables are replaced with empty strings on the client. NEXT_PUBLIC_ values are frozen at build time and visible to anyone inspecting the bundle.',
  ARRAY[
    'Prefix all client-needed environment variables with NEXT_PUBLIC_',
    'Never prefix secret keys, database URLs, or API tokens with NEXT_PUBLIC_',
    'Understand that NEXT_PUBLIC_ values are frozen at build time — they cannot change per-deployment without rebuilding'
  ],
  ARRAY[
    'For analytics IDs, public API URLs, and feature flags readable on client: use NEXT_PUBLIC_ANALYTICS_ID',
    'For API keys, DB credentials, and auth secrets: use DB_PASSWORD (no prefix)',
    'For runtime configuration that varies per deployment, use server-side process.env in dynamically rendered routes (opt in with connection() or cookies())'
  ],
  '127', 'Environment strategy', 'Runtime Environment & Execution Patterns > Configuration & Environment Management > Environment strategy',
  '{typescript}', '{nextjs}'
),

-- ADR-006
(
  'Call Revalidation Functions Before redirect() in Server Actions',
  'Next.js redirect() throws a framework-handled control-flow exception that stops all subsequent code execution. If revalidatePath() or revalidateTag() is called after redirect(), the cache will not be updated, leading to stale data on the destination page.',
  ARRAY[
    'Always call revalidatePath() or revalidateTag() before redirect() in Server Actions'
  ],
  ARRAY[
    'Correct order: revalidatePath(''/posts''); redirect(''/posts'');',
    'Incorrect order: redirect(''/posts''); revalidatePath(''/posts''); // NEVER EXECUTES',
    'This applies to any code after redirect() — it will not run'
  ],
  '108', 'Caching strategy', 'Data Storage, Modeling & Access > Data Access Patterns > Caching strategy',
  '{typescript}', '{nextjs}'
),

-- ADR-007
(
  'Use proxy.ts Instead of middleware.ts in Next.js 16+',
  'Next.js 16 deprecated middleware.ts and renamed it to proxy.ts to clarify its purpose as a network-boundary proxy, not Express-style middleware. The function export changed from middleware() to proxy().',
  ARRAY[
    'Use proxy.ts (not middleware.ts) for request interception in Next.js 16+ projects',
    'Export a function named proxy() (not middleware())',
    'Limit proxy to edge-appropriate logic: auth checks, redirects, header manipulation, CORS'
  ],
  ARRAY[
    'Migrate existing middleware: npx @next/codemod@canary middleware-to-proxy .',
    'Always configure a matcher: export const config = { matcher: [''/((?!api|_next/static|_next/image|favicon.ico).*)''] }',
    'Do not use proxy for business logic, database queries, or complex computations'
  ],
  '73', 'Web frameworks', 'Frameworks & Libraries > Application Frameworks > Web frameworks',
  '{typescript}', '{nextjs}'
),

-- ADR-008
(
  'Validate Server Action Inputs with Zod Schemas',
  'FormData values are all strings and can be tampered with by the client. Server Actions are publicly accessible endpoints. Without validation, malformed or malicious input can cause data corruption or security issues.',
  ARRAY[
    'Validate all Server Action inputs using Zod schemas before processing',
    'Use schema.safeParse() and return early with field errors if validation fails'
  ],
  ARRAY[
    'Define a Zod schema: const schema = z.object({ email: z.string().email(), amount: z.coerce.number().positive() })',
    'Parse form data: const result = schema.safeParse({ email: formData.get(''email''), amount: formData.get(''amount'') })',
    'Return errors: if (!result.success) return { errors: result.error.flatten().fieldErrors }',
    'Use zod-form-data for complex form validation with files and arrays'
  ],
  '140', 'Input validation, output encoding', 'Security & Compliance > Secure Coding Practices > Input validation, output encoding',
  '{typescript}', '{nextjs}'
),

-- ADR-009
(
  'Use Children Prop Pattern to Compose Server Components Inside Client Components',
  'Importing a Server Component file inside a ''use client'' file makes it a Client Component (all imports in a client boundary become client code). To keep server-rendered content inside interactive client wrappers, pass Server Components as children props.',
  ARRAY[
    'Never import Server Components directly inside ''use client'' files',
    'Use the children (or named slot) prop pattern to nest Server Components within Client Component wrappers'
  ],
  ARRAY[
    'Client wrapper accepts children: ''use client''; export function Modal({ children }: { children: React.ReactNode }) { ... }',
    'Server Component composes: import Modal from ''./modal''; import Cart from ''./cart''; export default function Page() { return <Modal><Cart /></Modal> }',
    'Cart renders on the server; Modal handles client-side interactivity'
  ],
  '169', 'Rendering Model', 'UI/UX & Frontend Decisions > Rendering Model',
  '{typescript}', '{nextjs}'
),

-- ADR-010
(
  'Use React.cache() to Deduplicate Non-Fetch Data Access',
  'React automatically memoizes fetch() GET requests within a render pass, but ORM calls, database queries, and CMS client calls are not memoized. Without React.cache(), calling getUser() in both a layout and a page results in two separate database queries for the same data.',
  ARRAY[
    'Wrap data access functions that don''t use fetch() with React.cache() when they may be called multiple times in a single request'
  ],
  ARRAY[
    'Import cache: import { cache } from ''react''',
    'Wrap the function: export const getUser = cache(async (id: string) => { return db.user.findUnique({ where: { id } }) })',
    'Call getUser(id) freely in layouts, pages, and components — it executes once per request',
    'Note: React.cache() is scoped to the current request only, not across requests'
  ],
  '108', 'Caching strategy', 'Data Storage, Modeling & Access > Data Access Patterns > Caching strategy',
  '{typescript}', '{nextjs}'
),

-- =============================================================================
-- TIER 2: Important (Improves Architecture)
-- =============================================================================

-- ADR-011
(
  'Fetch Data Directly in Server Components, Not Through Self-Referencing API Routes',
  'In the App Router, Server Components can directly query databases, call ORMs, and access APIs. Creating /api/ routes solely to fetch data for your own pages adds unnecessary network hops and complexity.',
  ARRAY[
    'Fetch data directly in Server Components using async/await with your ORM, database client, or fetch()',
    'Reserve Route Handlers (app/api/) for external consumers: webhooks, mobile apps, third-party integrations'
  ],
  ARRAY[
    'In page.tsx: const posts = await db.post.findMany() — no need for an API route',
    'For shared data logic, extract into a server-side utility function and call it directly',
    'Use Route Handlers only when an external client needs the endpoint'
  ],
  '169', 'Rendering Model', 'UI/UX & Frontend Decisions > Rendering Model',
  '{typescript}', '{nextjs}'
),

-- ADR-012
(
  'Use Promise.all() for Parallel Data Fetching in Server Components',
  'Sequential await statements in Server Components block each other. If getArtist() takes 200ms and getAlbums() takes 300ms, sequential fetching takes 500ms while parallel fetching takes 300ms.',
  ARRAY[
    'When multiple independent data fetches are needed in a single component, initiate all fetches before awaiting any and resolve with Promise.all()'
  ],
  ARRAY[
    'Initiate: const artistData = getArtist(id); const albumsData = getAlbums(id);',
    'Resolve: const [artist, albums] = await Promise.all([artistData, albumsData])',
    'Use Promise.allSettled() instead if one failure should not block the others'
  ],
  '125', 'Concurrency patterns', 'Runtime Environment & Execution Patterns > Process Model > Concurrency patterns',
  '{typescript}', '{nextjs}'
),

-- ADR-013
(
  'Use Suspense Boundaries with Skeleton Fallbacks for Streaming',
  'Without Suspense boundaries, slow data requests block the entire page from rendering. Suspense enables streaming: fast content renders immediately while slow content shows a fallback until ready.',
  ARRAY[
    'Wrap data-fetching components in <Suspense> boundaries with meaningful skeleton fallbacks',
    'Use loading.js for full-page loading states, <Suspense> for granular component-level streaming'
  ],
  ARRAY[
    'Component-level: <Suspense fallback={<PostListSkeleton />}><PostList /></Suspense>',
    'Page-level: Create app/blog/loading.tsx that exports a skeleton component',
    'Design skeletons that match the layout of the loaded content (cover photo, title placeholder, etc.)'
  ],
  '169', 'Rendering Model', 'UI/UX & Frontend Decisions > Rendering Model',
  '{typescript}', '{nextjs}'
),

-- ADR-014
(
  'Use useActionState for Form State Management with Server Actions',
  'React 19 introduced useActionState to replace the pattern of useState + manual fetch + loading state for forms. It provides state, a form action function, and a pending boolean in one hook, with built-in progressive enhancement.',
  ARRAY[
    'Use useActionState (not useState + fetch) for forms that submit to Server Actions',
    'The Server Action''s first parameter becomes prevState when used with useActionState'
  ],
  ARRAY[
    'Hook: const [state, formAction, pending] = useActionState(createPost, initialState)',
    'Form: <form action={formAction}>...</form>',
    'Server Action signature changes: async function createPost(prevState: State, formData: FormData)',
    'Display errors: {state?.message && <p>{state.message}</p>}',
    'Disable submit: <button disabled={pending}>Submit</button>'
  ],
  '174', 'Form validation strategy', 'UI/UX & Frontend Decisions > Interaction Patterns > Form validation strategy',
  '{typescript}', '{nextjs}'
),

-- ADR-015
(
  'Configure Proxy Matcher to Exclude Static Assets',
  'Proxy (formerly middleware) runs on every matched request. Without a matcher, it processes static files, images, and internal Next.js routes unnecessarily, adding latency to every asset load.',
  ARRAY[
    'Always export a config.matcher in proxy.ts that excludes _next/static, _next/image, and metadata files'
  ],
  ARRAY[
    'Standard matcher: export const config = { matcher: [''/((?!api|_next/static|_next/image|favicon.ico|sitemap.xml|robots.txt).*)''] }',
    'For auth-only proxy: export const config = { matcher: [''/dashboard/:path*'', ''/account/:path*''] }',
    'Matcher values must be constants — dynamic values are ignored at build time'
  ],
  '73', 'Web frameworks', 'Frameworks & Libraries > Application Frameworks > Web frameworks',
  '{typescript}', '{nextjs}'
),

-- ADR-016
(
  'Use Route Groups for Domain-Based Layout Segmentation',
  'Different sections of an app often need different layouts (marketing vs dashboard, public vs authenticated). Route groups (parenthesized folders) let you apply different layouts without affecting URL structure.',
  ARRAY[
    'Use (groupName) folders to segment routes by domain and apply different layouts',
    'Never rely on route groups to affect URL paths — they are purely organizational'
  ],
  ARRAY[
    'Create app/(marketing)/layout.tsx for public pages with header/footer',
    'Create app/(dashboard)/layout.tsx for authenticated pages with sidebar',
    'Each group can have its own loading.tsx, error.tsx, and template.tsx',
    'For separate root layouts, remove the top-level layout.tsx and add one inside each group (each must include <html> and <body>)'
  ],
  '70', 'Code organization patterns', 'Language & Paradigm > Coding Conventions > Code organization patterns',
  '{typescript}', '{nextjs}'
),

-- ADR-017
(
  'Use module Preserve for Next.js and Bundled TypeScript Projects',
  'When using a bundler (Next.js, Vite, Webpack), TypeScript should not transform imports — the bundler handles module resolution. module: Preserve (implying moduleResolution: Bundler) tells TypeScript to leave imports as-is.',
  ARRAY[
    'Set module: Preserve in tsconfig.json for projects using Next.js or any bundler',
    'Only use module: NodeNext when transpiling directly with tsc for Node.js consumption'
  ],
  ARRAY[
    'Add to compilerOptions: { "module": "Preserve", "noEmit": true }',
    'This implies moduleResolution: Bundler — no need to set it separately',
    'You do not need .js extensions on imports when using Preserve (the bundler resolves them)'
  ],
  '167', 'Type systems', 'Build, Delivery & Tooling > Developer Tooling > Type systems',
  '{typescript}', '{}'
),

-- ADR-018
(
  'Enable typedRoutes for Compile-Time Link Validation',
  'Broken internal links are only discovered at runtime or in manual testing. Next.js can generate type definitions for all routes, making <Link href=...> and router.push() type-checked at compile time.',
  ARRAY[
    'Enable typedRoutes: true in next.config.ts for TypeScript projects'
  ],
  ARRAY[
    'In next.config.ts: const nextConfig: NextConfig = { typedRoutes: true }',
    'Ensure .next/types/**/*.ts is in your tsconfig.json include array',
    'For dynamic strings, cast: router.push((''/blog/'' + slug) as Route)',
    'Types are auto-generated during next dev and next build'
  ],
  '167', 'Type systems', 'Build, Delivery & Tooling > Developer Tooling > Type systems',
  '{typescript}', '{nextjs}'
),

-- ADR-019
(
  'Use import type for Type-Only Imports with verbatimModuleSyntax',
  'Regular import statements can include module side effects even when only types are used. Without import type, bundlers may not tree-shake type-only imports correctly, and module side effects execute unnecessarily.',
  ARRAY[
    'Use import type { X } for imports that are only used as types',
    'Enable verbatimModuleSyntax: true in tsconfig.json to enforce this at compile time'
  ],
  ARRAY[
    'Type-only: import type { User } from ''./types''',
    'Mixed: import { type User, createUser } from ''./user''',
    'Add to compilerOptions: { "verbatimModuleSyntax": true }',
    'verbatimModuleSyntax replaces the older importsNotUsedAsValues and preserveValueImports options'
  ],
  '167', 'Type systems', 'Build, Delivery & Tooling > Developer Tooling > Type systems',
  '{typescript}', '{}'
),

-- ADR-020
(
  'Understand the Next.js 4-Layer Cache Model',
  'Next.js caches at four distinct layers with different lifetimes and locations. Adding custom caching without understanding these layers leads to stale data, over-caching, or redundant work.',
  ARRAY[
    'Know which cache layer applies before adding custom caching logic',
    'Use revalidatePath() in Server Actions to invalidate Data Cache + Full Route Cache + Router Cache simultaneously',
    'Use router.refresh() on the client to clear Router Cache without affecting server caches'
  ],
  ARRAY[
    'Request Memoization: Auto-deduplicates fetch GET in one render pass. Per-request only.',
    'Data Cache: Persistent server cache for fetch results. Opt out with { cache: ''no-store'' }. Revalidate with revalidateTag/revalidatePath.',
    'Full Route Cache: Cached HTML + RSC payload for static routes. Cleared by revalidation or redeployment.',
    'Router Cache: Client-side RSC payload cache. Cleared on page refresh, revalidation in Server Actions, or router.refresh().'
  ],
  '108', 'Caching strategy', 'Data Storage, Modeling & Access > Data Access Patterns > Caching strategy',
  '{typescript}', '{nextjs}'
);
