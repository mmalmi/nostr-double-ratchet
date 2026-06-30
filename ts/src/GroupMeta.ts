import { bytesToHex } from "@noble/hashes/utils";
import { Filter, UnsignedEvent, VerifiedEvent, verifyEvent } from "nostr-tools";

export const GROUP_FACT_KIND = 7368;
export const GROUP_FACT_SNAPSHOT_KIND = 37368;
export const GROUP_ROSTER_FACT_KIND = GROUP_FACT_SNAPSHOT_KIND;
export const GROUP_ROSTER_FACT_TYPE = "group_roster";
export const GROUP_ROSTER_FACT_SCHEMA = 1;
export const GROUP_METADATA_KIND = 40;
export const GROUP_INVITE_RUMOR_KIND = 10445;
export const GROUP_SENDER_KEY_DISTRIBUTION_KIND = 10446;
export const GROUP_SENDER_KEY_REPAIR_REQUEST_KIND = 10447;
/**
 * @deprecated 10447 is the sender-key repair request rumor kind. Group sender-key
 * outer events use MESSAGE_EVENT_KIND.
 */
export const GROUP_SENDER_KEY_MESSAGE_KIND = GROUP_SENDER_KEY_REPAIR_REQUEST_KIND;

export interface GroupData {
  id: string;
  name: string;
  description?: string;
  picture?: string;
  members: string[];
  admins: string[];
  createdAt: number;
  secret?: string;
  accepted?: boolean;
}

export interface GroupMetadata {
  id: string;
  name: string;
  description?: string;
  picture?: string;
  members: string[];
  admins: string[];
  secret?: string;
}

export interface GroupRosterFactFilterOptions {
  groupIds?: string | string[];
  authors?: string | string[];
  since?: number;
  until?: number;
  limit?: number;
}

export interface BuildGroupRosterFactOptions {
  signerPubkey: string;
  revision: number;
  createdBy?: string;
  updatedAt?: number;
  eventCreatedAt?: number;
  protocol?: "pairwise_fanout_v1" | "sender_key_v1";
}

export interface GroupRosterFact {
  eventId: string;
  signerPubkey: string;
  groupId: string;
  revision: number;
  createdBy: string;
  updatedAt: number;
  eventCreatedAt: number;
  protocol?: "pairwise_fanout_v1" | "sender_key_v1";
  group: GroupData;
}

export interface GroupRosterFactRumor {
  id: string;
  pubkey: string;
  kind: number;
  created_at: number;
  tags: string[][];
  content: string;
}

export function isGroupAdmin(group: GroupData, pubkey: string): boolean {
  return group.admins.includes(pubkey);
}

export function generateGroupSecret(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(32));
  return bytesToHex(bytes);
}

/**
 * Build local group state only (pure function).
 *
 * This does not perform any network I/O and does not send metadata to members.
 * Use `GroupManager.createGroup(...)` when you want create + metadata fanout in one step.
 */
export function createGroupData(
  name: string,
  creatorPubkey: string,
  memberPubkeys: string[]
): GroupData {
  const allMembers = [creatorPubkey, ...memberPubkeys.filter((p) => p !== creatorPubkey)];
  return {
    id: crypto.randomUUID(),
    name,
    members: allMembers,
    admins: [creatorPubkey],
    createdAt: Date.now(),
    secret: generateGroupSecret(),
    accepted: true,
  };
}

export function buildGroupMetadataContent(
  group: GroupData,
  opts?: { excludeSecret?: boolean }
): string {
  const metadata: GroupMetadata = {
    id: group.id,
    name: group.name,
    members: group.members,
    admins: group.admins,
    ...(group.description && { description: group.description }),
    ...(group.picture && { picture: group.picture }),
    ...(!opts?.excludeSecret && group.secret && { secret: group.secret }),
  };
  return JSON.stringify(metadata);
}

export function parseGroupMetadata(content: string): GroupMetadata | null {
  try {
    const metadata = JSON.parse(content) as Partial<GroupMetadata>;
    const { id, name, members, admins } = metadata;
    if (!id || !name || !Array.isArray(members) || !Array.isArray(admins)) return null;
    if (admins.length === 0) return null;
    return metadata as GroupMetadata;
  } catch {
    return null;
  }
}

