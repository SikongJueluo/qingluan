export type TaskStatus = 'draft' | 'todo' | 'in-progress' | 'review' | 'archive'

export interface KanbanTask {
  id: string
  title: string
  participants: string[]
  status: TaskStatus
  order: number
}

export interface KanbanColumnDef {
  id: TaskStatus
  title: string
}

export const KANBAN_COLUMNS: KanbanColumnDef[] = [
  { id: 'draft', title: 'Draft' },
  { id: 'todo', title: 'Todo' },
  { id: 'in-progress', title: 'In Progress' },
  { id: 'review', title: 'Review' },
  { id: 'archive', title: 'Archive' },
]
