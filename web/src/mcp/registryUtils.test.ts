import { describe, expect, it, vi } from 'vitest'
import type { McpRegistryRemote } from './registryTypes'
import {
  REGISTRY_FIXTURE_HTTP,
  REGISTRY_FIXTURE_MIXED,
  REGISTRY_FIXTURE_SSE_ONLY,
  REGISTRY_FIXTURE_STDIO,
} from './registryFixtures'
import {
  baseSlugFromRegistryName,
  classifyRegistryServer,
  collectRegistrySecretFields,
  filterRegistryEntriesRemoteOnly,
  installRegistryServersSequential,
  mapRegistryRemoteToUpsert,
  pickInstallableRemote,
  registrySearchQueryValid,
  slugFromRegistryName,
  uniquifyMcpSlug,
} from './registryUtils'

describe('registrySearchQueryValid', () => {
  it('allows empty and ≥3 chars', () => {
    expect(registrySearchQueryValid('')).toBe(true)
    expect(registrySearchQueryValid('abc')).toBe(true)
    expect(registrySearchQueryValid('  github  ')).toBe(true)
  })
  it('rejects 1–2 chars', () => {
    expect(registrySearchQueryValid('a')).toBe(false)
    expect(registrySearchQueryValid('ab')).toBe(false)
  })
})

describe('slugFromRegistryName', () => {
  it('uses last segment and lowercases', () => {
    expect(baseSlugFromRegistryName('io.github.Evozim/linear-broker')).toBe('linear-broker')
    expect(baseSlugFromRegistryName('ai.smithery/Hint-Services-foo')).toBe('hint-services-foo')
  })
  it('replaces invalid characters', () => {
    expect(baseSlugFromRegistryName('com.example/my_server!')).toBe('my_server')
  })
  it('refuses reserved preset slugs', () => {
    expect(slugFromRegistryName('com.foo/google', new Set())).toBe('google-2')
  })
  it('appends -2 on collision', () => {
    const taken = new Set(['linear-broker'])
    expect(slugFromRegistryName('io.github/foo/linear-broker', taken)).toBe('linear-broker-2')
  })
  it('uniquify handles reserved + taken', () => {
    expect(uniquifyMcpSlug('google', new Set(['google-2']))).toMatch(/^google-/)
  })
})

describe('classifyRegistryServer', () => {
  it('marks streamable-http installable', () => {
    expect(classifyRegistryServer(REGISTRY_FIXTURE_HTTP.server)).toBe('installable')
  })
  it('marks sse-only', () => {
    expect(classifyRegistryServer(REGISTRY_FIXTURE_SSE_ONLY.server)).toBe('sse-only')
  })
  it('marks stdio package-only', () => {
    expect(classifyRegistryServer(REGISTRY_FIXTURE_STDIO.server)).toBe('stdio')
  })
  it('prefers streamable-http when mixed with sse', () => {
    const remote = pickInstallableRemote(REGISTRY_FIXTURE_MIXED.server)
    expect(remote?.type).toBe('streamable-http')
    expect(remote?.url).toBe('https://linear.example/mcp')
    expect(classifyRegistryServer(REGISTRY_FIXTURE_MIXED.server)).toBe('installable')
  })
})

describe('filterRegistryEntriesRemoteOnly', () => {
  it('hides stdio-only rows', () => {
    const all = [
      REGISTRY_FIXTURE_HTTP,
      REGISTRY_FIXTURE_STDIO,
      REGISTRY_FIXTURE_SSE_ONLY,
    ]
    const filtered = filterRegistryEntriesRemoteOnly(all)
    expect(filtered.map((e) => e.server.name)).toEqual([
      REGISTRY_FIXTURE_HTTP.server.name,
      REGISTRY_FIXTURE_SSE_ONLY.server.name,
    ])
  })
})

describe('mapRegistryRemoteToUpsert', () => {
  it('maps Authorization to bearer and other secrets to headers', () => {
    const remote = REGISTRY_FIXTURE_HTTP.server.remotes![0]
    const body = mapRegistryRemoteToUpsert(remote, 'demo-mcp', {
      Authorization: 'Bearer tok_abc',
      'X-Api-Key': 'key_xyz',
    })
    expect(body).toMatchObject({
      name: 'demo-mcp',
      url: 'https://example.com/mcp',
      enabled: true,
      bearer: 'tok_abc',
      headers: { 'X-Api-Key': 'key_xyz' },
    })
  })
  it('collects fields needing operator input', () => {
    const remote = REGISTRY_FIXTURE_HTTP.server.remotes![0]
    const fields = collectRegistrySecretFields(remote)
    expect(fields.map((f) => f.name).sort()).toEqual(['Authorization', 'X-Api-Key'])
  })
  it('includes required non-secret headers with static values', () => {
    const remote: McpRegistryRemote = {
      type: 'streamable-http',
      url: 'https://example.com/mcp',
      headers: [
        {
          name: 'X-Client-Version',
          value: '1.0.0',
          isRequired: true,
          isSecret: false,
        },
        {
          name: 'Optional-Trace',
          value: 'off',
          isRequired: false,
          isSecret: false,
        },
      ],
    }
    const body = mapRegistryRemoteToUpsert(remote, 'demo', {})
    expect(body.headers).toEqual({ 'X-Client-Version': '1.0.0' })
  })
})

describe('installRegistryServersSequential', () => {
  it('continues after partial failure', async () => {
    const put = vi
      .fn()
      .mockResolvedValueOnce({})
      .mockRejectedValueOnce(new Error('boom'))
      .mockResolvedValueOnce({})
    const results = await installRegistryServersSequential(
      [
        { slug: 'a', body: { url: 'https://a' } },
        { slug: 'b', body: { url: 'https://b' } },
        { slug: 'c', body: { url: 'https://c' } },
      ],
      put,
    )
    expect(put).toHaveBeenCalledTimes(3)
    expect(results).toEqual([
      { slug: 'a', ok: true },
      { slug: 'b', ok: false, error: 'boom' },
      { slug: 'c', ok: true },
    ])
  })
})
