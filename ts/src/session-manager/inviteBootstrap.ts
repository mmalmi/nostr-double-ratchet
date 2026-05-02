import type { VerifiedEvent } from "nostr-tools"
import type { Session } from "../Session"
import { buildTypingRumor } from "../messageBuilders"

export const INVITE_BOOTSTRAP_EXPIRATION_SECONDS = 60
export const INVITE_BOOTSTRAP_RETRY_DELAYS_MS = [0, 500, 1500] as const

export function planInviteBootstrapEvents(session: Session): VerifiedEvent[] {
  const expiresAt =
    Math.floor(Date.now() / 1000) +
    INVITE_BOOTSTRAP_EXPIRATION_SECONDS

  return INVITE_BOOTSTRAP_RETRY_DELAYS_MS.map(
    () => session.sendEvent(buildTypingRumor({ expiration: { expiresAt } })).event
  )
}

export function scheduleInviteBootstrapRetryEvents(
  events: readonly VerifiedEvent[],
  publish: (event: VerifiedEvent) => Promise<void>,
  trackedTimeouts: Set<ReturnType<typeof setTimeout>>,
): void {
  events.slice(1).forEach((event, index) => {
    const timeout = setTimeout(() => {
      trackedTimeouts.delete(timeout)
      void publish(event).catch(() => {
        // Best-effort retry publish. A later inbound event can still recover the session.
      })
    }, INVITE_BOOTSTRAP_RETRY_DELAYS_MS[index + 1])

    trackedTimeouts.add(timeout)
  })
}
