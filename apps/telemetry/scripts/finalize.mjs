// Post-build: promote the SPA-mode shell (`_shell.html`) to
// `index.html` — the static contract csp-server's `spa_dir` serves
// (`/` → index.html, `/assets/*` → assets/*).
import { copyFileSync, existsSync } from 'node:fs'

const shell = 'dist/client/_shell.html'
if (!existsSync(shell)) {
  console.error('finalize: dist/client/_shell.html missing — was `spa` mode on?')
  process.exit(1)
}
copyFileSync(shell, 'dist/client/index.html')
console.log('finalize: dist/client/index.html written')
