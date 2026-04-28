import type { AppKeys } from "../AppKeys"
import type { VerifiedEvent } from "nostr-tools"
import type { MessageQueue } from "../MessageQueue"
import type { Session } from "../Session"
import type {
  IdentityKey,
  NostrPublish,
  NostrSubscribe,
  Rumor,
  Unsubscribe,
} from "../types"
import type { MessageOrigin } from "../MessageOrigin"

export type OnEventMeta = {
  fromDeviceId?: string
  outerEventId?: string
  senderOwnerPubkey?: string
  senderDevicePubkey?: string
  origin?: MessageOrigin
  isSelf?: boolean
  isCrossDeviceSelf?: boolean
}

export type OnEventCallback = (event: Rumor, from: string, meta?: OnEventMeta) => void

export interface InviteCredentials {
  ephemeralKeypair: { publicKey: string; privateKey: Uint8Array }
  sharedSecret: string
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
  appKeys?: AppKeys
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

export interface DeviceRecordUserHooks {
  isDeviceAuthorized(deviceId: string): boolean
  onDeviceRumor(deviceId: string, rumor: Rumor, outerEvent?: VerifiedEvent): void
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
  handleDeviceRumor(
    ownerPubkey: string,
    deviceId: string,
    rumor: Rumor,
    outerEvent?: VerifiedEvent,
  ): void
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
}

export type { Unsubscribe }
