import { ref } from 'vue'
import type { TaskStatus } from '@/kanban/types'
import { useKanbanBoard } from '@/composables/useKanbanBoard'

const draggingTaskId = ref<string | null>(null)
const dragOverColumn = ref<TaskStatus | null>(null)

export function useKanbanDragDrop() {
  const { moveTask } = useKanbanBoard()

  function onDragStart(event: DragEvent, taskId: string): void {
    if (!event.dataTransfer) return
    event.dataTransfer.effectAllowed = 'move'
    event.dataTransfer.setData('text/plain', taskId)
    draggingTaskId.value = taskId
  }

  function onDragEnter(event: DragEvent, columnStatus: TaskStatus): void {
    event.preventDefault()
    dragOverColumn.value = columnStatus
  }

  function onDragOver(event: DragEvent): void {
    event.preventDefault()
    if (event.dataTransfer) {
      event.dataTransfer.dropEffect = 'move'
    }
  }

  function onDragLeave(event: DragEvent): void {
    const currentTarget = event.currentTarget as HTMLElement | null
    const relatedTarget = event.relatedTarget as Node | null
    // Only clear if we truly left the column (not entering a child element)
    if (currentTarget && relatedTarget && currentTarget.contains(relatedTarget)) {
      return
    }
    dragOverColumn.value = null
  }

  function onDrop(event: DragEvent, columnStatus: TaskStatus, container: HTMLElement): void {
    event.preventDefault()
    const taskId = event.dataTransfer?.getData('text/plain')
    if (!taskId) return

    const insertIndex = getInsertIndex(event, container)
    moveTask(taskId, columnStatus, insertIndex)

    draggingTaskId.value = null
    dragOverColumn.value = null
  }

  function onDragEnd(): void {
    draggingTaskId.value = null
    dragOverColumn.value = null
  }

  return {
    draggingTaskId,
    dragOverColumn,
    onDragStart,
    onDragEnter,
    onDragOver,
    onDragLeave,
    onDrop,
    onDragEnd,
  }
}

function getInsertIndex(event: DragEvent, container: HTMLElement): number {
  const cards = Array.from(container.querySelectorAll<HTMLElement>('[data-card-id]'))
  for (let i = 0; i < cards.length; i++) {
    const card = cards[i]
    if (!card) continue
    const rect = card.getBoundingClientRect()
    const midY = rect.top + rect.height / 2
    if (event.clientY < midY) {
      return i
    }
  }
  return cards.length
}
