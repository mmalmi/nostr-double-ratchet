import { generateSecretKey, getPublicKey } from "nostr-tools"
import { describe, expect, it } from "vitest"

import {
  createDeviceLinkRequest,
  deterministicLinkInviteForDeviceLinkRequest,
  parseCompactDeviceLinkRequest,
} from "../src/DeviceLink"

describe("DeviceLink", () => {
  it("encodes scan-to-approve requests as one device pubkey plus one request secret", () => {
    const deviceAppKeySecretKey = generateSecretKey()
    const requestSecretKey = generateSecretKey()
    const local = createDeviceLinkRequest({
      deviceAppKeySecretKey,
      requestSecretKey,
      requestedAt: 41,
      label: "Iris Chat Web",
    })

    expect(local.code).toMatch(/^[0-9a-f]{64}\.[0-9a-f]{64}$/)
    expect(local.code).not.toContain("https://chat.iris.to")
    expect(local.code).not.toContain("nostrconnect:")
    expect(local.code).not.toContain("relay=")
    expect(local.code.length).toBe(129)
    expect(local.request.deviceAppKeyPubkey).toBe(getPublicKey(deviceAppKeySecretKey))
    expect(local.request.requestPubkey).toBe(getPublicKey(requestSecretKey))
    expect(local.request.requestSecret).toBe(toHex(requestSecretKey))

    expect(local.code.split(".")).toEqual([
      local.request.deviceAppKeyPubkey,
      toHex(requestSecretKey),
    ])

    const parsed = parseCompactDeviceLinkRequest(local.code)
    expect(parsed?.requestPubkey).toBe(local.request.requestPubkey)
    expect(parsed?.deviceAppKeyPubkey).toBe(local.request.deviceAppKeyPubkey)
    expect(parsed?.requestSecret).toBe(local.request.requestSecret)
    expect(parsed?.label).toBeUndefined()
  })

  it("rejects malformed approval codes", () => {
    expect(parseCompactDeviceLinkRequest("https://chat.iris.to/")).toBeNull()
    expect(parseCompactDeviceLinkRequest("not-a-compact-link-code")).toBeNull()
    expect(parseCompactDeviceLinkRequest(`${"a".repeat(64)}.${"b".repeat(64)}.extra`)).toBeNull()
  })

  it("derives deterministic NDR link invites from the compact secret", () => {
    const requestSecret =
      "0100000017000000c8010000d21e000000000000000000000000000000000000"
    const deviceAppKeyPubkey = "e".repeat(64)
    const request = {
      requestPubkey: getPublicKey(hexToBytes(requestSecret)),
      deviceAppKeyPubkey,
      requestSecret,
      requestedAt: 77,
    }

    const invite = deterministicLinkInviteForDeviceLinkRequest(request)
    const repeated = deterministicLinkInviteForDeviceLinkRequest({
      ...request,
      requestedAt: 88,
    })

    expect(toHex(invite.inviterEphemeralPrivateKey!)).toMatch(/^be3f1cca6354c294/)
    expect(invite.inviterEphemeralPublicKey).toBe(repeated.inviterEphemeralPublicKey)
    expect(invite.sharedSecret).toBe(repeated.sharedSecret)
    expect(invite.inviter).toBe(deviceAppKeyPubkey)
    expect(invite.deviceId).toBe(deviceAppKeyPubkey)
    expect(invite.maxUses).toBe(1)
    expect(invite.createdAt).toBe(0)
    expect(invite.purpose).toBe("link")
  })
})

function toHex(bytes: Uint8Array): string {
  return Array.from(bytes)
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("")
}

function hexToBytes(hex: string): Uint8Array {
  const bytes = new Uint8Array(hex.length / 2)
  for (let index = 0; index < bytes.length; index += 1) {
    bytes[index] = Number.parseInt(hex.slice(index * 2, index * 2 + 2), 16)
  }
  return bytes
}
