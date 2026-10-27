import { defineStore } from 'pinia'
import { computed, ref } from 'vue'

export type CheckoutPhase = 'running' | 'cancelled' | 'failed' | 'completed'

/**
 * The checkout that the app is running or has just given up on.
 *
 * The state lives here rather than inside the checkout page so it survives a
 * page switch: a cancelled checkout can be resumed from the update page, and the
 * background poll knows to stay out of the way while one is running.
 */
export interface CheckoutSession {
  url: string
  path: string
  revision?: number
  phase: CheckoutPhase
  startedAt: number
}

export const useCheckoutStore = defineStore('checkout', () => {
  const session = ref<CheckoutSession | null>(null)

  const begin = (url: string, path: string, revision?: number) => {
    session.value = { url, path, revision, phase: 'running', startedAt: Date.now() }
  }

  const markCancelled = () => {
    if (session.value) session.value.phase = 'cancelled'
  }

  const markFailed = () => {
    if (session.value) session.value.phase = 'failed'
  }

  const complete = () => {
    if (session.value) session.value.phase = 'completed'
  }

  const clear = () => {
    session.value = null
  }

  /** A checkout for this path that can be resumed. */
  const resumable = computed(() => (path: string | null) => {
    if (!path || !session.value || session.value.path !== path) return false
    return session.value.phase === 'cancelled' || session.value.phase === 'failed'
  })

  /** A checkout for this path that is running right now. */
  const running = computed(() => (path: string | null) => {
    if (!path || !session.value || session.value.path !== path) return false
    return session.value.phase === 'running'
  })

  return { session, begin, markCancelled, markFailed, complete, clear, resumable, running }
})
