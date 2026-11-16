/**
 * MCP Registry JSON the portal SPA reads.
 *
 * Upstream: `GET https://registry.modelcontextprotocol.io/v0.1/servers`
 * Portal: `GET /api/mcp/registry/search` and `GET /api/mcp/registry/servers/{name}`
 * add `install` (including `suggestedSlug`) on each record.
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

/**
 * Portal proxy draft added to each registry record (ADR-0041 §11).
 * Official registry JSON omits this object.
 */
export interface McpRegistryInstall {
  /** `[[mcp_servers]].name` to PUT. Same Custom URL reuses that row's name. */
  suggestedSlug?: string
}

export interface McpRegistryListEntry {
  server: McpRegistryServer
  _meta?: {
    'io.modelcontextprotocol.registry/official'?: McpRegistryOfficialMeta
  }
  install?: McpRegistryInstall
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
