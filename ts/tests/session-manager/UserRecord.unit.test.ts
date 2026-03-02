import { describe, expect, it, vi } from "vitest"

import type { AppKeys } from "../../src/AppKeys"
import type { MessageQueue } from "../../src/MessageQueue"
import { UserRecord } from "../../src/session-manager/UserRecord"
import type { UserRecordDeps, UserRecordManagerHooks } from "../../src/session-manager/types"
import type { Rumor } from "../../src/types"

const makeRumor = (id: string, content = "message"): Rumor => ({
  id,
  pubkey: "sender-pubkey",
  created_at: Math.floor(Date.now() / 1000),
  kind: 14,
  tags: [],
  content,
})

const createAppKeysMock = (deviceIds: string[]): AppKeys =>
  ({
    getAllDevices: () => deviceIds.map((identityPubkey) => ({ identityPubkey })),
    serialize: () => JSON.stringify({ deviceIds }),
  }) as unknown as AppKeys

const createQueueMock = () =>
  ({
    add: vi.fn().mockResolvedValue("entry-id"),
    getForTarget: vi.fn().mockResolvedValue([]),
    removeForTarget: vi.fn().mockResolvedValue(undefined),
    removeByTargetAndEventId: vi.fn().mockResolvedValue(undefined),
    remove: vi.fn().mockResolvedValue(undefined),
  }) as unknown as MessageQueue

const createHarness = (ourDeviceId = "our-device-id") => {
  const manager: UserRecordManagerHooks = {
    updateDelegateMapping: vi.fn(),
    removeDelegateMapping: vi.fn(),
    handleDeviceRumor: vi.fn(),
    persistUserRecord: vi.fn(),
  }

  const setupStateChanges: string[] = []

  const deps: UserRecordDeps = {
    manager,
    nostr: {
      subscribe: vi.fn(() => vi.fn()),
      publish: vi.fn().mockResolvedValue(undefined),
    },
    messageQueue: createQueueMock(),
    discoveryQueue: createQueueMock(),
    ourDeviceId,
    ourOwnerPubkey: "our-owner-pubkey",
    identityKey: new Uint8Array([1, 2, 3]),
    onSetupStateChange: (_ownerPubkey) => {
      setupStateChanges.push("changed")
    },
  }

  const record = new UserRecord("peer-owner-pubkey", deps)

  return {
    record,
    deps,
    manager,
    setupStateChanges,
  }
}

describe("UserRecord", () => {
  it("creates one device record per device id and rejects empty ids", () => {
    const { record } = createHarness()

    expect(() => record.ensureDevice("")).toThrow("Device record must include a deviceId")

    const first = record.ensureDevice("peer-device-1")
    const second = record.ensureDevice("peer-device-1")

    expect(first).toBe(second)
    expect(record.devices.size).toBe(1)
  })

  it("queues outbound events in discovery queue when app keys are unknown", async () => {
    const { record, deps } = createHarness()
    const rumor = makeRumor("rumor-1")

    await record.queueOutboundMessage(rumor)

    expect(deps.discoveryQueue.add).toHaveBeenCalledWith("peer-owner-pubkey", rumor)
    expect(deps.messageQueue.add).not.toHaveBeenCalled()
  })

  it("fans out outbound events to non-self devices when app keys are known", async () => {
    const { record, deps } = createHarness("our-device-id")
    const rumor = makeRumor("rumor-2")
    record.setAppKeys(createAppKeysMock(["our-device-id", "peer-device-1", "peer-device-2"]))

    await record.queueOutboundMessage(rumor)

    expect(deps.discoveryQueue.add).not.toHaveBeenCalled()
    expect(deps.messageQueue.add).toHaveBeenCalledTimes(2)
    expect(deps.messageQueue.add).toHaveBeenCalledWith("peer-device-1", rumor)
    expect(deps.messageQueue.add).toHaveBeenCalledWith("peer-device-2", rumor)
  })

  it("onAppKeys revokes removed devices and persists updated state", async () => {
    const { record, manager } = createHarness()

    const staleDevice = {
      deviceId: "stale-device",
      revoke: vi.fn().mockResolvedValue(undefined),
      ensureSetup: vi.fn().mockResolvedValue(undefined),
      deactivateCurrentSession: vi.fn(),
      close: vi.fn(),
    }
    const keepDevice = {
      deviceId: "keep-device",
      revoke: vi.fn().mockResolvedValue(undefined),
      ensureSetup: vi.fn().mockResolvedValue(undefined),
      deactivateCurrentSession: vi.fn(),
      close: vi.fn(),
    }

    record.devices.set("stale-device", staleDevice as never)
    record.devices.set("keep-device", keepDevice as never)

    const appKeys = createAppKeysMock(["keep-device"])
    await record.onAppKeys(appKeys)

    expect(manager.updateDelegateMapping).toHaveBeenCalledWith("peer-owner-pubkey", appKeys)
    expect(staleDevice.revoke).toHaveBeenCalledTimes(1)
    expect(manager.removeDelegateMapping).toHaveBeenCalledWith("stale-device")
    expect(record.devices.has("stale-device")).toBe(false)
    expect(record.devices.has("keep-device")).toBe(true)
    expect(keepDevice.ensureSetup).toHaveBeenCalledTimes(1)
    expect(record.state).toBe("ready")
    expect(manager.persistUserRecord).toHaveBeenCalledWith("peer-owner-pubkey")
  })

  it("marks setup as stale when app keys cannot be fetched", async () => {
    const { record, deps, setupStateChanges } = createHarness()
    vi.spyOn(record as never, "fetchAppKeys").mockResolvedValue(null)

    await record.ensureSetup()

    expect(record.state).toBe("stale")
    expect(deps.nostr.subscribe).toHaveBeenCalled()
    expect(setupStateChanges.length).toBeGreaterThan(0)
  })

  it("authorizes owner key immediately and delegates from app keys", () => {
    const { record } = createHarness()

    expect(record.isDeviceAuthorized("peer-owner-pubkey")).toBe(true)
    expect(record.isDeviceAuthorized("peer-device-1")).toBe(false)

    record.setAppKeys(createAppKeysMock(["peer-device-1"]))
    expect(record.isDeviceAuthorized("peer-device-1")).toBe(true)
  })
})
