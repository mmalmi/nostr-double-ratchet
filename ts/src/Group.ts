import { bytesToHex } from "@noble/hashes/utils";

export const GROUP_METADATA_KIND = 40
export const GROUP_INVITE_RUMOR_KIND = 10445
export const GROUP_SENDER_KEY_DISTRIBUTION_KIND = 10446
export const GROUP_SENDER_KEY_MESSAGE_KIND = 10447

export interface GroupData {
  id: string
  name: string
  description?: string
  picture?: string
  members: string[]
  admins: string[]
  createdAt: number
  secret?: string
  accepted?: boolean
}

export interface GroupMetadata {
  id: string
  name: string
  description?: string
  picture?: string
  members: string[]
  admins: string[]
  secret?: string
}

export function isGroupAdmin(group: GroupData, pubkey: string): boolean {
  return group.admins.includes(pubkey)
}

export function generateGroupSecret(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(32))
  return bytesToHex(bytes)
}

export function createGroupData(name: string, creatorPubkey: string, memberPubkeys: string[]): GroupData {
  const allMembers = [creatorPubkey, ...memberPubkeys.filter(p => p !== creatorPubkey)]
  return {
    id: crypto.randomUUID(),
    name,
    members: allMembers,
    admins: [creatorPubkey],
    createdAt: Date.now(),
    secret: generateGroupSecret(),
    accepted: true
  }
}

export function buildGroupMetadataContent(group: GroupData, opts?: { excludeSecret?: boolean }): string {
  const metadata: GroupMetadata = {
    id: group.id,
    name: group.name,
    members: group.members,
    admins: group.admins,
    ...(group.description && { description: group.description }),
    ...(group.picture && { picture: group.picture }),
    ...(!opts?.excludeSecret && group.secret && { secret: group.secret })
  }
  return JSON.stringify(metadata)
}

export function parseGroupMetadata(content: string): GroupMetadata | null {
  try {
    const metadata = JSON.parse(content) as Partial<GroupMetadata>
    const { id, name, members, admins } = metadata
    if (!id || !name || !Array.isArray(members) || !Array.isArray(admins)) return null
    if (admins.length === 0) return null
    return metadata as GroupMetadata
  } catch {
    return null
  }
}

export function validateMetadataUpdate(
  existing: GroupData,
  metadata: GroupMetadata,
  senderPubkey: string,
  myPubkey: string
): 'accept' | 'reject' | 'removed' {
  if (!isGroupAdmin(existing, senderPubkey)) return 'reject'
  if (!metadata.members.includes(myPubkey)) return 'removed'
  return 'accept'
}

export function validateMetadataCreation(
  metadata: GroupMetadata,
  senderPubkey: string,
  myPubkey: string
): boolean {
  if (!metadata.admins.includes(senderPubkey)) return false
  if (!metadata.members.includes(myPubkey)) return false
  return true
}

export function applyMetadataUpdate(existing: GroupData, metadata: GroupMetadata): GroupData {
  return {
    ...existing,
    name: metadata.name,
    members: metadata.members,
    admins: metadata.admins,
    description: metadata.description,
    picture: metadata.picture,
    secret: metadata.secret || existing.secret
  }
}

export function addGroupMember(group: GroupData, pubkey: string, actorPubkey: string): GroupData | null {
  if (!isGroupAdmin(group, actorPubkey)) return null
  if (group.members.includes(pubkey)) return null
  return {
    ...group,
    members: [...group.members, pubkey],
    secret: generateGroupSecret()
  }
}

export function removeGroupMember(group: GroupData, pubkey: string, actorPubkey: string): GroupData | null {
  if (!isGroupAdmin(group, actorPubkey)) return null
  if (!group.members.includes(pubkey)) return null
  if (pubkey === actorPubkey) return null
  return {
    ...group,
    members: group.members.filter(m => m !== pubkey),
    admins: group.admins.filter(a => a !== pubkey),
    secret: generateGroupSecret()
  }
}

export function updateGroupData(
  group: GroupData,
  updates: { name?: string; description?: string; picture?: string },
  actorPubkey: string
): GroupData | null {
  if (!isGroupAdmin(group, actorPubkey)) return null
  const updated = { ...group }
  if (updates.name !== undefined) updated.name = updates.name
  if (updates.description !== undefined) updated.description = updates.description
  if (updates.picture !== undefined) updated.picture = updates.picture
  return updated
}

export function addGroupAdmin(group: GroupData, pubkey: string, actorPubkey: string): GroupData | null {
  if (!isGroupAdmin(group, actorPubkey)) return null
  if (!group.members.includes(pubkey)) return null
  if (group.admins.includes(pubkey)) return null
  return {
    ...group,
    admins: [...group.admins, pubkey]
  }
}

export function removeGroupAdmin(group: GroupData, pubkey: string, actorPubkey: string): GroupData | null {
  if (!isGroupAdmin(group, actorPubkey)) return null
  if (!group.admins.includes(pubkey)) return null
  if (group.admins.length <= 1) return null
  return {
    ...group,
    admins: group.admins.filter(a => a !== pubkey)
  }
}
