import { Filter, VerifiedEvent, UnsignedEvent, getPublicKey, verifyEvent } from "nostr-tools"
import * as nip44 from "nostr-tools/nip44"
import { applyAppKeysSnapshot } from "./multiDevice"
import { APP_KEYS_EVENT_KIND, NostrSubscribe, Unsubscribe } from "./types"

const now = () => Math.round(Date.now() / 1000)
export const LEGACY_APP_KEYS_EVENT_KIND = 30078
export const NOSTR_IDENTITY_ROSTER_OP_KIND = 7368
export const NOSTR_IDENTITY_ROSTER_SNAPSHOT_KIND = 37368
export const NOSTR_IDENTITY_ROSTER_TYPE = "nostr_identity_roster_op"
export const NOSTR_IDENTITY_ROSTER_SNAPSHOT_TYPE = "nostr_identity_roster_snapshot"
export const NOSTR_IDENTITY_ROSTER_SCHEMA = 1
export const NOSTR_IDENTITY_ENCRYPTED_DEVICE_LABELS_FACT = "encrypted_device_labels"
export const NOSTR_IDENTITY_ENCRYPTED_DEVICE_LABELS_SCHEMA = 1
export const NOSTR_IDENTITY_OWNER_PUBKEY_FACT = "owner_pubkey"
const NOSTR_IDENTITY_APP_PURPOSE = "app"
const NOSTR_IDENTITY_ADMIN_CAPABILITY = "admin"
const NOSTR_IDENTITY_WRITE_CAPABILITY = "write"

export interface NostrIdentityRosterFilterOptions {
  profileIds?: string | string[]
  authors?: string | string[]
  since?: number
  until?: number
  limit?: number
}

export interface AppKeysEventOptions {
  ownerPrivateKey?: Uint8Array
  ownerPubkey?: string
  profileId?: string
  createdAt?: number
  heads?: string[]
}

export interface ParsedAppKeysSnapshot {
  profileId: string
  ownerPubkey: string
  appKeys: AppKeys
  createdAt: number
}

// Simplified tag format: ["device", identityPubkey, createdAt]
type DeviceTag = [
  type: "device",
  identityPubkey: string,
  createdAt: string,
]

const isDeviceTag = (tag: string[]): tag is DeviceTag =>
  tag.length >= 3 &&
  tag[0] === "device" &&
  typeof tag[1] === "string" &&
  typeof tag[2] === "string"

export function buildAppKeysFilter(authors?: string | string[]): Filter {
  const normalizedAuthors = Array.isArray(authors)
    ? authors.filter(Boolean)
    : authors ? [authors] : undefined

  return normalizedAuthors && normalizedAuthors.length > 0
    ? {
        kinds: [APP_KEYS_EVENT_KIND],
        authors: normalizedAuthors,
      }
    : {
        kinds: [APP_KEYS_EVENT_KIND],
      }
}

export function isAppKeysEvent(
  event: Pick<VerifiedEvent, "kind" | "tags">
): boolean {
  if (event.kind !== NOSTR_IDENTITY_ROSTER_SNAPSHOT_KIND) {
    return false
  }

  return event.tags.some(
    (tag) => tag[0] === "type" && tag[1] === NOSTR_IDENTITY_ROSTER_SNAPSHOT_TYPE
  )
}

/**
 * Device identity entry - contains only identity information.
 * identityPubkey serves as the device identifier.
 * Invite crypto material (ephemeral keys, shared secret) is in separate Invite events.
 */
export interface DeviceEntry {
  /** Identity public key - also serves as device identifier */
  identityPubkey: string
  createdAt: number
}

export interface DeviceLabels {
  deviceLabel?: string
  clientLabel?: string
  updatedAt: number
}

export interface NostrIdentityEncryptedDeviceLabelsPayload {
  schema: typeof NOSTR_IDENTITY_ENCRYPTED_DEVICE_LABELS_SCHEMA
  profileId: string
  secretEpoch: number
  labels: Record<string, string>
  updatedAt: number
}

export type NostrIdentityAppKeyFacet = {
  pubkey: string
  purposes: Set<string>
  capabilities: Set<string>
  addedAt: number
}

export type NostrIdentityRosterOp =
  | { op: "add_key"; key: NostrIdentityAppKeyFacet }
  | { op: "tombstone_key"; pubkey: string }
  | { op: "set_key_capabilities"; pubkey: string; capabilities: Set<string> }
  | { op: "ignore" }