export function validateMetadataUpdate(
  existing: GroupData,
  metadata: GroupMetadata,
  senderPubkey: string,
  myPubkey: string
): "accept" | "reject" | "removed" {
  if (!isGroupAdmin(existing, senderPubkey)) return "reject";
  if (!metadata.members.includes(myPubkey)) return "removed";
  return "accept";
}

export function validateMetadataCreation(
  metadata: GroupMetadata,
  senderPubkey: string,
  myPubkey: string
): boolean {
  if (!metadata.admins.includes(senderPubkey)) return false;
  if (!metadata.members.includes(myPubkey)) return false;
  return true;
}

export function applyMetadataUpdate(existing: GroupData, metadata: GroupMetadata): GroupData {
  return {
    ...existing,
    name: metadata.name,
    members: metadata.members,
    admins: metadata.admins,
    description: metadata.description,
    picture: metadata.picture,
    secret: metadata.secret || existing.secret,
  };
}

export function addGroupMember(
  group: GroupData,
  pubkey: string,
  actorPubkey: string
): GroupData | null {
  if (!isGroupAdmin(group, actorPubkey)) return null;
  if (group.members.includes(pubkey)) return null;
  return {
    ...group,
    members: [...group.members, pubkey],
    secret: generateGroupSecret(),
  };
}

export function removeGroupMember(
  group: GroupData,
  pubkey: string,
  actorPubkey: string
): GroupData | null {
  if (!isGroupAdmin(group, actorPubkey)) return null;
  if (!group.members.includes(pubkey)) return null;
  if (pubkey === actorPubkey) return null;
  return {
    ...group,
    members: group.members.filter((m) => m !== pubkey),
    admins: group.admins.filter((a) => a !== pubkey),
    secret: generateGroupSecret(),
  };
}

export function updateGroupData(
  group: GroupData,
  updates: { name?: string; description?: string; picture?: string },
  actorPubkey: string
): GroupData | null {
  if (!isGroupAdmin(group, actorPubkey)) return null;
  const updated = { ...group };
  if (updates.name !== undefined) updated.name = updates.name;
  if (updates.description !== undefined) updated.description = updates.description;
  if (updates.picture !== undefined) updated.picture = updates.picture;
  return updated;
}

export function addGroupAdmin(
  group: GroupData,
  pubkey: string,
  actorPubkey: string
): GroupData | null {
  if (!isGroupAdmin(group, actorPubkey)) return null;
  if (!group.members.includes(pubkey)) return null;
  if (group.admins.includes(pubkey)) return null;
  return {
    ...group,
    admins: [...group.admins, pubkey],
  };
}

export function removeGroupAdmin(
  group: GroupData,
  pubkey: string,
  actorPubkey: string
): GroupData | null {
  if (!isGroupAdmin(group, actorPubkey)) return null;
  if (!group.admins.includes(pubkey)) return null;
  if (group.admins.length <= 1) return null;
  return {
    ...group,
    admins: group.admins.filter((a) => a !== pubkey),
  };
}

export function buildGroupRosterFactFilter(
  options: GroupRosterFactFilterOptions = {}
): Filter {
  const filter: Filter = {
    kinds: [GROUP_ROSTER_FACT_KIND],
  };
  const groupIds = normalizeStringList(options.groupIds);
  if (groupIds.length > 0) filter["#d"] = groupIds;
  const authors = normalizeStringList(options.authors).map((author) =>
    requireHexPubkey(author, "author")
  );
  if (authors.length > 0) filter.authors = authors;
  if (options.since !== undefined) filter.since = options.since;
  if (options.until !== undefined) filter.until = options.until;
  if (options.limit !== undefined) filter.limit = options.limit;
  return filter;
}

