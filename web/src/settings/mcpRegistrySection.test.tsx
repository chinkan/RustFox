import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { type ReactElement } from 'react'
import { describe, expect, it, vi } from 'vitest'
import { I18nextProvider } from 'react-i18next'
import i18n from '../i18n'
import { REGISTRY_SEARCH_FIXTURE } from '../mcp/registryFixtures'
import type { Api } from '../api/client'
import { McpRegistrySection } from './mcpRegistrySection'

function wrap(ui: ReactElement) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  return render(
    <QueryClientProvider client={client}>
      <I18nextProvider i18n={i18n}>{ui}</I18nextProvider>
    </QueryClientProvider>,
  )
}

function apiOver(over: Partial<Api>): Api {
  return {
    searchMcpRegistry: vi.fn(),
    putMcpServer: vi.fn().mockResolvedValue({ server: {} }),
    ...over,
  } as unknown as Api
}

describe('McpRegistrySection', () => {
  it('blocks search under 3 characters', async () => {
    const searchMcpRegistry = vi.fn()
    wrap(
      <McpRegistrySection
        api={apiOver({ searchMcpRegistry })}
        existingServerNames={[]}
      />,
    )
    fireEvent.change(screen.getByPlaceholderText(/github/i), { target: { value: 'ab' } })
    fireEvent.click(screen.getByRole('button', { name: /^Search$/i }))
    expect(searchMcpRegistry).not.toHaveBeenCalled()
    expect(await screen.findByText(/at least 3 characters/i)).toBeTruthy()
  })

  it('soft-fails when registry proxy errors', async () => {
    const searchMcpRegistry = vi.fn().mockRejectedValue(new Error('502 upstream'))
    wrap(
      <McpRegistrySection
        api={apiOver({ searchMcpRegistry })}
        existingServerNames={[]}
      />,
    )
    fireEvent.click(screen.getByRole('button', { name: /^Search$/i }))
    await waitFor(() => expect(searchMcpRegistry).toHaveBeenCalled())
    expect(await screen.findByText(/502 upstream/)).toBeTruthy()
  })

  it('installs selected via sequential PUT', async () => {
    const searchMcpRegistry = vi.fn().mockResolvedValue({
      servers: [REGISTRY_SEARCH_FIXTURE.servers[0]],
      metadata: {},
    })
    const putMcpServer = vi.fn().mockResolvedValue({ server: { name: 'demo-mcp' } })
    wrap(
      <McpRegistrySection
        api={apiOver({ searchMcpRegistry, putMcpServer })}
        existingServerNames={[]}
      />,
    )
    fireEvent.change(screen.getByPlaceholderText(/github/i), { target: { value: 'demo' } })
    fireEvent.click(screen.getByRole('button', { name: /^Search$/i }))
    const checkbox = await screen.findByRole('checkbox', { name: /demo-mcp/i })
    fireEvent.click(checkbox)
    fireEvent.click(screen.getByRole('button', { name: /Install selected/i }))
    await waitFor(() => expect(putMcpServer).toHaveBeenCalledTimes(1))
    expect(putMcpServer.mock.calls[0][0]).toBe('demo-mcp')
    expect(putMcpServer.mock.calls[0][1]).toMatchObject({
      url: 'https://example.com/mcp',
      enabled: true,
    })
  })
})
