import { afterEach, describe, expect, it, vi } from "vitest"

import { Invite } from "../../src/Invite"
import type { MessageQueue } from "../../src/MessageQueue"
import type { Session } from "../../src/Session"
import { DeviceRecord } from "../../src/session-manager/DeviceRecord"
import type { DeviceRecordDeps, DeviceRecordUserHooks, NostrFacade } from "../../src/session-manager/types"
import type { Rumor, Unsubscribe } from "../../src/types"

const makeRumor = (id: string, content = "message"): Rumor => ({
  id,
  pubkey: "sender-pubkey",
  created_at: Math.floor(Date.now() / 1000),
  kind: 14,
  tags: [],
  content,
})

type QueueEntry = {
  id: string
  targetKey: string
  event: Rumor
  createdAt: number
}

const createFakeSession = (name: string) => {
  let onEventCallback: ((event: Rumor) => void) | undefined
  const unsubscribeSpy = vi.fn()
  const sendEvent = vi.fn((event: Rumor) => ({
    event: {
      ...event,
      id: `outer-${event.id}`,
      sig: "sig",
    },
  }))
  const onEvent = vi.fn((cb: (event: Rumor) => void) => {
    onEventCallback = cb
    return unsubscribeSpy
  })
  const session = {
    name,
    sendEvent,
    onEvent,
  } as unknown as Session

  return {
    session,
    sendEvent,
    emit: (event: Rumor) => onEventCallback?.(event),
    unsubscribeSpy,
  }
}

const createDeps = (
  overrides: Partial<Omit<DeviceRecordDeps, "user" | "nostr" | "messageQueue">> = {}
) => {
  let authorized = true
  const userHooks: DeviceRecordUserHooks = {
    isDeviceAuthorized: vi.fn(() => authorized),
    onDeviceRumor: vi.fn(),
    onDeviceDirty: vi.fn(),
  }

  const messageQueueMock = {
    add: vi.fn(),
    getForTarget: vi.fn<() => Promise<QueueEntry[]>>().mockResolvedValue([]),
    removeForTarget: vi.fn().mockResolvedValue(undefined),
    removeByTargetAndEventId: vi.fn().mockResolvedValue(undefined),
    remove: vi.fn().mockResolvedValue(undefined),
  }

  const subscribe = vi.fn(() => vi.fn())
  const publish = vi.fn().mockResolvedValue(undefined)
  const nostr: NostrFacade = {
    subscribe: subscribe as NostrFacade["subscribe"],
    publish,
  }

  const deps: DeviceRecordDeps = {
    ownerPubkey: "owner-pubkey",
    user: userHooks,
    nostr,
    messageQueue: messageQueueMock as unknown as MessageQueue,
    ourDeviceId: "our-device-id",
    ourOwnerPubkey: "our-owner-pubkey",
    identityKey: new Uint8Array([1, 2, 3]),
    ...overrides,
  }

  return {
    deps,
    userHooks,
    messageQueueMock,
    publish,
    setAuthorized: (value: boolean) => {
      authorized = value
    },
  }
}

describe("DeviceRecord", () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it("subscribes for invites when ensuring setup for a remote device", async () => {
    const { deps } = createDeps()
    const unsubscribe = vi.fn() as Unsubscribe
    const fromUserSpy = vi.spyOn(Invite, "fromUser").mockReturnValue(unsubscribe)
    const record = new DeviceRecord("peer-device-id", deps)

    await record.ensureSetup()

    expect(record.state).toBe("waiting-for-invite")
    expect(fromUserSpy).toHaveBeenCalledWith(
      "peer-device-id",
      deps.nostr.subscribe,
      expect.any(Function)
    )
  })

  it("does not subscribe to invites for our own device", async () => {
    const { deps } = createDeps({ ourDeviceId: "same-device-id" })
    const fromUserSpy = vi.spyOn(Invite, "fromUser")
    const record = new DeviceRecord("same-device-id", deps)

    await record.ensureSetup()

    expect(record.state).toBe("new")
    expect(fromUserSpy).not.toHaveBeenCalled()
  })

  it("rejects invites that target a different device", async () => {
    const { deps } = createDeps()
    const record = new DeviceRecord("target-device-id", deps)
    const invite = {
      deviceId: "wrong-device-id",
      inviter: "wrong-device-id",
    } as Invite

    await expect(record.acceptInvite(invite)).rejects.toThrow("Invite does not target this device")
  })

  it("only forwards device rumors when authorization passes", () => {
    const { deps, userHooks, setAuthorized } = createDeps({
      ownerPubkey: "owner-pubkey",
      ourDeviceId: "our-device-id",
    })
    const record = new DeviceRecord("peer-device-id", deps)
    const { session, emit } = createFakeSession("session-a")
    const rumor = makeRumor("rumor-1", "hello")

    record.installSession(session, false, { persist: false })

    setAuthorized(false)
    emit(rumor)
    expect(userHooks.onDeviceRumor).not.toHaveBeenCalled()

    setAuthorized(true)
    emit(rumor)
    expect(userHooks.onDeviceRumor).toHaveBeenCalledWith("peer-device-id", rumor)
    expect(record.state).toBe("session-ready")
    expect(userHooks.onDeviceDirty).toHaveBeenCalledTimes(1)
  })

  it("flushes queued messages and keeps failed entries for retry", async () => {
    const { deps, messageQueueMock, publish, userHooks } = createDeps()
    const record = new DeviceRecord("peer-device-id", deps)
    const { session, sendEvent } = createFakeSession("session-b")

    const rumorOne = makeRumor("event-1", "first")
    const rumorTwo = makeRumor("event-2", "second")
    messageQueueMock.getForTarget.mockResolvedValue([
      { id: "event-1/peer-device-id", targetKey: "peer-device-id", event: rumorOne, createdAt: 1 },
      { id: "event-2/peer-device-id", targetKey: "peer-device-id", event: rumorTwo, createdAt: 2 },
    ])

    sendEvent
      .mockImplementationOnce((event: Rumor) => ({ event: { ...event, id: "outer-event-1", sig: "sig" } }))
      .mockImplementationOnce(() => {
        throw new Error("send failed")
      })

    record.installSession(session, false, { persist: false })
    await record.flushMessageQueue()

    expect(publish).toHaveBeenCalledTimes(1)
    expect(messageQueueMock.removeByTargetAndEventId).toHaveBeenCalledTimes(1)
    expect(messageQueueMock.removeByTargetAndEventId).toHaveBeenCalledWith(
      "peer-device-id",
      "event-1"
    )
    expect(userHooks.onDeviceDirty).toHaveBeenCalledTimes(1)
  })

  it("revoke clears subscriptions, sessions, and queued outbound messages", async () => {
    const { deps, messageQueueMock, userHooks } = createDeps()
    const inviteUnsubscribe = vi.fn() as Unsubscribe
    vi.spyOn(Invite, "fromUser").mockReturnValue(inviteUnsubscribe)

    const record = new DeviceRecord("peer-device-id", deps)
    await record.ensureSetup()

    const { session, unsubscribeSpy } = createFakeSession("session-c")
    record.installSession(session, false, { persist: false })
    await record.revoke()

    expect(record.state).toBe("revoked")
    expect(record.activeSession).toBeUndefined()
    expect(record.inactiveSessions).toEqual([])
    expect(inviteUnsubscribe).toHaveBeenCalledTimes(1)
    expect(unsubscribeSpy).toHaveBeenCalledTimes(1)
    expect(messageQueueMock.removeForTarget).toHaveBeenCalledWith("peer-device-id")
    expect(userHooks.onDeviceDirty).toHaveBeenCalled()
  })
})
