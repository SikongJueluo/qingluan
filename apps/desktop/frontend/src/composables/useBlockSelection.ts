import { computed, ref } from 'vue'

const selected = ref<Set<string>>(new Set())
const anchorBlockId = ref<string | null>(null)

export function useBlockSelection() {
  function selectBlock(
    blockId: string,
    options?: {
      range?: boolean
      additive?: boolean
    },
  ) {
    if (!options?.additive && !options?.range) {
      selected.value = new Set([blockId])
      anchorBlockId.value = blockId
      return
    }

    if (options?.additive) {
      const next = new Set(selected.value)
      if (next.has(blockId)) next.delete(blockId)
      else next.add(blockId)
      selected.value = next
      anchorBlockId.value = blockId
      return
    }

    selected.value = new Set([anchorBlockId.value ?? blockId, blockId])
  }

  function clearSelection() {
    selected.value = new Set()
    anchorBlockId.value = null
  }

  return {
    selectedBlockIds: computed(() => selected.value),
    anchorBlockId: computed(() => anchorBlockId.value),
    selectBlock,
    clearSelection,
  }
}
