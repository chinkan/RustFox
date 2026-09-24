// End-to-end white-screen + feature test against the REAL embedded portal
// binary. Serves web/dist compiled into the Rust binary via include_dir! —
// exactly what ships in production. Covers every M1 page: login, dashboard,
// chat (SSE stream + persistence), agents, memory, tasks, settings.
//
// Usage:
//   ./scripts/build-all.sh && cargo build --release --example portal_preview
//   (PORTAL_PREVIEW_PORT=8123 ./target/release/examples/portal_preview &)
//   node e2e-verify.mjs
import pw from '/home/kan/.nvm/versions/node/v24.13.0/lib/node_modules/@playwright/test/index.js'
const { chromium } = pw

const BASE = process.env.BASE || 'http://127.0.0.1:8123'
const TOKEN = process.env.TOKEN || 'preview-token'
const SHOT = (n) => `/tmp/pw-${n}.png`
const results = []
const errs = []

const browser = await chromium.launch({ headless: true })
const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 } })
const page = await ctx.newPage()

page.on('console', (m) => { if (m.type() === 'error') errs.push(m.text()) })
page.on('pageerror', (e) => errs.push('PAGEERROR: ' + e.message))
page.on('requestfailed', (r) => errs.push('REQFAIL: ' + r.url() + ' ' + (r.failure()?.errorText || '')))