export type SignedNostrIdentityRosterOp = {
  opId: string
  profileId: string
  signerPubkey: string
  createdAt: number
  op: NostrIdentityRosterOp
}

interface DeviceLabelsEntry extends DeviceLabels {
  identityPubkey: string
}

interface EncryptedAppKeysContent {
  type: "app-keys-labels"
  v: 1
  deviceLabels: DeviceLabelsEntry[]
}

type LegacyDeviceLabelsEntry = Partial<{
  identityPubkey: unknown
  identity_pubkey: unknown
  deviceLabel: unknown
  device_label: unknown
  clientLabel: unknown
  client_label: unknown
  updatedAt: unknown
  updated_at: unknown
}>

type LegacyEncryptedAppKeysContent = Partial<{
  type: unknown
  v: unknown
  deviceLabels: unknown
  device_labels: unknown
}>

const normalizeDeviceLabelsEntry = (value: unknown): DeviceLabelsEntry | null => {
  if (!value || typeof value !== "object") return null
  const entry = value as LegacyDeviceLabelsEntry
  const identityPubkey = entry.identityPubkey ?? entry.identity_pubkey
  const updatedAt = entry.updatedAt ?? entry.updated_at
  const deviceLabel = entry.deviceLabel ?? entry.device_label
  const clientLabel = entry.clientLabel ?? entry.client_label

  if (typeof identityPubkey !== "string" || typeof updatedAt !== "number") {
    return null
  }
  if (deviceLabel !== undefined && typeof deviceLabel !== "string") {
    return null
  }
  if (clientLabel !== undefined && typeof clientLabel !== "string") {
    return null
  }

  return {
    identityPubkey,
    updatedAt,
    ...(deviceLabel !== undefined ? { deviceLabel } : {}),
    ...(clientLabel !== undefined ? { clientLabel } : {}),
  }
}

export function isNostrIdentityRosterOpEvent(
  event: Pick<VerifiedEvent, "kind" | "tags">
): boolean {
  return event.kind === NOSTR_IDENTITY_ROSTER_OP_KIND
    && tagValues(event.tags, "type").includes(NOSTR_IDENTITY_ROSTER_TYPE)
}

export function buildNostrIdentityRosterFilter(
  options: NostrIdentityRosterFilterOptions = {}
): Filter {
  const filter: Filter = {
    kinds: [NOSTR_IDENTITY_ROSTER_OP_KIND],
  }
  const profileIds = normalizeStringList(options.profileIds).map(canonicalProfileId)
  if (profileIds.length > 0) filter["#i"] = profileIds
  const authors = normalizeStringList(options.authors)
    .map((author) => requireHexPubkey(author, "author"))
  if (authors.length > 0) filter.authors = authors
  if (options.since !== undefined) filter.since = options.since
  if (options.until !== undefined) filter.until = options.until
  if (options.limit !== undefined) filter.limit = options.limit
  return filter
}

export function buildNostrIdentityRosterSnapshotFilter(
  authors?: string | string[]
): Filter {
  return buildAppKeysFilter(authors)
}

export function encryptedDeviceLabelPayloadsFromNostrIdentityRosterOpEvent(
  event: Pick<VerifiedEvent, "tags">
): string[] {
  return tagValues(event.tags, NOSTR_IDENTITY_ENCRYPTED_DEVICE_LABELS_FACT)
}

function tagValues(tags: string[][], name: string): string[] {
  return tags
    .filter((tag) => tag[0] === name)
    .map((tag) => tag[1]?.trim() ?? "")
    .filter(Boolean)
}

function normalizeStringList(value: string | string[] | undefined): string[] {
  if (Array.isArray(value)) {
    return value.map((item) => item.trim()).filter(Boolean)
  }
  return value?.trim() ? [value.trim()] : []
}

function normalizeEventIds(value: string[] | undefined): string[] {
  return (value ?? [])
    .map((item) => item.trim().toLowerCase())
    .filter((item) => /^[0-9a-f]{64}$/.test(item))
    .sort()
}

function firstTagValue(tags: string[][], name: string): string | undefined {
  return tagValues(tags, name)[0]
}

function requireTagValue(tags: string[][], name: string): string {
  const value = firstTagValue(tags, name)
  if (!value) throw new Error(`NostrIdentity roster missing ${name}`)
  return value
}

