import { describe, expect, it } from "vitest"

import type { Session } from "../../src/Session"
import type {
  AcceptInviteResult,
  DeviceSetupState,
  StoredDeviceRecord,
  StoredSessionEntry,
  StoredUserRecord,
  UserSetupState,
  UserSetupStatus,
} from "../../src/session-manager/types"

describe("session-manager types", () => {
  it("defines the expected setup state literals", () => {
    const userStates: UserSetupState[] = [
      "new",
      "fetching-appkeys",
      "appkeys-known",
      "ready",
      "stale",
    ]
    const deviceStates: DeviceSetupState[] = [
      "new",
      "waiting-for-invite",
      "accepting-invite",
      "session-ready",
      "stale",
      "revoked",
    ]

    expect(new Set(userStates).size).toBe(5)
    expect(new Set(deviceStates).size).toBe(6)
  })

  it("captures stored record shapes used by persistence", () => {
    const storedSession: StoredSessionEntry = {
      name: "session-name",
      state: "{\"state\":\"serialized\"}",
    }
    const storedDevice: StoredDeviceRecord = {
      deviceId: "device-id",
      activeSession: storedSession,
      inactiveSessions: [storedSession],
      createdAt: 1700000000,
    }
    const storedUser: StoredUserRecord = {
      publicKey: "owner-pubkey",
      devices: [storedDevice],
      appKeys: "serialized-app-keys",
    }

    expect(storedUser.devices[0]?.deviceId).toBe("device-id")
    expect(storedUser.devices[0]?.inactiveSessions).toHaveLength(1)
  })

  it("defines setup status and invite acceptance result contracts", () => {
    const status: UserSetupStatus = {
      ownerPublicKey: "owner-pubkey",
      state: "new",
      ready: false,
      appKeysKnown: false,
    }
    const accepted: AcceptInviteResult = {
      ownerPublicKey: "owner-pubkey",
      deviceId: "device-id",
      session: {} as Session,
    }

    expect(status.ready).toBe(false)
    expect(accepted.deviceId).toBe("device-id")
  })
})