export function buildGroupRosterFactEvent(
  group: GroupData,
  options: BuildGroupRosterFactOptions
): UnsignedEvent {
  const groupId = requireNonEmpty(group.id, "group id");
  const signerPubkey = requireHexPubkey(options.signerPubkey, "signer");
  const eventCreatedAt = options.eventCreatedAt ?? Math.round(Date.now() / 1000);
  const revision = requireNonNegativeInteger(options.revision, "revision");
  const createdAt = unixSecondsFromTimestamp(group.createdAt, "created_at");
  const updatedAt = requireNonNegativeInteger(
    options.updatedAt ?? eventCreatedAt,
    "updated_at"
  );
  const createdBy = requireHexPubkey(options.createdBy ?? signerPubkey, "created_by");
  const members = canonicalPubkeys(group.members, "member");
  const admins = canonicalPubkeys(group.admins, "admin");
  requireAdminsAreMembers(admins, members);

  const tags: string[][] = [
    ["d", groupId],
    ["i", groupId, "subject"],
    ["type", GROUP_ROSTER_FACT_TYPE],
    ["schema", String(GROUP_ROSTER_FACT_SCHEMA)],
    ["group_id", groupId],
    ["revision", String(revision)],
    ["name", requireNonEmpty(group.name, "name")],
    ["created_at", String(createdAt)],
    ["updated_at", String(updatedAt)],
    ["created_by", createdBy],
  ];
  if (options.protocol) tags.push(["protocol", options.protocol]);
  if (group.description) tags.push(["about", group.description]);
  if (group.picture) tags.push(["picture", group.picture]);
  for (const member of members) tags.push(["member", member]);
  for (const admin of admins) tags.push(["admin", admin]);

  return {
    kind: GROUP_ROSTER_FACT_KIND,
    pubkey: signerPubkey,
    created_at: eventCreatedAt,
    tags: canonicalizeTags(tags),
    content: "",
  };
}

export function isGroupRosterFactEvent(
  event: Pick<VerifiedEvent, "kind" | "tags">
): boolean {
  return event.kind === GROUP_ROSTER_FACT_KIND
    && tagValues(event.tags, "type").includes(GROUP_ROSTER_FACT_TYPE);
}

export function parseGroupRosterFactEvent(event: VerifiedEvent): GroupRosterFact {
  if (!verifyEvent(event)) {
    throw new Error("GroupRoster fact signature is invalid");
  }
  const fact = parseGroupRosterFactWire(event);
  if (!fact.group.admins.includes(fact.signerPubkey)) {
    throw new Error("GroupRoster signer must be an admin");
  }
  return fact;
}

export function parseGroupRosterFactRumor(event: GroupRosterFactRumor): GroupRosterFact {
  return parseGroupRosterFactWire(event);
}

function parseGroupRosterFactWire(
  event: Pick<
    GroupRosterFactRumor,
    "id" | "pubkey" | "kind" | "tags" | "content" | "created_at"
  >
): GroupRosterFact {
  if (!isGroupRosterFactEvent(event)) {
    throw new Error("Event is not a GroupRoster fact");
  }
  if (event.content !== "") {
    throw new Error("GroupRoster fact event content must be empty");
  }
  const schema = requireTagInteger(event.tags, "schema");
  if (schema !== GROUP_ROSTER_FACT_SCHEMA) {
    throw new Error(`Unsupported GroupRoster fact schema ${schema}`);
  }
  const groupId = groupIdFromTags(event.tags);
  const taggedGroupId = firstTagValue(event.tags, "group_id");
  if (taggedGroupId && taggedGroupId !== groupId) {
    throw new Error("GroupRoster group_id/subject tag mismatch");
  }
  const members = canonicalPubkeys(tagValues(event.tags, "member"), "member");
  const admins = canonicalPubkeys(tagValues(event.tags, "admin"), "admin");
  requireAdminsAreMembers(admins, members);
  const signerPubkey = requireHexPubkey(event.pubkey, "signer");
  const about = firstTagValue(event.tags, "about")
    ?? firstTagValue(event.tags, "description");
  const picture = firstTagValue(event.tags, "picture");
  const protocol = parseGroupRosterProtocol(firstTagValue(event.tags, "protocol"));
  const group: GroupData = {
    id: groupId,
    name: requireTagValue(event.tags, "name"),
    members,
    admins,
    createdAt: requireTagInteger(event.tags, "created_at"),
    ...(about && { description: about }),
    ...(picture && { picture }),
  };

  return {
    eventId: event.id,
    signerPubkey,
    groupId,
    revision: requireTagInteger(event.tags, "revision"),
    createdBy: requireHexPubkey(requireTagValue(event.tags, "created_by"), "created_by"),
    updatedAt: requireTagInteger(event.tags, "updated_at"),
    eventCreatedAt: event.created_at,
    ...(protocol && { protocol }),
    group,
  };
}

export function projectGroupRosterFactEvents(events: VerifiedEvent[]): GroupRosterFact[] {
  const byGroup = new Map<string, GroupRosterFact>();
  for (const event of events) {
    let fact: GroupRosterFact;
    try {
      fact = parseGroupRosterFactEvent(event);
    } catch {
      continue;
    }
    const existing = byGroup.get(fact.groupId);
    if (!existing || compareGroupRosterFacts(fact, existing) > 0) {
      byGroup.set(fact.groupId, fact);
    }
  }
  return Array.from(byGroup.values()).sort((left, right) =>
    left.groupId.localeCompare(right.groupId)
  );
}