function normalizeHexPubkey(value: string): string | null {
  const trimmed = value.trim().toLowerCase()
  return /^[0-9a-f]{64}$/.test(trimmed) ? trimmed : null
}

function requireHexPubkey(value: string, label: string): string {
  const normalized = normalizeHexPubkey(value)
  if (!normalized) throw new Error(`NostrIdentity ${label} pubkey must be 64-char hex`)
  return normalized
}

function requireInteger(value: string, label: string): number {
  if (!/^\d+$/.test(value)) throw new Error(`NostrIdentity ${label} must be an integer`)
  const parsed = Number(value)
  if (!Number.isSafeInteger(parsed)) throw new Error(`NostrIdentity ${label} is too large`)
  return parsed
}

function profileIdFromTags(tags: string[][]): string {
  const profileId = tags
    .find((tag) => tag[0] === "i" && tag[2] === "subject")
    ?.at(1)
    ?.trim()
  if (
    !profileId
    || !/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(profileId)
  ) {
    throw new Error("NostrIdentity roster missing profile subject")
  }
  return profileId.toLowerCase()
}

export function parseNostrIdentityRosterOpEvent(event: VerifiedEvent): SignedNostrIdentityRosterOp {
  if (!verifyEvent(event)) {
    throw new Error("NostrIdentity roster signature is invalid")
  }
  if (!isNostrIdentityRosterOpEvent(event)) {
    throw new Error("Event is not an NostrIdentity roster op")
  }
  if (event.content !== "") {
    throw new Error("NostrIdentity roster fact event content must be empty")
  }
  const schema = requireInteger(requireTagValue(event.tags, "schema"), "schema")
  if (schema !== NOSTR_IDENTITY_ROSTER_SCHEMA) {
    throw new Error(`Unsupported NostrIdentity roster schema ${schema}`)
  }
  const profileId = profileIdFromTags(event.tags)
  const actorPubkey = requireHexPubkey(requireTagValue(event.tags, "actor_pubkey"), "actor")
  const signerPubkey = requireHexPubkey(event.pubkey, "signer")
  if (actorPubkey !== signerPubkey) {
    throw new Error("NostrIdentity roster actor signer mismatch")
  }
  const createdAt = requireInteger(requireTagValue(event.tags, "created_at"), "created_at")
  if (createdAt !== event.created_at) {
    throw new Error("NostrIdentity roster created_at mismatch")
  }
  return {
    opId: event.id,
    profileId,
    signerPubkey,
    createdAt,
    op: parseNostrIdentityRosterOp(event.tags),
  }
}

function parseNostrIdentityRosterOp(tags: string[][]): NostrIdentityRosterOp {
  const op = requireTagValue(tags, "op")
  if (op === "add_key") {
    return {
      op,
      key: {
        pubkey: requireHexPubkey(requireTagValue(tags, "key_pubkey"), "key"),
        purposes: new Set(tagValues(tags, "key_purpose")),
        capabilities: new Set(tagValues(tags, "key_capability")),
        addedAt: requireInteger(requireTagValue(tags, "key_added_at"), "key_added_at"),
      },
    }
  }
  if (op === "tombstone_key") {
    return {
      op,
      pubkey: requireHexPubkey(requireTagValue(tags, "target_pubkey"), "target"),
    }
  }
  if (op === "set_key_capabilities") {
    return {
      op,
      pubkey: requireHexPubkey(requireTagValue(tags, "target_pubkey"), "target"),
      capabilities: new Set(tagValues(tags, "capability")),
    }
  }
  if (op === "rotate_secret_epoch" || op === "repair_secret_wraps") {
    return { op: "ignore" }
  }
  throw new Error(`Unsupported NostrIdentity roster op ${op}`)
}

function canonicalProfileId(profileId: string): string {
  const normalized = profileId.trim().toLowerCase()
  if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(normalized)) {
    throw new Error("NostrIdentity id must be a UUID")
  }
  return normalized
}

