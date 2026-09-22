// End-to-end white-screen test against the REAL embedded portal binary.
// Serves web/dist compiled into the Rust binary via include_dir! —
// exactly what ships in production.
import pw from '/home/kan/.nvm/versions/node/v24.13.0/lib/node_modules/@playwright/test/index.js'
const { chromium } = pw

const BASE = process.env.BASE || 'http://127.0.0.1:8123'
const TOKEN = process.env.TOKEN || 'preview-token'
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

// ---- 1. Login page renders (not white screen) ----
await page.goto(BASE + '/', { waitUntil: 'networkidle' })
await page.waitForTimeout(600)
const title = await page.title()
check('page title', title.includes('RustFox'), `title="${title}"`)
const bodyLen = (await page.evaluate(() => document.body.innerText.trim().length))
check('SPA rendered (body text > 20 chars)', bodyLen > 20, `bodyLen=${bodyLen}`)
const loginCard = await page.locator('.login-card, form').count()
check('login card visible', loginCard > 0, `count=${loginCard}`)
await page.screenshot({ path: '/tmp/pw-01-login.png' })

// ---- 2. Sign in with token ----
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
const dashText = await page.evaluate(() => document.body.innerText)
check('dashboard has content', dashText.length > 50, `len=${dashText.length}`)
await page.screenshot({ path: '/tmp/pw-02-dashboard.png' })

// ---- 3. Client-side route deep-link + refresh (SPA fallback) ----
await page.goto(BASE + '/settings', { waitUntil: 'networkidle' })
await page.waitForTimeout(700)
const setOk = page.url().includes('/settings') && (await page.evaluate(() => document.body.innerText.length)) > 50
check('deep-link /settings works (SPA fallback embedded)', setOk, `url=${page.url()}`)
await page.reload({ waitUntil: 'networkidle' })
await page.waitForTimeout(500)
check('refresh on /settings keeps app alive', (await page.evaluate(() => document.body.innerText.length)) > 50)
await page.screenshot({ path: '/tmp/pw-03-settings.png' })

// ---- 4. JS/CSS assets served from embedded dist ----
const jsReq = await page.evaluate(() => performance.getEntriesByType('resource').filter(r => r.name.includes('/assets/')).map(r => ({ u: r.name, s: r.responseStatus })))
const allAssetsOk = jsReq.length >= 2 && jsReq.every(r => r.s === 200)
check('embedded /assets/* served with 200', allAssetsOk, JSON.stringify(jsReq.map(r => `${r.s}:${r.u.split('/').pop()}`)))

// ---- 5. API through same binary ----
const me = await page.evaluate(async () => (await fetch('/api/auth/me')).json())
check('GET /api/auth/me authenticated', JSON.stringify(me).includes('web'), JSON.stringify(me).slice(0, 120))

// ---- 6. Console errors gate ----
const realErrs = errs.filter(e => !e.includes('favicon'))
check('no console/page errors', realErrs.length === 0, realErrs.slice(0, 3).join(' | '))

await ctx.close(); await browser.close()
const failed = results.filter(r => !r.ok)
console.log(`\n${'='.repeat(50)}\nRESULT: ${results.length - failed.length}/${results.length} checks passed`)
process.exit(failed.length ? 1 : 0)
