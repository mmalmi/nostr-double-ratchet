import type { AppKeys } from "../AppKeys"
import type { MessageQueue } from "../MessageQueue"
import type { Session } from "../Session"
import type {
  IdentityKey,
  NostrPublish,
  NostrSubscribe,
  Rumor,
  Unsubscribe,
} from "../types"

export type OnEventMeta = { fromDeviceId?: string }
export type OnEventCallback = (event: Rumor, from: string, meta?: OnEventMeta) => void

/**
 * Credentials for the invite handshake - used to listen for and decrypt invite responses
 */
export interface InviteCredentials {
  ephemeralKeypair: { publicKey: string; privateKey: Uint8Array }
  sharedSecret: string
}

export interface AcceptInviteOptions {
  ownerPublicKey?: string
}

export interface AcceptInviteResult {
  ownerPublicKey: string
  deviceId: string
  session: Session
}

export interface StoredSessionEntry {
  name: string
  state: string
}

export interface StoredDeviceRecord {
  deviceId: string
  activeSession: StoredSessionEntry | null
  inactiveSessions: StoredSessionEntry[]
  createdAt: number
}

export interface StoredUserRecord {
  publicKey: string
  devices: StoredDeviceRecord[]
  appKeys?: string
}

export type UserSetupState =
  | "new"
  | "fetching-appkeys"
  | "appkeys-known"
  | "ready"
  | "stale"

export interface UserSetupStatus {
  ownerPublicKey: string
  state: UserSetupState
  ready: boolean
  appKeysKnown: boolean
}

export type DeviceSetupState =
  | "new"
  | "waiting-for-invite"
  | "accepting-invite"
  | "session-ready"
  | "stale"
  | "revoked"

export interface NostrFacade {
  subscribe: NostrSubscribe
  publish: (event: Parameters<NostrPublish>[0]) => Promise<void>
}

export interface DeviceRecord {
  deviceId: string
  activeSession?: Session
  inactiveSessions: Session[]
  createdAt: number
}

export interface UserRecord {
  publicKey: string
  devices: Map<string, DeviceRecord>
  /** Full AppKeys for this user - single source of truth for device list */
  appKeys?: AppKeys
}

export interface DeviceRecordUserHooks {
  isDeviceAuthorized(deviceId: string): boolean
  onDeviceRumor(deviceId: string, rumor: Rumor): void
  onDeviceDirty(): void
}

export interface DeviceRecordDeps {
  ownerPubkey: string
  user: DeviceRecordUserHooks
  nostr: NostrFacade
  messageQueue: MessageQueue
  ourDeviceId: string
  ourOwnerPubkey: string
  identityKey: IdentityKey
  createdAt?: number
}

export interface UserRecordManagerHooks {
  updateDelegateMapping(ownerPubkey: string, appKeys: AppKeys): void
  removeDelegateMapping(deviceId: string): void
  handleDeviceRumor(ownerPubkey: string, deviceId: string, rumor: Rumor): void
  persistUserRecord(ownerPubkey: string): void
}

export interface UserRecordDeps {
  manager: UserRecordManagerHooks
  nostr: NostrFacade
  messageQueue: MessageQueue
  discoveryQueue: MessageQueue
  ourDeviceId: string
  ourOwnerPubkey: string
  identityKey: IdentityKey
  onSetupStateChange: (ownerPubkey: string) => void
}

export type { Unsubscribe }
