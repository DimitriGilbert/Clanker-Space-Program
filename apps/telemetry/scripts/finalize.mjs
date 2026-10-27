// Post-build: promote the SPA-mode shell (`_shell.html`) to
// `index.html` — the static contract csp-server's `spa_dir` serves
// (`/` → index.html, `/assets/*` → assets/*).
import { copyFileSync, existsSync, readdirSync, readFileSync } from 'node:fs'
import { extname, join, relative } from 'node:path'

const shell = 'dist/client/_shell.html'
if (!existsSync(shell)) {
  console.error('finalize: dist/client/_shell.html missing — was `spa` mode on?')
  process.exit(1)
}
copyFileSync(shell, 'dist/client/index.html')
console.log('finalize: dist/client/index.html written')

// §2.12.3 leak gate — `/` and `/assets/*` are served *token-free*
// (csp-server/src/http.rs), so a bearer baked into the bundle is a
// public secret to anyone who fetches it. ws.ts gates
// `VITE_CSP_TOKEN` on `import.meta.env.DEV` so the define folds to
// `''` in production, but the var can also reach `vite build` through
// a `.env` file this process never sees — scan the emitted assets
// rather than trusting the gate alone.
//
// Coverage mirrors what `spa_asset` can serve as text: index.html at
// `/`, plus every file under `assets/` (nested directories included —
// `clean_asset_path` admits multi-segment paths) whose extension maps
// to a text response. Binary types (png/woff/woff2/…) can't carry a
// readable token and are skipped.
const clientDir = 'dist/client'
const assetsDir = join(clientDir, 'assets')
const TEXT_EXT = new Set(['.js', '.mjs', '.css', '.html', '.json', '.map', '.svg'])

function* walkTextFiles(dir) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name)
    if (entry.isDirectory()) {
      yield* walkTextFiles(path)
    } else if (entry.isFile() && TEXT_EXT.has(extname(entry.name))) {
      yield path
    }
  }
}

const served = [
  join(clientDir, 'index.html'),
  ...(existsSync(assetsDir) ? [...walkTextFiles(assetsDir)] : []),
]
const bundles = served.map((path) => ({
  name: relative(clientDir, path),
  text: readFileSync(path, 'utf8'),
}))
if (bundles.length === 0) {
  console.error('finalize: no served text files under dist/client — did the client build emit?')
  process.exit(1)
}

const leaks = []

// 1. This shell's build-time value must not appear verbatim —
//    `includes` catches it however it was embedded (string literal,
//    template, comment). The length floor keeps a dev convenience
//    value (`VITE_CSP_TOKEN=x`) from matching every bundle; real
//    tokens are 64 hex chars — check 2's shape regardless.
const envToken = process.env.VITE_CSP_TOKEN
if (envToken && envToken.length >= 8) {
  for (const { name, text } of bundles) {
    if (text.includes(envToken)) {
      leaks.push(`${name}: contains the VITE_CSP_TOKEN build value`)
    }
  }
}

// 2. Nothing token-shaped either — csp-server's `generate_token` is
//    32 CSPRNG bytes rendered as 64 lowercase hex chars — whatever
//    env channel it arrived by (`.env` files are not visible here).
const TOKEN_LITERAL = /["'`]([0-9a-f]{64})["'`]/g
for (const { name, text } of bundles) {
  const hits = new Set()
  for (const match of text.matchAll(TOKEN_LITERAL)) hits.add(match[1])
  for (const hit of hits) {
    leaks.push(`${name}: token-shaped literal (64-hex) "${hit.slice(0, 16)}…"`)
  }
}

if (leaks.length > 0) {
  console.error('finalize: refusing to ship — bearer token in the token-free bundle:')
  for (const leak of leaks) console.error(`  - ${leak}`)
  process.exit(1)
}
console.log(`finalize: token-leak scan clean (${bundles.length} served file(s) scanned)`)