function check(name, ok, detail = '') {
  results.push({ name, ok, detail })
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? '  — ' + detail : ''}`)
}
const bodyText = () => page.evaluate(() => document.body.innerText)

// ==================== 1. Login page renders (not white screen) ====================
await page.goto(BASE + '/', { waitUntil: 'networkidle' })
await page.waitForTimeout(600)
const title = await page.title()
check('page title', title.includes('RustFox'), `title="${title}"`)
const bodyLen = (await bodyText()).trim().length
check('SPA rendered (body text > 20 chars)', bodyLen > 20, `bodyLen=${bodyLen}`)
const loginCard = await page.locator('.login-card, form').count()
check('login card visible', loginCard > 0, `count=${loginCard}`)
await page.screenshot({ path: SHOT('01-login') })

// ==================== 2. Sign in with token ====================
const tokenInput = page.locator('#token, input[type=password], input[type=text]').first()
await tokenInput.fill(TOKEN)
await page.locator('button[type=submit]').click()
try {
  await page.waitForURL('**/dashboard', { timeout: 8000 })
  check('login redirects to /dashboard', true, page.url())
} catch {
  check('login redirects to /dashboard', false, `stuck at ${page.url()}`)
}
await page.waitForTimeout(800)
check('dashboard has content', (await bodyText()).length > 50)
await page.screenshot({ path: SHOT('02-dashboard') })

// ==================== 3. Deep-link + refresh (SPA fallback) ====================
await page.goto(BASE + '/settings', { waitUntil: 'networkidle' })
await page.waitForTimeout(700)
const setOk = page.url().includes('/settings') && (await bodyText()).length > 50
check('deep-link /settings works (SPA fallback embedded)', setOk, `url=${page.url()}`)
await page.reload({ waitUntil: 'networkidle' })
await page.waitForTimeout(500)
check('refresh on /settings keeps app alive', (await bodyText()).length > 50)
await page.screenshot({ path: SHOT('03-settings') })

// ==================== 4. Assets + auth API from embedded dist ====================
const jsReq = await page.evaluate(() => performance.getEntriesByType('resource').filter(r => r.name.includes('/assets/')).map(r => ({ u: r.name, s: r.responseStatus })))
const allAssetsOk = jsReq.length >= 2 && jsReq.every(r => r.s === 200)
check('embedded /assets/* served with 200', allAssetsOk, JSON.stringify(jsReq.map(r => `${r.s}:${r.u.split('/').pop()}`)))
const me = await page.evaluate(async () => (await fetch('/api/auth/me')).json())
check('GET /api/auth/me authenticated', JSON.stringify(me).includes('web'), JSON.stringify(me).slice(0, 120))

// ==================== 5. AGENTS page ====================
await page.goto(BASE + '/agents', { waitUntil: 'networkidle' })
await page.waitForTimeout(700)
let txt = await bodyText()
check('agents: main agent row rendered', txt.includes('rustfox') && txt.includes('telegram+web'), `len=${txt.length}`)
check('agents: subagent from AgentOps rendered', txt.includes('preview-agent'))
check('agents: skills section rendered', txt.includes('preview-skill') && /Skills \(\d+\)/.test(txt))
// client-side filter driven by URL search param (TanStack typed search)
await page.locator('input[type=search]').fill('nomatch-zzz')
await page.waitForTimeout(400)
txt = await bodyText()
check('agents: filter hides rows (empty state)', txt.includes('No agents match this filter.') && !txt.includes('telegram+web'))
await page.locator('input[type=search]').fill('')
await page.waitForTimeout(400)
// reload-skills mutation → success hint
await page.getByRole('button', { name: /Reload skills/i }).click()
await page.waitForTimeout(900)
txt = await bodyText()
check('agents: reload mutation shows result', /Reloaded \d+ skills, \d+ agents/.test(txt))
await page.screenshot({ path: SHOT('04-agents') })

// ==================== 6. MEMORY page ====================
await page.goto(BASE + '/memory', { waitUntil: 'networkidle' })
await page.waitForTimeout(800)
txt = await bodyText()
check('memory: browse mode renders seeded knowledge', txt.includes('The Death of Portia') && txt.includes('Telegram AI assistant'), 'empty-q browse')
check('memory: no error state on first load', !txt.includes('internal_error'))
// hybrid search (FTS fallback — no embeddings in preview server)
await page.locator('input[type=search]').fill('death')
await page.waitForTimeout(900)
txt = await bodyText()
check('memory: search returns fact + conversation hits', txt.includes('fact:') && txt.includes('[user]') && txt.includes('The Death of Portia'))
// kind filter → conversation only
await page.getByRole('button', { name: 'Conversation', exact: true }).click()
await page.waitForTimeout(900)
txt = await bodyText()
const convOnly = txt.includes('[user]') && !txt.includes('fact: favourite_author')
check('memory: kind filter narrows results', convOnly)
// empty-state path
await page.locator('input[type=search]').fill('zzzznope')
await page.waitForTimeout(900)
txt = await bodyText()
check('memory: empty state for no matches', txt.includes('Nothing matched'))
await page.screenshot({ path: SHOT('05-memory') })

// ==================== 7. TASKS (schedule) page ====================
await page.goto(BASE + '/tasks', { waitUntil: 'networkidle' })
await page.waitForTimeout(800)
txt = await bodyText()
check('tasks: seeded tasks rendered with cron', txt.includes('Daily weather briefing') && txt.includes('0 2 * * 0') && txt.includes('Inbox sweep'))
check('tasks: one_shot shows "once" chip', txt.includes('once'))
// execution history panel (GET /api/tasks/{id}/runs)
await page.getByRole('button', { name: 'Runs', exact: true }).first().click()
await page.waitForTimeout(900)
txt = await bodyText()
check('tasks: runs panel lists completed + failed', txt.includes('completed') && txt.includes('failed') && txt.includes('timeout after 60s'))
// toggle: disable → row stays but flips to paused + Enable offered;
// enable → restart hint + back to running (full round-trip through the API)
const rowsBefore = await page.locator('tbody tr').count()
await page.getByRole('button', { name: 'Disable', exact: true }).first().click()
await page.waitForTimeout(1200)
txt = await bodyText()
check('tasks: disable flips badge to paused', txt.includes('paused') && (await page.locator('tbody tr').count()) === rowsBefore)
check('tasks: paused row offers Enable', (await page.getByRole('button', { name: 'Enable', exact: true }).count()) >= 1)
await page.getByRole('button', { name: 'Enable', exact: true }).first().click()
await page.waitForTimeout(1200)
txt = await bodyText()
check('tasks: enable shows restart hint + running badge', txt.includes('re-arms on next restart') && !txt.includes(' paused'))
await page.screenshot({ path: SHOT('06-tasks') })

// ==================== 8. CHAT page (history + SSE stream + persistence) ====================
await page.goto(BASE + '/chat', { waitUntil: 'networkidle' })
try {
  await page.waitForURL('**/chat/*', { timeout: 8000 })
  check('chat: /chat redirects to active thread', true, page.url())
} catch {
  check('chat: /chat redirects to active thread', false, page.url())
}
await page.waitForTimeout(900)
txt = await bodyText()
check('chat: history loads seeded messages', txt.includes('The Winds of Winter') && txt.includes('Death of Portia'))
check('chat: thread sidebar shows seeded title', txt.includes('Which Patrick Rothfuss book'))
// send a message → SSE tokens + tool notes + done
await page.locator('.chat-input input').fill('hello e2e stream')
await page.getByRole('button', { name: 'Send', exact: true }).click()
await page.waitForTimeout(2500) // scripted stream ≈ 5 words × 30ms + tool events
txt = await bodyText()
check('chat: streamed assistant reply rendered', txt.includes('Preview reply to: hello e2e stream'))
check('chat: tool event notes rendered', /🛠\s*read_file/.test(txt))
check('chat: user bubble visible', txt.includes('hello e2e stream'))
await page.screenshot({ path: SHOT('07-chat-streamed') })
// persistence: reload → server is source of truth (messages live in SQLite)
await page.reload({ waitUntil: 'networkidle' })
await page.waitForTimeout(900)
txt = await bodyText()
check('chat: reload reconciles from DB (no ghost bubbles)', txt.includes('Preview reply to: hello e2e stream'))
// sidebar badge refetches on reload (MVP: eventually consistent — not
// invalidated mid-session), should now include the 2 new messages
const badge = (txt.match(/(\d+) messages/) ?? [])[1]
check('chat: thread message count updated after reload', Number(badge) >= 6, `badge=${badge}`)
// deep-link into the thread directly by id
const threadUrl = page.url()
await page.goto(BASE + '/dashboard', { waitUntil: 'networkidle' })
await page.goto(threadUrl, { waitUntil: 'networkidle' })
await page.waitForTimeout(700)
check('chat: thread deep-link works', (await bodyText()).includes('Preview reply to'))
await page.screenshot({ path: SHOT('08-chat-deeplink') })

// ==================== 9. Console errors gate ====================
const realErrs = errs.filter(e => !e.includes('favicon'))
check('no console/page errors across all pages', realErrs.length === 0, realErrs.slice(0, 3).join(' | '))

await ctx.close(); await browser.close()
const failed = results.filter(r => !r.ok)
console.log(`\n${'='.repeat(60)}\nRESULT: ${results.length - failed.length}/${results.length} checks passed`)
if (failed.length) {
  console.log('FAILED:')
  for (const f of failed) console.log('  -', f.name, f.detail ? `— ${f.detail}` : '')
}
process.exit(failed.length ? 1 : 0)
