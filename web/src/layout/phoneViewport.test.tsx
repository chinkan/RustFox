import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { RouterProvider, createMemoryHistory, createRouter } from '@tanstack/react-router'
import { routeTree } from '../routeTree.gen'
import { __resetAuthForTests, authStore, bindAuthApi, type RouterContext } from '../auth'
import { BootWatcher } from '../hooks/useBootWatcher'
import { ChatSession } from '../hooks/useChatStream'
import { fakeApi } from '../api/fake'
import { __resetOfficePrefsForTests } from '../agents/officePrefs'

/**
 * Phone layout contract (#401).
 *
 * A typical phone is 375–430 CSS px wide. The desktop shell (232px + main)
 * and chat layout (250px + pane) must not stay side by side there: navigation,
 * Chat, and Settings have to be operable without a horizontal lock.
 */

const webRoot = join(dirname(fileURLToPath(import.meta.url)), '../..')
const css = readFileSync(join(webRoot, 'src/styles.css'), 'utf8')
const html = readFileSync(join(webRoot, 'index.html'), 'utf8')

interface MediaBlock {
  query: string
  body: string
}

function mediaBlocks(source: string): MediaBlock[] {
  const blocks: MediaBlock[] = []
  const re = /@media\s*([^{]+)\{/g
  let match: RegExpExecArray | null
  while ((match = re.exec(source))) {
    const start = match.index + match[0].length
    let depth = 1
    let i = start
    while (i < source.length && depth > 0) {
      if (source[i] === '{') depth += 1
      else if (source[i] === '}') depth -= 1
      i += 1
    }
    blocks.push({ query: match[1].trim().replace(/\s+/g, ' '), body: source.slice(start, i - 1) })
    re.lastIndex = i
  }
  return blocks
}

function withoutMedia(source: string): string {
  let out = ''
  let i = 0
  const re = /@media\s*[^{]+\{/g
  let match: RegExpExecArray | null
  while ((match = re.exec(source))) {
    out += source.slice(i, match.index)
    const start = match.index + match[0].length
    let depth = 1
    let j = start
    while (j < source.length && depth > 0) {
      if (source[j] === '{') depth += 1
      else if (source[j] === '}') depth -= 1
      j += 1
    }
    i = j
    re.lastIndex = j
  }
  out += source.slice(i)
  return out
}

function phoneBlock(): MediaBlock | undefined {
  return mediaBlocks(css).find((block) => block.query === '(max-width: 767px)')
}

const originalMatchMedia = window.matchMedia

function installViewport(width: number) {
  Object.defineProperty(window, 'innerWidth', { configurable: true, writable: true, value: width })
  window.matchMedia = ((query: string) => {
    const max = /max-width:\s*(\d+)px/.exec(query)
    const matches = max ? width <= Number(max[1]) : false
    return {
      matches,
      media: query,
      onchange: null,
      addEventListener: () => {},
      removeEventListener: () => {},
      dispatchEvent: () => false,
      addListener: () => {},
      removeListener: () => {},
    } as MediaQueryList
  }) as typeof window.matchMedia
}

function makeContext(): RouterContext {
  const ctxApi = fakeApi()
  const qc = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  return {
    auth: authStore,
    api: ctxApi,
    queryClient: qc,
    boot: new BootWatcher(ctxApi, () => undefined),
    chat: new ChatSession(ctxApi),
  }
}

function renderAt(path: string, ctx: RouterContext) {
  const router = createRouter({
    routeTree,
    context: ctx,
    history: createMemoryHistory({ initialEntries: [path] }),
    defaultPreload: false,
  })
  render(
    <QueryClientProvider client={ctx.queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  )
  return router
}

beforeEach(() => {
  bindAuthApi(fakeApi())
  __resetAuthForTests()
  localStorage.clear()
  sessionStorage.clear()
  __resetOfficePrefsForTests()
  installViewport(1440)
})

afterEach(() => {
  cleanup()
  if (originalMatchMedia) window.matchMedia = originalMatchMedia
  else Reflect.deleteProperty(window, 'matchMedia')
})

describe('phone layout stylesheet (#401)', () => {
  it('keeps the existing viewport meta', () => {
    expect(html).toContain('<meta name="viewport" content="width=device-width, initial-scale=1.0" />')
  })

  it('keeps the desktop two-column shell outside the phone breakpoint', () => {
    const desktop = withoutMedia(css)
    expect(desktop).toMatch(/\.shell\s*\{[^}]*grid-template-columns:\s*232px\s+1fr/)
    expect(desktop).toMatch(/\.chat-layout\s*\{[^}]*grid-template-columns:\s*250px\s+1fr/)
  })

  it('collapses the shell and chat layout to one column through 430px', () => {
    const phone = phoneBlock()
    expect(phone, 'missing @media (max-width: 767px)').toBeTruthy()
    const max = Number(/max-width:\s*(\d+)px/.exec(phone!.query)?.[1])
    expect(max).toBeGreaterThanOrEqual(430)
    expect(375).toBeLessThanOrEqual(max)
    expect(1440).toBeGreaterThan(max)
    expect(phone!.body).toMatch(/\.shell\s*\{[^}]*grid-template-columns:\s*minmax\(0,\s*1fr\)/)
    expect(phone!.body).toMatch(/\.chat-layout\s*\{[^}]*grid-template-columns:\s*minmax\(0,\s*1fr\)/)
    expect(phone!.body).not.toMatch(/232px\s+1fr/)
    expect(phone!.body).not.toMatch(/250px\s+1fr/)
    expect(phone!.body).toMatch(/\.sidebar\s*\{[^}]*display:\s*none/)
    expect(phone!.body).toMatch(/\.sidebar\.open\s*\{[^}]*display:\s*flex/)
  })
})

describe('phone viewport fixture (#401)', () => {
  it('keeps desktop navigation inline at 1440px', async () => {
    installViewport(1440)
    await authStore.loginWithToken('test-token')
    renderAt('/dashboard', makeContext())
    expect(await screen.findByRole('heading', { name: 'Dashboard' })).toBeTruthy()
    expect(screen.queryByRole('button', { name: 'Open menu' })).toBeNull()
    expect(screen.getByRole('link', { name: /Chat/ })).toBeTruthy()
    expect(screen.getByRole('link', { name: /Settings/ })).toBeTruthy()
  })

  it('makes nav, Chat, and Settings operable at 390px', async () => {
    installViewport(390)
    await authStore.loginWithToken('test-token')
    const router = renderAt('/chat/conv-1', makeContext())
    await waitFor(() => {
      expect(router.state.location.pathname).toBe('/chat/conv-1')
    })

    expect(await screen.findByRole('heading', { name: 'Chat' })).toBeTruthy()
    expect(screen.queryByRole('link', { name: /Settings/ })).toBeNull()
    expect(screen.queryByRole('button', { name: 'Sign out' })).toBeNull()

    const composer = screen.getByPlaceholderText('Message RustFox…')
    fireEvent.change(composer, { target: { value: 'hi from a phone' } })
    expect((screen.getByRole('button', { name: 'Send' }) as HTMLButtonElement).disabled).toBe(false)
    expect((screen.getByRole('button', { name: 'New conversation' }) as HTMLButtonElement).disabled).toBe(false)

    fireEvent.click(screen.getByRole('button', { name: 'Open menu' }))
    expect(screen.getByRole('button', { expanded: true }).textContent).toBe('Close menu')
    expect(screen.getByRole('link', { name: /Chat/ })).toBeTruthy()
    expect(screen.getByRole('button', { name: 'Sign out' })).toBeTruthy()

    fireEvent.click(screen.getByRole('link', { name: /Settings/ }))
    expect(await screen.findByRole('heading', { name: 'Settings' })).toBeTruthy()
    expect(screen.queryByRole('link', { name: /Settings/ })).toBeNull()

    const model = (await screen.findByText('Model')).parentElement?.querySelector('input')
    expect(model).toBeTruthy()
    fireEvent.change(model!, { target: { value: 'phone-model' } })
    const save = screen.getAllByRole('button', { name: 'Save' }).find((button) => !button.hasAttribute('disabled'))
    expect(save).toBeTruthy()
  })

  it.each([375, 430])('uses the phone shell at %spx', async (width) => {
    installViewport(width)
    await authStore.loginWithToken('test-token')
    renderAt('/chat/conv-1', makeContext())
    expect(await screen.findByRole('button', { name: 'Open menu' })).toBeTruthy()
    expect(screen.getByRole('button', { name: 'New conversation' })).toBeTruthy()
    expect(screen.getByPlaceholderText('Message RustFox…')).toBeTruthy()
    expect(document.querySelector('.shell')?.getAttribute('data-layout')).toBe('phone')
    expect(document.querySelector('.chat-layout')?.getAttribute('data-layout')).toBe('phone')
  })
})
