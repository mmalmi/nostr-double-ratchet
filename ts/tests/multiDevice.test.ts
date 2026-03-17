import { describe, expect, it } from "vitest"
import { AppKeys } from "../src/AppKeys"
import {
  applyAppKeysSnapshot,
  evaluateDeviceRegistrationState,
  hasExistingSessionWithRecipient,
  resolveConversationCandidatePubkeys,
  resolveInviteOwnerRouting,
  resolveSessionPubkeyToOwner,
  shouldRequireRelayRegistrationConfirmation,
  type SessionUserRecordsLike,
} from "../src/multiDevice"

describe("multiDevice helpers", () => {
  it("advances to newer AppKeys snapshots", () => {
    const current = new AppKeys([{ identityPubkey: "device-1", createdAt: 100 }])
    const incoming = new AppKeys([
      { identityPubkey: "device-1", createdAt: 100 },
      { identityPubkey: "device-2", createdAt: 101 },
    ])

    const applied = applyAppKeysSnapshot({
      currentAppKeys: current,
      currentCreatedAt: 100,
      incomingAppKeys: incoming,
      incomingCreatedAt: 101,
    })

    expect(applied.decision).toBe("advanced")
    expect(applied.appKeys.getAllDevices()).toEqual([
      { identityPubkey: "device-1", createdAt: 100 },
      { identityPubkey: "device-2", createdAt: 101 },
    ])
  })

  it("ignores stale AppKeys snapshots", () => {
    const current = new AppKeys([{ identityPubkey: "device-2", createdAt: 101 }])
    const incoming = new AppKeys([{ identityPubkey: "device-1", createdAt: 100 }])

    const applied = applyAppKeysSnapshot({
      currentAppKeys: current,
      currentCreatedAt: 101,
      incomingAppKeys: incoming,
      incomingCreatedAt: 100,
    })

    expect(applied.decision).toBe("stale")
    expect(applied.appKeys.getAllDevices()).toEqual([
      { identityPubkey: "device-2", createdAt: 101 },
    ])
  })

  it("merges same-second AppKeys snapshots monotonically", () => {
    const current = new AppKeys([{ identityPubkey: "device-1", createdAt: 100 }])
    const incoming = new AppKeys([
      { identityPubkey: "device-1", createdAt: 100 },
      { identityPubkey: "device-2", createdAt: 100 },
    ])

    const applied = applyAppKeysSnapshot({
      currentAppKeys: current,
      currentCreatedAt: 100,
      incomingAppKeys: incoming,
      incomingCreatedAt: 100,
    })

    expect(applied.decision).toBe("merged_equal_timestamp")
    expect(applied.appKeys.getAllDevices()).toEqual([
      { identityPubkey: "device-1", createdAt: 100 },
      { identityPubkey: "device-2", createdAt: 100 },
    ])
  })

  it("evaluates device registration state", () => {
    const state = evaluateDeviceRegistrationState({
      currentDevicePubkey: "device-2",
      registeredDevices: [{ identityPubkey: "device-1" }],
      hasLocalAppKeys: false,
      appKeysManagerReady: true,
      sessionManagerReady: true,
    })

    expect(state.isCurrentDeviceRegistered).toBe(false)
    expect(state.hasKnownRegisteredDevices).toBe(true)
    expect(state.noPreviousDevicesFound).toBe(false)
    expect(state.requiresDeviceRegistration).toBe(true)
    expect(state.canSendPrivateMessages).toBe(true)
  })

  it("skips relay confirmation for first-device bootstrap", () => {
    expect(
      shouldRequireRelayRegistrationConfirmation({
        currentDevicePubkey: "device-1",
        registeredDevices: [],
        hasLocalAppKeys: false,
        appKeysManagerReady: true,
        sessionManagerReady: true,
      })
    ).toBe(false)
  })

  it("requires relay confirmation when adding a new device to an existing owner", () => {
    expect(
      shouldRequireRelayRegistrationConfirmation({
        currentDevicePubkey: "device-2",
        registeredDevices: [{ identityPubkey: "device-1" }],
        hasLocalAppKeys: false,
        appKeysManagerReady: true,
        sessionManagerReady: true,
      })
    ).toBe(true)
  })

  it("orders self-targeted conversation candidates by linked device before owner", () => {
    const candidates = resolveConversationCandidatePubkeys({
      ownerPubkey: "owner",
      senderPubkey: "owner",
      rumor: {
        pubkey: "linked-device",
        tags: [["p", "owner"]],
      },
    })

    expect(candidates).toEqual(["linked-device", "owner"])
  })

  it("falls back to device identity for unverified chat owner claims", () => {
    const resolution = resolveInviteOwnerRouting({
      devicePubkey: "device-1",
      claimedOwnerPublicKey: "owner-1",
      invitePurpose: "chat",
      currentOwnerPublicKey: "my-owner",
      appKeys: new AppKeys([{ identityPubkey: "other-device", createdAt: 1 }]),
    })

    expect(resolution.ownerPublicKey).toBe("device-1")
    expect(resolution.fellBackToDeviceIdentity).toBe(true)
  })

  it("keeps owner-side link invite routing even before AppKeys registration", () => {
    const resolution = resolveInviteOwnerRouting({
      devicePubkey: "device-1",
      claimedOwnerPublicKey: "my-owner",
      invitePurpose: "link",
      currentOwnerPublicKey: "my-owner",
      appKeys: null,
    })

    expect(resolution.ownerPublicKey).toBe("my-owner")
    expect(resolution.usedLinkBootstrapException).toBe(true)
  })

  it("resolves linked device pubkeys back to the owner record", () => {
    const userRecords: SessionUserRecordsLike = new Map([
      [
        "owner-1",
        {
          devices: new Map([
            [
              "linked-device",
              {
                activeSession: null,
                inactiveSessions: [],
              },
            ],
          ]),
          appKeys: {
            getAllDevices: () => [{ identityPubkey: "linked-device" }],
          },
        },
      ],
    ])

    expect(resolveSessionPubkeyToOwner(userRecords, "linked-device")).toBe("owner-1")
  })

  it("detects existing sessions for a recipient across linked devices", () => {
    const userRecords: SessionUserRecordsLike = new Map([
      [
        "owner-1",
        {
          devices: new Map([
            [
              "linked-device",
              {
                activeSession: {
                  state: {
                    theirCurrentNostrPublicKey: "peer-1",
                  },
                },
                inactiveSessions: [],
              },
            ],
          ]),
        },
      ],
    ])

    expect(hasExistingSessionWithRecipient(userRecords, "peer-1")).toBe(true)
  })
})
