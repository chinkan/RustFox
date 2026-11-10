import type { McpUpsertBody } from '../api/types'
import type { McpRegistryRemote, McpRegistryServer } from './registryTypes'

export const RESERVED_MCP_SLUGS = new Set(['exa', 'jina', 'google'])

export const REGISTRY_SEARCH_MIN_CHARS = 3

export type RegistryInstallKind = 'installable' | 'stdio' | 'sse-only' | 'unavailable'

export function registrySearchQueryValid(q: string): boolean {
  const t = q.trim()
  return t.length === 0 || t.length >= REGISTRY_SEARCH_MIN_CHARS
}

export function shortSlugHash(input: string): string {
  let h = 2166136261
  for (let i = 0; i < input.length; i++) {
    h ^= input.charCodeAt(i)
    h = Math.imul(h, 16777619)
  }
  return (h >>> 0).toString(36).slice(0, 6)
}

/** Last path segment of registry `server.name`, lowercased, invalid chars → `-`. */
export function baseSlugFromRegistryName(registryName: string): string {
  const segment = registryName.includes('/')
    ? registryName.slice(registryName.lastIndexOf('/') + 1)
    : registryName
  let slug = segment
    .toLowerCase()
    .replace(/[^a-z0-9._-]+/g, '-')
    .replace(/-+/g, '-')
    .replace(/^[-.]+|[-.]+$/g, '')
  if (!slug) slug = 'mcp-server'
  if (!/^[a-z0-9]/.test(slug)) slug = `srv-${slug}`
  return slug
}

export function uniquifyMcpSlug(base: string, takenNames: Set<string>): string {
  let candidate = RESERVED_MCP_SLUGS.has(base) ? `${base}-2` : base
  if (!takenNames.has(candidate)) return candidate
  const root = RESERVED_MCP_SLUGS.has(base) ? `${base}-2` : base
  for (let n = 2; n < 50; n++) {
    candidate = `${root}-${n}`
    if (!takenNames.has(candidate)) return candidate
  }
  candidate = `${root}-${shortSlugHash(base)}`
  return takenNames.has(candidate) ? `${candidate}-2` : candidate
}

export function slugFromRegistryName(registryName: string, takenNames: Set<string>): string {
  const base = baseSlugFromRegistryName(registryName)
  return uniquifyMcpSlug(base, takenNames)
}

function isHttpRemote(type: string): boolean {
  const t = type.toLowerCase()
  return t === 'streamable-http' || t === 'http'
}

/** Prefer `streamable-http` when both HTTP and SSE remotes exist. */
export function pickInstallableRemote(server: McpRegistryServer): McpRegistryRemote | null {
  const remotes = server.remotes ?? []
  const http = remotes.find((r) => isHttpRemote(r.type))
  return http ?? null
}

export function classifyRegistryServer(server: McpRegistryServer): RegistryInstallKind {
  const remotes = server.remotes ?? []
  const packages = server.packages ?? []
  if (pickInstallableRemote(server)) return 'installable'
  if (remotes.length > 0) return 'sse-only'
  if (packages.length > 0) return 'stdio'
  return 'unavailable'
}

export function filterRegistryEntriesRemoteOnly<T extends { server: McpRegistryServer }>(
  entries: T[],
): T[] {
  return entries.filter((e) => (e.server.remotes?.length ?? 0) > 0)
}

export function headerValueHasPlaceholder(value: string | undefined): boolean {
  return typeof value === 'string' && /\{[^}]+\}/.test(value)
}

/** Headers the install review must collect (secrets / required templates). */
export function collectRegistrySecretFields(
  remote: McpRegistryRemote,
): { name: string; isBearer: boolean; description?: string }[] {
  const out: { name: string; isBearer: boolean; description?: string }[] = []
  for (const h of remote.headers ?? []) {
    const isBearer = h.name.toLowerCase() === 'authorization'
    const needs =
      h.isSecret ||
      h.isRequired ||
      headerValueHasPlaceholder(h.value) ||
      (isBearer && (h.value?.toLowerCase().includes('bearer') ?? false))
    if (!needs) continue
    if (out.some((x) => x.name.toLowerCase() === h.name.toLowerCase())) continue
    out.push({ name: h.name, isBearer, description: h.description })
  }
  return out
}

export function mapRegistryRemoteToUpsert(
  remote: McpRegistryRemote,
  slug: string,
  secretValues: Record<string, string>,
): McpUpsertBody {
  const body: McpUpsertBody = {
    name: slug,
    url: remote.url,
    enabled: true,
  }
  const headerMap: Record<string, string> = {}
  for (const h of remote.headers ?? []) {
    const key = h.name
    const lower = key.toLowerCase()
    const operator = (secretValues[key] ?? secretValues[lower] ?? '').trim()
    if (lower === 'authorization') {
      if (operator) {
        const bearer = operator.replace(/^Bearer\s+/i, '').trim()
        if (bearer) body.bearer = bearer
      }
      continue
    }
    if (operator) {
      headerMap[key] = operator
      continue
    }
    const staticValue = h.value?.trim() ?? ''
    if (
      h.isRequired &&
      !h.isSecret &&
      staticValue &&
      !headerValueHasPlaceholder(staticValue)
    ) {
      headerMap[key] = staticValue
    }
  }
  if (Object.keys(headerMap).length > 0) body.headers = headerMap
  return body
}

export async function installRegistryServersSequential(
  items: { slug: string; body: McpUpsertBody }[],
  putMcpServer: (name: string, body: McpUpsertBody) => Promise<unknown>,
): Promise<{ slug: string; ok: boolean; error?: string }[]> {
  const results: { slug: string; ok: boolean; error?: string }[] = []
  for (const item of items) {
    try {
      await putMcpServer(item.slug, item.body)
      results.push({ slug: item.slug, ok: true })
    } catch (e) {
      results.push({ slug: item.slug, ok: false, error: (e as Error).message })
    }
  }
  return results
}
