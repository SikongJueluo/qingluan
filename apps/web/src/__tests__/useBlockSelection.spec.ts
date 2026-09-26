import { beforeEach, describe, expect, it } from 'vitest'
import { useBlockSelection } from '@/composables/useBlockSelection'

describe('useBlockSelection', () => {
  beforeEach(() => {
    useBlockSelection().clearSelection()
  })

  it('selects a single block and stores it as the anchor', () => {
    const selection = useBlockSelection()

    selection.selectBlock('block-a')

    expect([...selection.selectedBlockIds.value]).toEqual(['block-a'])
    expect(selection.anchorBlockId.value).toBe('block-a')
  })

  it('toggles additive selections', () => {
    const selection = useBlockSelection()

    selection.selectBlock('block-a')
    selection.selectBlock('block-b', { additive: true })
    expect(selection.selectedBlockIds.value.has('block-a')).toBe(true)
    expect(selection.selectedBlockIds.value.has('block-b')).toBe(true)

    selection.selectBlock('block-b', { additive: true })
    expect([...selection.selectedBlockIds.value]).toEqual(['block-a'])
  })

  it('keeps anchor and current block for range selections', () => {
    const selection = useBlockSelection()

    selection.selectBlock('block-a')
    selection.selectBlock('block-c', { range: true })

    expect(selection.selectedBlockIds.value.has('block-a')).toBe(true)
    expect(selection.selectedBlockIds.value.has('block-c')).toBe(true)
    expect(selection.anchorBlockId.value).toBe('block-a')
  })

  it('clears selection and anchor', () => {
    const selection = useBlockSelection()

    selection.selectBlock('block-a')
    selection.clearSelection()

    expect([...selection.selectedBlockIds.value]).toEqual([])
    expect(selection.anchorBlockId.value).toBeNull()
  })
})
