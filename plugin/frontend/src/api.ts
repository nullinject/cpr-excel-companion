import type { CatalogPage } from './gateway'

export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message)
    this.name = 'ApiError'
  }
}
interface HostBridge {
  version: number
  request: (request: {
    path: string
    method: string
    contentType?: string
    body?: string
  }) => Promise<{ status: number, body: ArrayBuffer }>
}
export async function api<T>(path: string, data?: unknown): Promise<T> {
  const host = (window as unknown as { codexProxyPlugin?: HostBridge })
    .codexProxyPlugin
  if (!host || host.version !== 2)
    throw new ApiError('请从 CPR 侧栏打开 Excel 网关。', 0)
  const response = await host.request({
    path,
    method: data === undefined ? 'GET' : 'POST',
    ...(data === undefined
      ? {}
      : { contentType: 'application/json', body: JSON.stringify(data) }),
  })
  let value: unknown
  try {
    value = JSON.parse(new TextDecoder().decode(response.body))
  }
  catch {
    throw new ApiError(
      `服务返回了无法识别的响应（HTTP ${response.status}）。`,
      response.status,
    )
  }
  if (response.status >= 400) {
    const message
      = typeof value === 'object'
        && value
        && 'error' in value
        && typeof value.error === 'string'
        ? value.error
        : `请求未完成（HTTP ${response.status}）。`
    throw new ApiError(message, response.status)
  }
  return value as T
}
export async function catalog<T>(path: string): Promise<T[]> {
  const items: T[] = []
  const visited = new Set<string>()
  let cursor: string | null = null
  do {
    const page: CatalogPage<T> = await api(path, { cursor })
    if (
      !Array.isArray(page.items)
      || (page.next_cursor !== null && typeof page.next_cursor !== 'string')
    ) {
      throw new ApiError('目录返回格式不正确。', 502)
    }
    items.push(...page.items)
    cursor = page.next_cursor
    if (cursor !== null) {
      if (visited.has(cursor))
        throw new ApiError('目录分页重复，请稍后重新读取。', 502)
      visited.add(cursor)
    }
  } while (cursor !== null)
  return items
}
