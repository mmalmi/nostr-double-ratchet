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
  it("deleteChat should remove local session state and allow explicit reinit", async () => {
    const sharedRelay = new MockRelay()
    const alice = await createMockSessionManager("alice-device-1", sharedRelay)
    const bob = await createMockSessionManager("bob-device-1", sharedRelay)

    alice.manager.setupUser(bob.publicKey)
    expect(alice.manager.getUserRecords().has(bob.publicKey)).toBe(true)

    await alice.manager.deleteChat(bob.publicKey)
    expect(alice.manager.getUserRecords().has(bob.publicKey)).toBe(false)

    await alice.manager.sendMessage(bob.publicKey, "after-delete-reinit")

    await waitFor(() => alice.manager.getUserRecords().has(bob.publicKey))
    expect(alice.manager.getUserRecords().has(bob.publicKey)).toBe(true)
  })
})
