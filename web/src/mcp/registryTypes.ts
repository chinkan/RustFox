/**
 * Official MCP Registry JSON shapes (portal proxy contract).
 *
 * Upstream: `GET https://registry.modelcontextprotocol.io/v0.1/servers`
 * Portal: `GET /api/mcp/registry/search` and `GET /api/mcp/registry/servers/{name}`.
 */

export interface McpRegistryRemoteHeader {
  name: string
  value?: string
  description?: string
  isRequired?: boolean
  isSecret?: boolean
}

export interface McpRegistryRemote {
  type: string
  url: string
  headers?: McpRegistryRemoteHeader[]
}

export interface McpRegistryPackage {
  registryType?: string
  identifier?: string
  version?: string
  transport?: { type?: string }
}

export interface McpRegistryOfficialMeta {
  status?: string
  isLatest?: boolean
}

export interface McpRegistryServer {
  name: string
  description?: string
  title?: string
  version?: string
  remotes?: McpRegistryRemote[]
  packages?: McpRegistryPackage[]
}

export interface McpRegistryListEntry {
  server: McpRegistryServer
  _meta?: {
    'io.modelcontextprotocol.registry/official'?: McpRegistryOfficialMeta
  }
}

/** `GET /api/mcp/registry/search` */
export interface McpRegistrySearchResponse {
  servers: McpRegistryListEntry[]
  metadata: {
    nextCursor?: string | null
    count?: number
  }
}

/** `GET /api/mcp/registry/servers/{urlencodedName}` */
export type McpRegistryServerDetailResponse = McpRegistryListEntry