function compareGroupRosterFacts(left: GroupRosterFact, right: GroupRosterFact): number {
  return left.revision - right.revision
    || left.updatedAt - right.updatedAt
    || left.eventCreatedAt - right.eventCreatedAt
    || left.eventId.localeCompare(right.eventId);
}

function normalizeStringList(value: string | string[] | undefined): string[] {
  if (Array.isArray(value)) {
    return value.map((item) => item.trim()).filter(Boolean);
  }
  return value?.trim() ? [value.trim()] : [];
}

function tagValues(tags: string[][], name: string): string[] {
  return tags
    .filter((tag) => tag[0] === name)
    .map((tag) => tag[1]?.trim() ?? "")
    .filter(Boolean);
}

function canonicalizeTags(tags: string[][]): string[][] {
  const unique = new Map(tags.map((tag) => [tagKey(tag), tag]));
  return Array.from(unique.values()).sort(compareTags);
}

function compareTags(left: string[], right: string[]): number {
  const length = Math.max(left.length, right.length);
  for (let index = 0; index < length; index += 1) {
    const diff = (left[index] ?? "").localeCompare(right[index] ?? "");
    if (diff !== 0) return diff;
  }
  return 0;
}

function tagKey(tag: string[]): string {
  return JSON.stringify(tag);
}

function firstTagValue(tags: string[][], name: string): string | undefined {
  return tagValues(tags, name)[0];
}

function requireTagValue(tags: string[][], name: string): string {
  const value = firstTagValue(tags, name);
  if (!value) throw new Error(`GroupRoster fact missing ${name}`);
  return value;
}

function requireTagInteger(tags: string[][], name: string): number {
  return requireNonNegativeInteger(requireTagValue(tags, name), name);
}

function requireNonEmpty(value: string, label: string): string {
  const trimmed = value.trim();
  if (!trimmed) throw new Error(`GroupRoster ${label} must not be empty`);
  return trimmed;
}

function requireNonNegativeInteger(value: string | number, label: string): number {
  const raw = typeof value === "number" ? String(value) : value.trim();
  if (!/^\d+$/.test(raw)) throw new Error(`GroupRoster ${label} must be an integer`);
  const parsed = Number(raw);
  if (!Number.isSafeInteger(parsed)) throw new Error(`GroupRoster ${label} is too large`);
  return parsed;
}

function unixSecondsFromTimestamp(value: number, label: string): number {
  const parsed = requireNonNegativeInteger(value, label);
  return parsed > 10_000_000_000 ? Math.floor(parsed / 1000) : parsed;
}

function requireHexPubkey(value: string, label: string): string {
  const normalized = value.trim().toLowerCase();
  if (!/^[0-9a-f]{64}$/.test(normalized)) {
    throw new Error(`GroupRoster ${label} pubkey must be 64-char hex`);
  }
  return normalized;
}

function canonicalPubkeys(values: string[], label: string): string[] {
  return Array.from(new Set(values.map((value) => requireHexPubkey(value, label)))).sort();
}

function requireAdminsAreMembers(admins: string[], members: string[]): void {
  if (admins.length === 0) throw new Error("GroupRoster admins must not be empty");
  const memberSet = new Set(members);
  if (admins.some((admin) => !memberSet.has(admin))) {
    throw new Error("GroupRoster admins must also be members");
  }
}

function groupIdFromTags(tags: string[][]): string {
  const subjects = tags
    .filter((tag) => tag[0] === "i" && tag[2] === "subject")
    .map((tag) => tag[1]?.trim() ?? "")
    .filter(Boolean);
  if (subjects.length !== 1) {
    throw new Error("GroupRoster fact must have exactly one subject i tag");
  }
  const groupId = subjects[0];
  const d = requireTagValue(tags, "d");
  if (d !== groupId) {
    throw new Error("GroupRoster d/subject tag mismatch");
  }
  return groupId;
}

function parseGroupRosterProtocol(
  value: string | undefined
): GroupRosterFact["protocol"] {
  if (value === undefined) return undefined;
  if (value === "pairwise_fanout_v1" || value === "sender_key_v1") return value;
  throw new Error(`Unsupported GroupRoster protocol ${value}`);
}
