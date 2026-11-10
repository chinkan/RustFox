import type { McpRegistryListEntry, McpRegistrySearchResponse } from './registryTypes'

/** Fixture mirrors live Official Registry list item shape (contract doc for tests). */
export const REGISTRY_FIXTURE_HTTP: McpRegistryListEntry = {
  server: {
    name: 'io.github.user/demo-mcp',
    description: 'Demo streamable HTTP MCP',
    version: '1.0.0',
    remotes: [
      {
        type: 'streamable-http',
        url: 'https://example.com/mcp',
        headers: [
          {
            name: 'Authorization',
            value: 'Bearer {api_key}',
            isSecret: true,
            isRequired: true,
          },
          {
            name: 'X-Api-Key',
            value: '{key}',
            isSecret: true,
          },
        ],
      },
    ],
  },
  _meta: {
    'io.modelcontextprotocol.registry/official': { status: 'active', isLatest: true },
  },
}

export const REGISTRY_FIXTURE_SSE_ONLY: McpRegistryListEntry = {
  server: {
    name: 'io.github.Evozim/linear-broker',
    description: 'SSE only broker',
    remotes: [{ type: 'sse', url: 'https://linear-broker.example/api/mcp' }],
  },
}

export const REGISTRY_FIXTURE_STDIO: McpRegistryListEntry = {
  server: {
    name: 'agency.extremely/agency',
    description: 'Package-only stdio',
    packages: [
      {
        registryType: 'npm',
        identifier: '@agency/mcp',
        transport: { type: 'stdio' },
      },
    ],
  },
}

export const REGISTRY_FIXTURE_MIXED: McpRegistryListEntry = {
  server: {
    name: 'app.linear/linear',
    remotes: [
      { type: 'sse', url: 'https://linear.example/sse' },
      { type: 'streamable-http', url: 'https://linear.example/mcp' },
    ],
  },
}

export const REGISTRY_SEARCH_FIXTURE: McpRegistrySearchResponse = {
  servers: [
    REGISTRY_FIXTURE_HTTP,
    REGISTRY_FIXTURE_SSE_ONLY,
    REGISTRY_FIXTURE_STDIO,
    REGISTRY_FIXTURE_MIXED,
  ],
  metadata: { nextCursor: 'cursor-2', count: 4 },
}
