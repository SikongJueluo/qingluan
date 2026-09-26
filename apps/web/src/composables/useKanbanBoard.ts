import { computed, ref } from 'vue'
import type { KanbanTask, TaskStatus } from '@/kanban/types'
import { KANBAN_COLUMNS } from '@/kanban/types'

const tasks = ref<KanbanTask[]>([])

export function useKanbanBoard() {
  const columns = computed(() =>
    KANBAN_COLUMNS.map((col) => ({
      ...col,
      tasks: tasks.value.filter((t) => t.status === col.id).sort((a, b) => a.order - b.order),
    })),
  )

  function moveTask(taskId: string, targetStatus: TaskStatus, insertIndex: number): void {
    const taskIndex = tasks.value.findIndex((t) => t.id === taskId)
    if (taskIndex === -1) return
    const task = tasks.value[taskIndex]
    if (!task) return

    const targetTasks = tasks.value
      .filter((t) => t.status === targetStatus && t.id !== taskId)
      .sort((a, b) => a.order - b.order)

    task.status = targetStatus
    targetTasks.splice(insertIndex, 0, task)
    targetTasks.forEach((t, i) => {
      t.order = i
    })
  }

  function initBoard(data: KanbanTask[]): void {
    tasks.value = data.map((t) => ({ ...t }))
  }

  function reset(): void {
    tasks.value = []
  }

  return {
    columns,
    tasks: computed(() => tasks.value),
    moveTask,
    initBoard,
    reset,
  }
}
