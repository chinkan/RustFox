import { chromium } from 'playwright'
const b = await chromium.launch({ channel: 'chrome' })
const p = await b.newPage({ viewport: { width: 1440, height: 900 } })
const errs = []
p.on('console', m => { if (m.type()==='error') errs.push(m.text()) })
p.on('pageerror', e => errs.push('PAGEERROR: '+e.message))

await p.goto('http://127.0.0.1:5199/', { waitUntil: 'networkidle' })
console.log('after / -> url:', p.url())
await p.screenshot({ path: '/tmp/01-login.png' })

// sign in as admin
await p.click('button[type=submit]')
await p.waitForTimeout(1200)
console.log('after login -> url:', p.url())
await p.screenshot({ path: '/tmp/02-dashboard.png' })

await p.goto('http://127.0.0.1:5199/agents?q=exp&status=running', { waitUntil:'networkidle' })
await p.waitForTimeout(800)
const rows = await p.locator('tbody tr').count()
console.log('agents rows with status=running & q=exp:', rows)
await p.screenshot({ path: '/tmp/03-agents-filtered.png' })

await p.goto('http://127.0.0.1:5199/chat/t2', { waitUntil:'networkidle' })
await p.waitForTimeout(800)
const hasSidebar = await p.locator('.thread-list').count()
const hasMsg = await p.locator('.msg').count()
console.log('chat: sidebar=', hasSidebar, 'messages=', hasMsg)
await p.screenshot({ path: '/tmp/04-chat.png' })

await p.goto('http://127.0.0.1:5199/memory?q=HKT', { waitUntil:'networkidle' })
await p.waitForTimeout(800)
console.log('memory cards:', await p.locator('.card').count())
await p.screenshot({ path: '/tmp/05-memory.png' })

await p.goto('http://127.0.0.1:5199/settings', { waitUntil:'networkidle' })
await p.waitForTimeout(800)
console.log('settings (admin) -> url:', p.url())
await p.screenshot({ path: '/tmp/06-settings.png' })

// switch to user role, try settings
await p.evaluate(() => { localStorage.setItem('rustfox.portal.session', JSON.stringify({username:'guest',role:'user'})) })
await p.goto('http://127.0.0.1:5199/settings', { waitUntil:'networkidle' })
await p.waitForTimeout(1000)
console.log('settings (user) -> redirected to:', p.url())
await p.screenshot({ path: '/tmp/07-rbac-blocked.png' })

console.log('CONSOLE ERRORS:', errs.length ? errs : 'none')
await b.close()
