import { vi } from "vitest"
import { generateSecretKey, getPublicKey, finalizeEvent, UnsignedEvent, VerifiedEvent } from "nostr-tools"
import { AppKeysManager, DelegateManager } from "../../src/AppKeysManager"
import { AppKeys } from "../../src/AppKeys"
import { InMemoryStorageAdapter, StorageAdapter } from "../../src/StorageAdapter"
import { NostrPublish, NostrSubscribe, APP_KEYS_EVENT_KIND } from "../../src/types"
import { SessionManager } from "../../src/SessionManager"
import { ControlledMockRelay } from "./ControlledMockRelay"

export async function createControlledMockSessionManager(
  _deviceId: string,
  relay: ControlledMockRelay,
  existingSecretKey?: Uint8Array,
  existingStorage?: StorageAdapter,
  existingDelegateStorage?: StorageAdapter,
  _options?: { autoDeliver?: boolean },
): Promise<{
  manager: SessionManager
  publicKey: string
  secretKey: Uint8Array
  publish: ReturnType<typeof vi.fn<NostrPublish>>
  mockStorage: StorageAdapter
}> {
  const secretKey = existingSecretKey || generateSecretKey()
  const publicKey = getPublicKey(secretKey)

  const mockStorage = existingStorage || new InMemoryStorageAdapter()

  // Create nostrSubscribe wired to relay
  const nostrSubscribe: NostrSubscribe = (filter, onEvent) => {
    const handle = relay.subscribe(filter, onEvent)
    return handle.close
  }

  // Create nostrPublish spy — always uses publishAndDeliver during setup
  const publish = vi.fn<NostrPublish>(async (event: UnsignedEvent) => {
    const signedEvent = finalizeEvent(event, secretKey)
    await relay.publishAndDeliver(signedEvent as unknown as VerifiedEvent)
    return signedEvent as unknown as VerifiedEvent
  })

  // Create AppKeysManager
  const appKeysManager = new AppKeysManager({
    nostrPublish: publish,
    storage: new InMemoryStorageAdapter(),
  })
  await appKeysManager.init()

  // Check for existing AppKeys on the relay for this owner (multi-device support)
  const existingEvents = relay.getAllEvents()
  for (const event of existingEvents) {
    if (event.kind === APP_KEYS_EVENT_KIND && event.pubkey === publicKey) {
      const tags = event.tags || []
      const dTag = tags.find((t) => t[0] === "d" && t[1] === "double-ratchet/app-keys")
      if (dTag) {
        try {
          const appKeys = AppKeys.fromEvent(event)
          await appKeysManager.setAppKeys(appKeys)
        } catch {
          // ignore invalid
        }
      }
    }
  }

  // Create delegate publish that signs with delegate key
  const delegateManagerHolder: { manager: DelegateManager | null } = { manager: null }
  const delegatePublish = vi.fn<NostrPublish>(async (event: UnsignedEvent | VerifiedEvent) => {
    if ("sig" in event && event.sig) {
      await relay.publishAndDeliver(event as unknown as VerifiedEvent)
      return event as unknown as VerifiedEvent
    }
    const privKey = delegateManagerHolder.manager?.getIdentityKey()
    if (!privKey) throw new Error("No delegate key available")
    const signedEvent = finalizeEvent(event as UnsignedEvent, privKey)
    await relay.publishAndDeliver(signedEvent as unknown as VerifiedEvent)
    return signedEvent as unknown as VerifiedEvent
  })

  // Create DelegateManager
  const delegateStorage = existingDelegateStorage || new InMemoryStorageAdapter()
  const delegateManager = new DelegateManager({
    nostrSubscribe,
    nostrPublish: delegatePublish,
    storage: delegateStorage,
  })
  delegateManagerHolder.manager = delegateManager

  await delegateManager.init()

  // Add device to AppKeysManager and publish
  appKeysManager.addDevice(delegateManager.getRegistrationPayload())
  await appKeysManager.publish()

  // Activate deterministically
  await delegateManager.activate(publicKey)

  // Create SessionManager
  const manager = delegateManager.createSessionManager(mockStorage)
  await manager.init()

  return {
    manager,
    publicKey,
    secretKey,
    publish,
    mockStorage,
  }
}
