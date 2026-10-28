/**
 * Evidence pack for Portal 3D spike — Pixel vs 3D on /agents.
 * Run: npm run build && npx vite preview --port 4173 &
 *      node e2e/capture-office-3d-spike.mjs
 */
import { chromium } from 'playwright'
import { mkdirSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = join(dirname(fileURLToPath(import.meta.url)), '../..')
const outDir = join(root, 'artifacts/portal-3d-spike')
mkdirSync(outDir, { recursive: true })

const agents = [
  {
    id: 'main',
    name: 'rustfox',
    model: 'anthropic/claude-opus-4.8',
    status: 'idle',
    lastActive: null,
    platform: 'telegram+web',
    live_signal: true,
    definition_only: false,
  },
  {
    id: 'agent:verifier',
    name: 'verifier',
    model: 'openai/gpt-5.5',
    status: 'running',
    lastActive: null,
    platform: 'subagent',
    live_signal: true,
    definition_only: false,
  },
  {
    id: 'agent:ghost',
    name: 'ghost-seat',
    model: 'local/qwen',
    status: 'running',
    lastActive: null,
    platform: 'subagent',
    live_signal: false,
    definition_only: true,
  },
]

const skills = [
  { name: 'verifier', description: 'Verifier', kind: 'agent' },
  { name: 'hko-weather', description: 'Weather', kind: 'skill' },
]

const base = 'http://127.0.0.1:4173'

const browser = await chromium.launch({ channel: 'chrome' })
const narrow = await browser.newPage({ viewport: { width: 390, height: 844 } })
const wide = await browser.newPage({ viewport: { width: 1280, height: 800 } })

async function stubApi(page) {
  await page.route('**/api/**', async (route) => {
    const url = route.request().url()
    if (url.includes('/api/auth/me')) {
      return route.fulfill({ json: { authenticated: true, username: 'web', role: 'admin' } })
    }
    if (url.includes('/api/auth/login')) {
      return route.fulfill({ json: { username: 'web', role: 'admin' } })
    }
    if (url.endsWith('/api/agents')) {
      return route.fulfill({ json: agents })
    }
    if (url.endsWith('/api/skills')) {
      return route.fulfill({ json: skills })
    }
    if (url.endsWith('/api/health')) {
      return route.fulfill({
        json: {
          cpuPercent: 5,
          memUsedGb: 2,
          memTotalGb: 8,
          diskUsedPercent: 20,
          uptimeHours: 1,
          bootId: 'boot-spike',
        },
      })
    }
    return route.fulfill({ status: 404, body: 'not stubbed' })
  })
}

for (const page of [wide, narrow]) {
  await stubApi(page)
  await page.goto(`${base}/agents`, { waitUntil: 'networkidle' })
}

await wide.screenshot({ path: join(outDir, 'agents-pixel-wide.png'), fullPage: true })
await wide.getByRole('button', { name: '3D' }).click()
await wide.waitForTimeout(1200)
await wide.screenshot({ path: join(outDir, 'agents-3d-wide.png'), fullPage: true })

await narrow.screenshot({ path: join(outDir, 'agents-pixel-narrow.png'), fullPage: true })
await narrow.getByRole('button', { name: '3D' }).click()
await narrow.waitForTimeout(1200)
await narrow.screenshot({ path: join(outDir, 'agents-3d-narrow.png'), fullPage: true })

console.log('Wrote screenshots to', outDir)
await browser.close()