export function createNostrIdentityProfileId(): string {
  if (typeof crypto === "undefined" || !crypto.getRandomValues) {
    throw new Error("Secure random source is not available")
  }
  if (crypto.randomUUID) {
    return crypto.randomUUID()
  }
  const bytes = new Uint8Array(16)
  crypto.getRandomValues(bytes)
  bytes[6] = (bytes[6] & 0x0f) | 0x40
  bytes[8] = (bytes[8] & 0x3f) | 0x80
  const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("")
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`
}

function normalizeAppKeysEventOptions(
  input?: Uint8Array | AppKeysEventOptions
): Required<Pick<AppKeysEventOptions, "createdAt" | "heads">>
  & Omit<AppKeysEventOptions, "createdAt" | "heads"> {
  if (input instanceof Uint8Array) {
    return {
      ownerPrivateKey: input,
      ownerPubkey: getPublicKey(input),
      profileId: undefined,
      createdAt: now(),
      heads: [],
    }
  }
  const ownerPrivateKey = input?.ownerPrivateKey
  return {
    ownerPrivateKey,
    ownerPubkey: input?.ownerPubkey ?? (ownerPrivateKey ? getPublicKey(ownerPrivateKey) : undefined),
    profileId: input?.profileId,
    createdAt: input?.createdAt ?? now(),
    heads: input?.heads ?? [],
  }
}

function factTag(predicate: string, ...values: string[]): string[] {
  return [predicate, ...values]
}

function canonicalizeSnapshotTags(tags: string[][]): string[][] {
  const unique = new Map(tags.map((tag) => [JSON.stringify(tag), tag]))
  return [...unique.values()].sort((left, right) => {
    const len = Math.max(left.length, right.length)
    for (let index = 0; index < len; index += 1) {
      const diff = (left[index] ?? "").localeCompare(right[index] ?? "")
      if (diff !== 0) return diff
    }
    return 0
  })
}

function buildAppKeysFactSnapshotTags(
  profileId: string,
  facts: string[][],
  heads: string[] = [],
): string[][] {
  const pubkeys = new Set<string>()
  for (const fact of facts) {
    for (const value of fact.slice(1)) {
      const pubkey = normalizeHexPubkey(value)
      if (pubkey) pubkeys.add(pubkey)
    }
  }
  return canonicalizeSnapshotTags([
    ["d", profileId],
    ["i", profileId, "subject"],
    ...normalizeEventIds(heads).map((head) => ["e", head, "", "head"]),
    ...[...pubkeys].sort().map((pubkey) => ["p", pubkey]),
    ...facts,
  ])
}

export function isNostrIdentityRosterSnapshotEvent(
  event: Pick<VerifiedEvent, "kind" | "tags">
): boolean {
  return event.kind === NOSTR_IDENTITY_ROSTER_SNAPSHOT_KIND
    && tagValues(event.tags, "type").includes(NOSTR_IDENTITY_ROSTER_SNAPSHOT_TYPE)
}

function isAppKeyFacet(facet: NostrIdentityAppKeyFacet): boolean {
  return facet.purposes.has(NOSTR_IDENTITY_APP_PURPOSE)
    && facet.capabilities.has(NOSTR_IDENTITY_WRITE_CAPABILITY)
}

export function projectNostrIdentityRosterEvents(
  profileId: string,
  events: VerifiedEvent[]
): DeviceEntry[] {
  const targetProfileId = canonicalProfileId(profileId)
  const activeFacets = new Map<string, NostrIdentityAppKeyFacet>()
  const sorted = events
    .slice()
    .sort((left, right) => (
      left.created_at - right.created_at
      || left.id.localeCompare(right.id)
    ))

  for (const event of sorted) {
    let signed: SignedNostrIdentityRosterOp
    try {
      signed = parseNostrIdentityRosterOpEvent(event)
    } catch {
      continue
    }
    if (signed.profileId !== targetProfileId) continue

    const op = signed.op
    const signerFacet = activeFacets.get(signed.signerPubkey)
    const isBootstrap = activeFacets.size === 0
      && op.op === "add_key"
      && op.key.pubkey === signed.signerPubkey
      && op.key.capabilities.has(NOSTR_IDENTITY_ADMIN_CAPABILITY)
    const canAdmin = isBootstrap
      || Boolean(signerFacet?.capabilities.has(NOSTR_IDENTITY_ADMIN_CAPABILITY))
    if (!canAdmin) continue

    if (op.op === "add_key") {
      activeFacets.set(op.key.pubkey, {
        pubkey: op.key.pubkey,
        purposes: new Set(op.key.purposes),
        capabilities: new Set(op.key.capabilities),
        addedAt: op.key.addedAt,
      })
    } else if (op.op === "tombstone_key") {
      activeFacets.delete(op.pubkey)
    } else if (op.op === "set_key_capabilities") {
      const facet = activeFacets.get(op.pubkey)
      if (facet) {
        activeFacets.set(op.pubkey, {
          ...facet,
          capabilities: new Set(op.capabilities),
        })
      }
    }
  }

  return Array.from(activeFacets.values())
    .filter(isAppKeyFacet)
    .sort((left, right) => left.addedAt - right.addedAt || left.pubkey.localeCompare(right.pubkey))
    .map((facet) => ({
      identityPubkey: facet.pubkey,
      createdAt: facet.addedAt,
    }))
}

/**
 * Manages a consolidated list of device invites (kind 30078, d-tag "double-ratchet/app-keys").
 * Single atomic event containing all device invites for a user.
 * Uses union merge strategy for conflict resolution.
 *
 * Note: ownerPublicKey is not stored - it's passed to getEvent() when publishing,
 * and NDK's signer sets the correct pubkey during signing anyway.
 */
export class AppKeys {
  private devices: Map<string, DeviceEntry> = new Map()
  private deviceLabels: Map<string, DeviceLabels> = new Map()

  constructor(devices: DeviceEntry[] = [], deviceLabels: DeviceLabelsEntry[] = []) {
    devices.forEach((device) => this.devices.set(device.identityPubkey, device))
    deviceLabels.forEach(({ identityPubkey, ...labels }) => {
      this.deviceLabels.set(identityPubkey, labels)
    })
  }

  /**
   * Creates a new device identity entry.
   * Note: This only creates the identity entry. The device must separately
   * create and publish its own Invite event with ephemeral keys.
   */
  createDeviceEntry(identityPubkey: string): DeviceEntry {
    return {
      identityPubkey,
      createdAt: now(),
    }
  }

  addDevice(device: DeviceEntry): void {
    if (!this.devices.has(device.identityPubkey)) {
      this.devices.set(device.identityPubkey, device)
    }
  }

  removeDevice(identityPubkey: string): void {
    this.devices.delete(identityPubkey)
    this.deviceLabels.delete(identityPubkey)
  }

  getDevice(identityPubkey: string): DeviceEntry | undefined {
    return this.devices.get(identityPubkey)
  }

  getAllDevices(): DeviceEntry[] {
    return Array.from(this.devices.values())
  }

  setDeviceLabels(
    identityPubkey: string,
    labels: {
      deviceLabel?: string
      clientLabel?: string
    },
    updatedAt = now()
  ): void {
    this.deviceLabels.set(identityPubkey, {
      deviceLabel: labels.deviceLabel,
      clientLabel: labels.clientLabel,
      updatedAt,
    })
  }

  getDeviceLabels(identityPubkey: string): DeviceLabels | undefined {
    return this.deviceLabels.get(identityPubkey)
  }

  getAllDeviceLabels(): DeviceLabelsEntry[] {
    return Array.from(this.deviceLabels.entries()).map(([identityPubkey, labels]) => ({
      identityPubkey,
      ...labels,
    }))
  }

  private getEncryptedContent(ownerPrivateKey?: Uint8Array): string {
    const deviceLabels = this.getAllDeviceLabels().filter(({ identityPubkey }) =>
      this.devices.has(identityPubkey)
    )

    if (deviceLabels.length === 0) {
      return ""
    }

    if (!ownerPrivateKey) {
      return ""
    }

    const ownerPublicKey = getPublicKey(ownerPrivateKey)
    const conversationKey = nip44.v2.utils.getConversationKey(
      ownerPrivateKey,
      ownerPublicKey
    )
    const plaintext: EncryptedAppKeysContent = {
      type: "app-keys-labels",
      v: 1,
      deviceLabels,
    }

    return nip44.v2.encrypt(JSON.stringify(plaintext), conversationKey)
  }

  private loadEncryptedContent(content: string, ownerPrivateKey: Uint8Array): void {
    if (!content) return

    const ownerPublicKey = getPublicKey(ownerPrivateKey)
    const conversationKey = nip44.v2.utils.getConversationKey(
      ownerPrivateKey,
      ownerPublicKey
    )
    const decrypted = nip44.v2.decrypt(content, conversationKey)
    const payload = JSON.parse(decrypted) as LegacyEncryptedAppKeysContent

    if (payload.type !== "app-keys-labels" || payload.v !== 1) {
      throw new Error("Unsupported AppKeys label payload")
    }

    const rawLabelEntries = Array.isArray(payload.deviceLabels)
      ? payload.deviceLabels
      : Array.isArray(payload.device_labels) ? payload.device_labels : []
    const labelEntries = rawLabelEntries
      .map(normalizeDeviceLabelsEntry)
      .filter((entry): entry is DeviceLabelsEntry => entry !== null)

    this.deviceLabels.clear()
    labelEntries.forEach(({ identityPubkey, ...labels }) => {
      if (this.devices.has(identityPubkey)) {
        this.deviceLabels.set(identityPubkey, labels)
      }
    })
  }

  getEvent(options?: Uint8Array | AppKeysEventOptions): UnsignedEvent {
    const normalized = normalizeAppKeysEventOptions(options)
    const profileId = canonicalProfileId(normalized.profileId ?? createNostrIdentityProfileId())
    const ownerPubkey = normalized.ownerPubkey
      ? requireHexPubkey(normalized.ownerPubkey, "owner")
      : undefined
    const facts = [
      factTag("type", NOSTR_IDENTITY_ROSTER_SNAPSHOT_TYPE),
      factTag("schema", String(NOSTR_IDENTITY_ROSTER_SCHEMA)),
      ...(ownerPubkey ? [factTag(NOSTR_IDENTITY_OWNER_PUBKEY_FACT, ownerPubkey)] : []),
      ...this.getAllDevices()
        .slice()
        .sort((left, right) => (
          left.createdAt - right.createdAt
          || left.identityPubkey.localeCompare(right.identityPubkey)
        ))
        .map((device) => factTag(
          "device",
          device.identityPubkey.trim().toLowerCase(),
          String(device.createdAt),
        )),
    ]
    const encryptedLabels = this.getEncryptedContent(normalized.ownerPrivateKey)
    if (encryptedLabels) {
      facts.push(factTag(NOSTR_IDENTITY_ENCRYPTED_DEVICE_LABELS_FACT, encryptedLabels))
    }

    return {
      kind: NOSTR_IDENTITY_ROSTER_SNAPSHOT_KIND,
      pubkey: "", // Signer will set this
      content: "",
      created_at: normalized.createdAt,
      tags: buildAppKeysFactSnapshotTags(profileId, facts, normalized.heads),
    }
  }

  static fromEvent(event: VerifiedEvent, ownerPrivateKey?: Uint8Array): AppKeys {
    if (!event.sig) {
      throw new Error("Event is not signed")
    }
    if (!verifyEvent(event)) {
      throw new Error("Event signature is invalid")
    }
    if (!isNostrIdentityRosterSnapshotEvent(event)) {
      throw new Error("Event is not a NostrIdentity roster snapshot")
    }
    if (event.content !== "") {
      throw new Error("NostrIdentity roster snapshot content must be empty")
    }
    const schema = requireInteger(requireTagValue(event.tags, "schema"), "schema")
    if (schema !== NOSTR_IDENTITY_ROSTER_SCHEMA) {
      throw new Error(`Unsupported NostrIdentity roster schema ${schema}`)
    }
    const ownerPubkey = firstTagValue(event.tags, NOSTR_IDENTITY_OWNER_PUBKEY_FACT)
    if (ownerPubkey && requireHexPubkey(ownerPubkey, "owner") !== event.pubkey) {
      throw new Error("NostrIdentity roster owner signer mismatch")
    }
    profileIdFromTags(event.tags)

    const devices = event.tags
      .filter(isDeviceTag)
      .map(([, identityPubkey, createdAt]) => ({
        identityPubkey: identityPubkey.trim().toLowerCase(),
        createdAt: parseInt(createdAt, 10) || event.created_at,
      }))

    const appKeys = new AppKeys(devices)
    const encryptedLabels = firstTagValue(event.tags, NOSTR_IDENTITY_ENCRYPTED_DEVICE_LABELS_FACT)
    if (ownerPrivateKey && encryptedLabels) {
      appKeys.loadEncryptedContent(encryptedLabels, ownerPrivateKey)
    }

    return appKeys
  }

  static fromNostrIdentityRosterSnapshotEvent(
    event: VerifiedEvent,
    ownerPrivateKey?: Uint8Array
  ): ParsedAppKeysSnapshot {
    const appKeys = AppKeys.fromEvent(event, ownerPrivateKey)
    const ownerPubkey =
      firstTagValue(event.tags, NOSTR_IDENTITY_OWNER_PUBKEY_FACT) ?? event.pubkey
    return {
      profileId: profileIdFromTags(event.tags),
      ownerPubkey: requireHexPubkey(ownerPubkey, "owner"),
      appKeys,
      createdAt: event.created_at,
    }
  }

  static fromNostrIdentityRosterEvents(profileId: string, events: VerifiedEvent[]): AppKeys {
    return new AppKeys(projectNostrIdentityRosterEvents(profileId, events))
  }

  serialize(): string {
    return JSON.stringify({
      devices: this.getAllDevices(),
      deviceLabels: this.getAllDeviceLabels(),
    })
  }

  static deserialize(json: string): AppKeys {
    const data = JSON.parse(json) as {
      devices: DeviceEntry[]
      deviceLabels?: DeviceLabelsEntry[]
    }
    return new AppKeys(data.devices, data.deviceLabels || [])
  }

  merge(other: AppKeys): AppKeys {
    // Merge devices, preferring the one with earlier createdAt for same identityPubkey
    const mergedDevices = [...this.devices.values(), ...other.devices.values()]
      .reduce((map, device) => {
        const existing = map.get(device.identityPubkey)
        if (!existing || device.createdAt < existing.createdAt) {
          map.set(device.identityPubkey, device)
        }
        return map
      }, new Map<string, DeviceEntry>())

    const mergedLabels = [...this.deviceLabels.entries(), ...other.deviceLabels.entries()]
      .reduce((map, [identityPubkey, labels]) => {
        const existing = map.get(identityPubkey)
        if (!existing || labels.updatedAt > existing.updatedAt) {
          map.set(identityPubkey, labels)
        }
        return map
      }, new Map<string, DeviceLabels>())

    const mergedDeviceKeys = new Set(mergedDevices.keys())
    const deviceLabels = Array.from(mergedLabels.entries())
      .filter(([identityPubkey]) => mergedDeviceKeys.has(identityPubkey))
      .map(([identityPubkey, labels]) => ({
        identityPubkey,
        ...labels,
      }))

    return new AppKeys(Array.from(mergedDevices.values()), deviceLabels)
  }

  /**
   * Subscribe to AppKeys events from a user.
   * Similar to Invite.fromUser pattern.
   */
  static fromUser(
    user: string,
    subscribe: NostrSubscribe,
    onAppKeysList: (appKeys: AppKeys) => void,
    ownerPrivateKey?: Uint8Array
  ): Unsubscribe {
    return subscribe(
      buildAppKeysFilter(user),
      (event) => {
        if (event.pubkey !== user) return
        try {
          const appKeys = AppKeys.fromEvent(event, ownerPrivateKey)
          onAppKeysList(appKeys)
        } catch {
          // Invalid event
        }
      }
    )
  }

  /**
   * Wait for AppKeys from a user with timeout.
   * Returns the most recent AppKeys received within the timeout, or null.
   * Note: Uses the most recent event by created_at, not merging, since
   * device revocation is determined by absence from the list.
   */
  static waitFor(
    user: string,
    subscribe: NostrSubscribe,
    timeoutMs = 500,
    ownerPrivateKey?: Uint8Array
  ): Promise<AppKeys | null> {
    return AppKeys.waitForSnapshot(user, subscribe, timeoutMs, ownerPrivateKey).then(
      (snapshot) => snapshot?.appKeys ?? null
    )
  }

  static waitForSnapshot(
    user: string,
    subscribe: NostrSubscribe,
    timeoutMs = 500,
    ownerPrivateKey?: Uint8Array
  ): Promise<{ appKeys: AppKeys; createdAt: number } | null> {
    return new Promise((resolve) => {
      let latest: { list: AppKeys; createdAt: number } | null = null

      setTimeout(() => {
        unsubscribe()
        resolve(latest ? { appKeys: latest.list, createdAt: latest.createdAt } : null)
      }, timeoutMs)

      const unsubscribe = subscribe(
        buildAppKeysFilter(user),
        (event) => {
          if (event.pubkey !== user) return
          try {
            const list = AppKeys.fromEvent(event, ownerPrivateKey)
            const next = applyAppKeysSnapshot({
              currentAppKeys: latest?.list,
              currentCreatedAt: latest?.createdAt,
              incomingAppKeys: list,
              incomingCreatedAt: event.created_at,
            })
            if (next.decision === "stale") {
              return
            }
            latest = { list: next.appKeys, createdAt: next.createdAt }
          } catch {
            // Invalid event
          }
        }
      )
    })
  }
}
