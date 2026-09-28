import { beforeEach, describe, expect, it } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { allDirKeys, buildFileTree } from '@/components/review/file-tree'
import type { ChangedFileMeta } from '@/components/code-review/types'
import { useReviewExplorerStore } from '@/stores/reviewExplorer'

function meta(path: string, additions = 1, deletions = 0): ChangedFileMeta {
  return { path, status: 'modified', additions, deletions, binary: false }
}

describe('buildFileTree', () => {
  it('nests files under directories with folders first', () => {
    const tree = buildFileTree([
      meta('README.md'),
      meta('src/main.rs'),
      meta('src/lib/deep.rs'),
      meta('docs/adr/x.md'),
    ])
    expect(tree.map((n) => n.name)).toEqual(['docs', 'src', 'README.md'])
    const src = tree.find((n) => n.name === 'src')!
    expect(src.dir).toBe(true)
    expect(src.children!.map((n) => n.name)).toEqual(['lib', 'main.rs'])
    expect(src.children!.find((n) => n.name === 'lib')!.children![0]!.name).toBe('deep.rs')
  })

  it('keeps full paths as keys and attaches diff meta to leaves', () => {
    const tree = buildFileTree([meta('src/lib/a.rs', 3, 2)])
    const file = tree[0]!.children![0]!.children![0]!
    expect(file.key).toBe('src/lib/a.rs')
    expect(file.dir).toBe(false)
    expect(file.meta?.additions).toBe(3)
    expect(file.meta?.deletions).toBe(2)
  })

  it('sorts case-insensitively and stays stable for equal names', () => {
    const tree = buildFileTree([meta('Zeta.md'), meta('alpha.md'), meta('Beta.md')])
    expect(tree.map((n) => n.name)).toEqual(['alpha.md', 'Beta.md', 'Zeta.md'])
  })

  it('collects every directory key for default expansion', () => {
    const keys = allDirKeys(buildFileTree([meta('a/b/c/d.rs'), meta('a/e.rs')]))
    expect(keys.sort()).toEqual(['a', 'a/b', 'a/b/c'])
  })

  it('handles an empty file list', () => {
    expect(buildFileTree([])).toEqual([])
  })
})

describe('useReviewExplorerStore', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
  })

  it('selects files by path and falls back to overview with null', () => {
    const store = useReviewExplorerStore()
    store.openSession('s1', [meta('a.rs'), meta('b.rs')])
    store.select('a.rs')
    expect(store.selectedFile).toBe('a.rs')
    store.select(null)
    expect(store.selectedFile).toBeNull()
  })

  it('ignores selections for unknown paths', () => {
    const store = useReviewExplorerStore()
    store.openSession('s1', [meta('a.rs')])
    store.select('a.rs')
    store.select('missing.rs')
    expect(store.selectedFile).toBe('a.rs')
  })

  it('clear resets session, selection and tab', () => {
    const store = useReviewExplorerStore()
    store.openSession('s1', [meta('a.rs')])
    store.select('a.rs')
    store.setTab('files')
    store.clear()
    expect(store.sessionId).toBeNull()
    expect(store.files).toEqual([])
    expect(store.selectedFile).toBeNull()
    expect(store.tab).toBe('workspace')
  })
})
