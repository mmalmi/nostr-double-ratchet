import { describe, expect, it } from "vitest"
import { createMockSessionManager } from "./helpers/mockSessionManager"
import { MockRelay } from "./helpers/mockRelay"

async function waitFor(
  predicate: () => boolean,
  timeoutMs = 4000,
  intervalMs = 25
): Promise<void> {
  const start = Date.now()
  while (Date.now() - start < timeoutMs) {
    if (predicate()) return
    await new Promise((resolve) => setTimeout(resolve, intervalMs))
  }
  throw new Error("Timed out waiting for condition")
}

describe("SessionManager local chat deletion", () => {
  it("deleteChat should tombstone locally and sync to sibling devices", async () => {
    const sharedRelay = new MockRelay()
    const alice1 = await createMockSessionManager("alice-device-1", sharedRelay)
    const alice2 = await createMockSessionManager("alice-device-2", sharedRelay, alice1.secretKey)
    const bob = await createMockSessionManager("bob-device-1", sharedRelay)

    // Ensure alice devices have an active sibling session so local tombstone sync can fan out.
    const siblingReady = new Promise<void>((resolve) => {
      alice2.manager.onEvent((event) => {
        if (event.content === "bootstrap-delete-sync") resolve()
      })
    })
    await alice1.manager.sendMessage(alice1.publicKey, "bootstrap-delete-sync")
    await siblingReady

    await alice1.manager.deleteChat(bob.publicKey)

    expect(alice1.manager.isChatTombstoned(bob.publicKey)).toBe(true)

    await waitFor(() => alice2.manager.isChatTombstoned(bob.publicKey))
    expect(alice2.manager.isChatTombstoned(bob.publicKey)).toBe(true)
  })
})
