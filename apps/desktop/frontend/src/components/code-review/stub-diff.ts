import type { ChangedFile } from './types'

// TODO(code-review): replace this stub with a real diff source once
// qingluan-protocol exposes a changed-files/diff endpoint on the daemon.
export const stubChangedFiles: ChangedFile[] = [
  {
    path: 'src/stores/session.ts',
    status: 'modified',
    additions: 18,
    deletions: 6,
    oldText: `import { defineStore } from 'pinia'
import { ref } from 'vue'

export interface Session {
  id: string
  title: string
  createdAt: number
}

export const useSessionStore = defineStore('session', () => {
  const sessions = ref<Session[]>([])
  const currentId = ref<string | null>(null)

  function create(title: string) {
    const session: Session = {
      id: crypto.randomUUID(),
      title,
      createdAt: Date.now(),
    }
    sessions.value.push(session)
    return session
  }

  function remove(id: string) {
    sessions.value = sessions.value.filter((s) => s.id !== id)
  }

  return { sessions, currentId, create, remove }
})
`,
    newText: `import { defineStore } from 'pinia'
import { computed, ref } from 'vue'

export interface Session {
  id: string
  title: string
  createdAt: number
  updatedAt: number
}

export const useSessionStore = defineStore('session', () => {
  const sessions = ref<Session[]>([])
  const currentId = ref<string | null>(null)

  const current = computed(
    () => sessions.value.find((s) => s.id === currentId.value) ?? null,
  )

  function create(title: string) {
    const now = Date.now()
    const session: Session = {
      id: crypto.randomUUID(),
      title,
      createdAt: now,
      updatedAt: now,
    }
    sessions.value.push(session)
    currentId.value = session.id
    return session
  }

  function rename(id: string, title: string) {
    const session = sessions.value.find((s) => s.id === id)
    if (!session) return
    session.title = title
    session.updatedAt = Date.now()
  }

  function remove(id: string) {
    sessions.value = sessions.value.filter((s) => s.id !== id)
    if (currentId.value === id) currentId.value = null
  }

  return { sessions, currentId, current, create, rename, remove }
})
`,
  },
  {
    path: 'src/lib/daemon-client.ts',
    status: 'modified',
    additions: 14,
    deletions: 3,
    oldText: `const DEFAULT_URL = 'http://127.0.0.1:47129'

export class DaemonClient {
  constructor(private baseUrl: string = DEFAULT_URL) {}

  async health(): Promise<boolean> {
    const res = await fetch(\`\${this.baseUrl}/health\`)
    return res.ok
  }
}
`,
    newText: `const DEFAULT_URL = 'http://127.0.0.1:47129'
const REQUEST_TIMEOUT_MS = 5_000

export class DaemonClient {
  constructor(private baseUrl: string = DEFAULT_URL) {}

  private async request<T>(path: string, init?: RequestInit): Promise<T> {
    const res = await fetch(\`\${this.baseUrl}\${path}\`, {
      ...init,
      signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
    })
    if (!res.ok) {
      throw new Error(\`daemon request failed: \${res.status} \${path}\`)
    }
    return (await res.json()) as T
  }

  health(): Promise<{ ok: boolean }> {
    return this.request('/health')
  }
}
`,
  },
  {
    path: 'src/components/TaskCard.vue',
    status: 'added',
    additions: 22,
    deletions: 0,
    oldText: ``,
    newText: `<template>
  <Card class="p-4">
    <div class="flex items-center justify-between">
      <h3 class="font-medium">{{ task.title }}</h3>
      <Badge :variant="badgeVariant">{{ task.status }}</Badge>
    </div>
    <p class="mt-2 text-sm text-muted-foreground">{{ task.summary }}</p>
  </Card>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { Badge } from '@/components/ui/badge'
import { Card } from '@/components/ui/card'
import type { Task } from '@/kanban/types'

const props = defineProps<{ task: Task }>()

const badgeVariant = computed(() =>
  props.task.status === 'done' ? 'secondary' : 'default',
)
</script>
`,
  },
]
