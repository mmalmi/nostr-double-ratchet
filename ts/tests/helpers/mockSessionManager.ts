import { vi } from "vitest"
import { generateSecretKey, getPublicKey, finalizeEvent, UnsignedEvent, VerifiedEvent } from "nostr-tools"
import { AppKeysManager, DelegateManager } from "../../src/AppKeysManager"
import { AppKeys } from "../../src/AppKeys"
import { InMemoryStorageAdapter, StorageAdapter } from "../../src/StorageAdapter"
import { NostrPublish, NostrSubscribe, APP_KEYS_EVENT_KIND } from "../../src/types"
import { SessionManager } from "../../src/SessionManager"
import { MockRelay } from "./mockRelay"

export async function createMockSessionManager(
  deviceId: string,
  relay: MockRelay,
  existingSecretKey?: Uint8Array,
  existingStorage?: StorageAdapter,
): Promise<{
  manager: SessionManager
  publicKey: string
  secretKey: Uint8Array
  publish: ReturnType<typeof vi.fn<NostrPublish>>
  mockStorage: StorageAdapter
  appKeysManager: AppKeysManager
}> {
  const secretKey = existingSecretKey || generateSecretKey()
  const publicKey = getPublicKey(secretKey)

  const mockStorage = existingStorage || new InMemoryStorageAdapter()

  // Create nostrSubscribe wired to relay
  const nostrSubscribe: NostrSubscribe = (filter, onEvent) => {
    const handle = relay.subscribe(filter, onEvent)
    return handle.close
  }

  // Create nostrPublish spy wired to relay
  const publish = vi.fn<NostrPublish>(async (event: UnsignedEvent) => {
    const signedEvent = finalizeEvent(event, secretKey)
    relay.storeAndDeliver(signedEvent as unknown as VerifiedEvent)
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
      relay.storeAndDeliver(event as unknown as VerifiedEvent)
      return event as unknown as VerifiedEvent
    }
    const privKey = delegateManagerHolder.manager?.getIdentityKey()
    if (!privKey) throw new Error("No delegate key available")
    const signedEvent = finalizeEvent(event as UnsignedEvent, privKey)
    relay.storeAndDeliver(signedEvent as unknown as VerifiedEvent)
    return signedEvent as unknown as VerifiedEvent
  })

  // Create DelegateManager
  const delegateStorage = new InMemoryStorageAdapter()
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

  // Activate deterministically (no relay polling)
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
    appKeysManager,
  }
}
