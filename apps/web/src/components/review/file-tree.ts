/**
 * Review 文件树：把 diff 的扁平文件列表构建成 GitHub 风格的目录树。
 *
 * 纯函数、无依赖，便于单测；渲染见 ReviewFileTree.vue。
 */
import type { ChangedFileMeta } from '@/components/code-review/types'

export interface TreeNode {
  /** 完整路径（作为树的唯一 key 与选中值）。 */
  key: string
  /** 显示名（路径末段）。 */
  name: string
  dir: boolean
  /** 仅文件节点：diff 元信息。 */
  meta?: ChangedFileMeta
  /** 仅目录节点。 */
  children?: TreeNode[]
}

/** 把一个文件的路径挂进树（沿途创建目录节点）。 */
function insertPath(root: TreeNode, meta: ChangedFileMeta): void {
  const segments = meta.path.split('/')
  let node = root
  let prefix = ''
  for (let i = 0; i < segments.length; i++) {
    const segment = segments[i]!
    prefix = prefix ? `${prefix}/${segment}` : segment
    const isLeaf = i === segments.length - 1
    node = descend(node, prefix, segment, isLeaf, meta)
  }
}

function descend(
  node: TreeNode,
  prefix: string,
  segment: string,
  isLeaf: boolean,
  meta: ChangedFileMeta,
): TreeNode {
  if (isLeaf) {
    node.children!.push({ key: prefix, name: segment, dir: false, meta })
    return node
  }
  let dir = node.children!.find((c) => c.dir && c.name === segment)
  if (!dir) {
    dir = { key: prefix, name: segment, dir: true, children: [] }
    node.children!.push(dir)
  }
  return dir
}

/**
 * 从 diff 文件列表构建目录树。
 *
 * 排序对齐 GitHub：目录在前、文件在后，各自按名称排序（大小写不敏感，
 * 稳定排序保持同名列的原相对顺序）。
 */
export function buildFileTree(files: ChangedFileMeta[]): TreeNode[] {
  const root: TreeNode = { key: '', name: '', dir: true, children: [] }
  for (const meta of files) insertPath(root, meta)
  sortLevel(root.children!)
  return root.children!
}

function sortLevel(nodes: TreeNode[]): void {
  nodes.sort((a, b) => {
    if (a.dir !== b.dir) return a.dir ? -1 : 1
    return a.name.localeCompare(b.name, undefined, { sensitivity: 'base' })
  })
  for (const n of nodes) if (n.children) sortLevel(n.children)
}

/** 收集树里全部目录 key（默认全展开用）。 */
export function allDirKeys(nodes: TreeNode[], acc: string[] = []): string[] {
  for (const n of nodes) {
    if (n.dir) {
      acc.push(n.key)
      allDirKeys(n.children ?? [], acc)
    }
  }
  return acc
}
