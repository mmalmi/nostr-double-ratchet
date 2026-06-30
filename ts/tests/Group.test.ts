import { describe, it, expect } from 'vitest'
import { generateSecretKey, getPublicKey, finalizeEvent } from 'nostr-tools'
import {
  isGroupAdmin,
  generateGroupSecret,
  createGroupData,
  validateMetadataUpdate,
  validateMetadataCreation,
  applyMetadataUpdate,
  addGroupMember,
  removeGroupMember,
  updateGroupData,
  addGroupAdmin,
  removeGroupAdmin,
  GROUP_INVITE_RUMOR_KIND,
  GROUP_FACT_KIND,
  GROUP_FACT_SNAPSHOT_KIND,
  GROUP_ROSTER_FACT_KIND,
  GROUP_ROSTER_FACT_TYPE,
  buildGroupRosterFactEvent,
  buildGroupRosterFactFilter,
  parseGroupRosterFactEvent,
  parseGroupRosterFactRumor,
  projectGroupRosterFactEvents,
  type GroupData,
  type GroupMetadata,
} from '../src/Group'

const ALICE = 'aaaa'.repeat(16)
const BOB = 'bbbb'.repeat(16)
const CAROL = 'cccc'.repeat(16)
const DAVE = 'dddd'.repeat(16)

function sortTags(tags: string[][]): string[][] {
  return [...tags].sort((left, right) => {
    const length = Math.max(left.length, right.length)
    for (let index = 0; index < length; index += 1) {
      const diff = (left[index] ?? '').localeCompare(right[index] ?? '')
      if (diff !== 0) return diff
    }
    return 0
  })
}

function makeGroup(overrides?: Partial<GroupData>): GroupData {
  return {
    id: 'test-group',
    name: 'Test',
    members: [ALICE, BOB],
    admins: [ALICE],
    createdAt: Date.now(),
    secret: 'a'.repeat(64),
    accepted: true,
    ...overrides
  }
}

describe('Group constants', () => {
  it('GROUP_INVITE_RUMOR_KIND is 10445', () => {
    expect(GROUP_INVITE_RUMOR_KIND).toBe(10445)
  })

  it('group roster facts use the fact snapshot kind', () => {
    expect(GROUP_FACT_KIND).toBe(7368)
    expect(GROUP_FACT_SNAPSHOT_KIND).toBe(37368)
    expect(GROUP_ROSTER_FACT_KIND).toBe(GROUP_FACT_SNAPSHOT_KIND)
    expect(GROUP_ROSTER_FACT_TYPE).toBe('group_roster')
  })
})

describe('Group roster fact helpers', () => {
  it('builds and parses deterministic group roster facts around GroupData', () => {
    const adminSecret = generateSecretKey()
    const admin = getPublicKey(adminSecret)
    const bobSecret = generateSecretKey()
    const bob = getPublicKey(bobSecret)
    const carolSecret = generateSecretKey()
    const carol = getPublicKey(carolSecret)
    const group = makeGroup({
      id: 'group-facts',
      name: 'Fact Friends',
      description: 'tag-native roster',
      picture: 'https://example.test/group.png',
      members: [carol, admin, bob],
      admins: [admin, bob],
      createdAt: 1_700_000_000,
      secret: 'not-on-the-wire',
    })

    const unsigned = buildGroupRosterFactEvent(group, {
      signerPubkey: admin,
      revision: 4,
      createdBy: admin,
      updatedAt: 1_700_000_123,
      eventCreatedAt: 1_700_000_124,
    })
    expect(unsigned).toMatchObject({
      kind: GROUP_ROSTER_FACT_KIND,
      pubkey: admin,
      content: '',
      created_at: 1_700_000_124,
    })
    const expectedMembers = [admin, bob, carol].sort()
    const expectedAdmins = [admin, bob].sort()
    expect(unsigned.tags).toEqual(sortTags([
      ['d', 'group-facts'],
      ['i', 'group-facts', 'subject'],
      ['type', GROUP_ROSTER_FACT_TYPE],
      ['schema', '1'],
      ['group_id', 'group-facts'],
      ['revision', '4'],
      ['name', 'Fact Friends'],
      ['created_at', '1700000000'],
      ['updated_at', '1700000123'],
      ['created_by', admin],
      ['about', 'tag-native roster'],
      ['picture', 'https://example.test/group.png'],
      ...expectedMembers.map((member) => ['member', member]),
      ...expectedAdmins.map((groupAdmin) => ['admin', groupAdmin]),
    ]))
    expect(JSON.stringify(unsigned)).not.toContain('not-on-the-wire')

    const signed = finalizeEvent(unsigned, adminSecret)
    const parsed = parseGroupRosterFactEvent(signed)
    expect(parsed).toMatchObject({
      groupId: 'group-facts',
      revision: 4,
      signerPubkey: admin,
      createdBy: admin,
      updatedAt: 1_700_000_123,
      group: {
        id: 'group-facts',
        name: 'Fact Friends',
        description: 'tag-native roster',
        picture: 'https://example.test/group.png',
        members: expectedMembers,
        admins: expectedAdmins,
        createdAt: 1_700_000_000,
      },
    })
  })

  it('parses encrypted device-authored group roster fact rumors', () => {
    const admin = getPublicKey(generateSecretKey())
    const device = getPublicKey(generateSecretKey())
    const bob = getPublicKey(generateSecretKey())
    const group = makeGroup({
      id: 'group-rumor-fact',
      name: 'Device Fact',
      members: [admin, bob],
      admins: [admin],
      createdAt: 1_700_000_000,
    })

    const rumor = {
      ...buildGroupRosterFactEvent(group, {
        signerPubkey: device,
        revision: 7,
        createdBy: admin,
        updatedAt: 1_700_000_111,
        eventCreatedAt: 1_700_000_112,
      }),
      id: 'f'.repeat(64),
    }

    const parsed = parseGroupRosterFactRumor(rumor)
    expect(parsed.signerPubkey).toBe(device)
    expect(parsed.createdBy).toBe(admin)
    expect(parsed.group).toMatchObject({
      id: 'group-rumor-fact',
      name: 'Device Fact',
      members: [admin, bob].sort(),
      admins: [admin],
      createdAt: 1_700_000_000,
    })
  })

  it('builds filters and projects the newest fact per group by revision', () => {
    const adminSecret = generateSecretKey()
    const admin = getPublicKey(adminSecret)
    const bobSecret = generateSecretKey()
    const bob = getPublicKey(bobSecret)
    const base = makeGroup({
      id: 'group-facts',
      name: 'Old',
      members: [admin],
      admins: [admin],
      createdAt: 10,
    })
    const newer = makeGroup({
      id: 'group-facts',
      name: 'New',
      members: [admin, bob],
      admins: [admin],
      createdAt: 10,
    })

    expect(buildGroupRosterFactFilter({
      groupIds: 'group-facts',
      authors: admin,
      since: 99,
    })).toEqual({
      kinds: [GROUP_ROSTER_FACT_KIND],
      authors: [admin],
      '#d': ['group-facts'],
      since: 99,
    })

    const oldEvent = finalizeEvent(
      buildGroupRosterFactEvent(base, {
        signerPubkey: admin,
        revision: 1,
        updatedAt: 11,
        eventCreatedAt: 11,
      }),
      adminSecret
    )
    const newEvent = finalizeEvent(
      buildGroupRosterFactEvent(newer, {
        signerPubkey: admin,
        revision: 2,
        updatedAt: 12,
        eventCreatedAt: 12,
      }),
      adminSecret
    )

    expect(projectGroupRosterFactEvents([newEvent, oldEvent])).toEqual([
      expect.objectContaining({
        groupId: 'group-facts',
        revision: 2,
      group: expect.objectContaining({
        name: 'New',
        members: [admin, bob].sort(),
      }),
    }),
  ])
  })
})

describe('isGroupAdmin', () => {
  it('returns true for admin', () => {
    expect(isGroupAdmin(makeGroup(), ALICE)).toBe(true)
  })

  it('returns false for non-admin member', () => {
    expect(isGroupAdmin(makeGroup(), BOB)).toBe(false)
  })

  it('returns false for non-member', () => {
    expect(isGroupAdmin(makeGroup(), DAVE)).toBe(false)
  })
})

describe('generateGroupSecret', () => {
  it('returns a 64-char hex string', () => {
    const secret = generateGroupSecret()
    expect(secret).toHaveLength(64)
    expect(/^[0-9a-f]{64}$/.test(secret)).toBe(true)
  })

  it('generates unique secrets', () => {
    const a = generateGroupSecret()
    const b = generateGroupSecret()
    expect(a).not.toBe(b)
  })
})

describe('createGroupData', () => {
  it('creates group with creator as first member and sole admin', () => {
    const group = createGroupData('My Group', ALICE, [BOB, CAROL])
    expect(group.name).toBe('My Group')
    expect(group.members).toEqual([ALICE, BOB, CAROL])
    expect(group.admins).toEqual([ALICE])
    expect(group.accepted).toBe(true)
    expect(group.secret).toHaveLength(64)
    expect(group.id).toBeTruthy()
  })

  it('deduplicates creator from member list', () => {
    const group = createGroupData('Dedup', ALICE, [ALICE, BOB])
    const aliceCount = group.members.filter(m => m === ALICE).length
    expect(aliceCount).toBe(1)
  })
})

describe('validateMetadataUpdate', () => {
  it('accepts update from admin', () => {
    const group = makeGroup()
    const metadata: GroupMetadata = { id: group.id, name: 'New', members: [ALICE, BOB], admins: [ALICE] }
    expect(validateMetadataUpdate(group, metadata, ALICE, BOB)).toBe('accept')
  })

  it('rejects update from non-admin', () => {
    const group = makeGroup()
    const metadata: GroupMetadata = { id: group.id, name: 'Hack', members: [ALICE, BOB], admins: [BOB] }
    expect(validateMetadataUpdate(group, metadata, BOB, ALICE)).toBe('reject')
  })

  it('returns removed when myPubkey not in members', () => {
    const group = makeGroup()
    const metadata: GroupMetadata = { id: group.id, name: 'Kicked', members: [ALICE], admins: [ALICE] }
    expect(validateMetadataUpdate(group, metadata, ALICE, BOB)).toBe('removed')
  })
})

describe('validateMetadataCreation', () => {
  it('accepts when sender is in admins and myPubkey is in members', () => {
    const meta: GroupMetadata = { id: 'g1', name: 'G', members: [ALICE, BOB], admins: [ALICE] }
    expect(validateMetadataCreation(meta, ALICE, BOB)).toBe(true)
  })

  it('rejects when sender is not in admins', () => {
    const meta: GroupMetadata = { id: 'g1', name: 'G', members: [ALICE, BOB], admins: [ALICE] }
    expect(validateMetadataCreation(meta, BOB, BOB)).toBe(false)
  })

  it('rejects when myPubkey is not in members', () => {
    const meta: GroupMetadata = { id: 'g1', name: 'G', members: [ALICE], admins: [ALICE] }
    expect(validateMetadataCreation(meta, ALICE, BOB)).toBe(false)
  })
})

describe('applyMetadataUpdate', () => {
  it('updates fields from metadata while preserving accepted status', () => {
    const group = makeGroup({ accepted: true })
    const meta: GroupMetadata = {
      id: group.id, name: 'Updated', members: [ALICE, BOB, CAROL],
      admins: [ALICE], description: 'new desc'
    }
    const updated = applyMetadataUpdate(group, meta)
    expect(updated.name).toBe('Updated')
    expect(updated.members).toEqual([ALICE, BOB, CAROL])
    expect(updated.description).toBe('new desc')
    expect(updated.secret).toBe(group.secret)
    expect(updated.accepted).toBe(true)
  })

  it('keeps existing secret when metadata has none', () => {
    const group = makeGroup({ secret: 'original'.padEnd(64, '0') })
    const meta: GroupMetadata = { id: group.id, name: 'X', members: [ALICE], admins: [ALICE] }
    const updated = applyMetadataUpdate(group, meta)
    expect(updated.secret).toBe('original'.padEnd(64, '0'))
  })
})

describe('addGroupMember', () => {
  it('admin can add a member and secret rotates', () => {
    const group = makeGroup()
    const result = addGroupMember(group, CAROL, ALICE)
    expect(result).not.toBeNull()
    expect(result!.members).toContain(CAROL)
    expect(result!.secret).not.toBe(group.secret)
  })

  it('returns null if actor is not admin', () => {
    expect(addGroupMember(makeGroup(), CAROL, BOB)).toBeNull()
  })

  it('returns null if member already exists', () => {
    expect(addGroupMember(makeGroup(), BOB, ALICE)).toBeNull()
  })
})

describe('removeGroupMember', () => {
  it('admin can remove a member and secret rotates', () => {
    const group = makeGroup({ members: [ALICE, BOB, CAROL] })
    const result = removeGroupMember(group, CAROL, ALICE)
    expect(result).not.toBeNull()
    expect(result!.members).not.toContain(CAROL)
    expect(result!.secret).not.toBe(group.secret)
  })

  it('also strips admin status from removed member', () => {
    const group = makeGroup({ members: [ALICE, BOB], admins: [ALICE, BOB] })
    const result = removeGroupMember(group, BOB, ALICE)
    expect(result!.admins).not.toContain(BOB)
  })

  it('returns null if actor is not admin', () => {
    expect(removeGroupMember(makeGroup({ members: [ALICE, BOB, CAROL] }), CAROL, BOB)).toBeNull()
  })

  it('returns null if member not in group', () => {
    expect(removeGroupMember(makeGroup(), DAVE, ALICE)).toBeNull()
  })

  it('returns null if trying to remove self', () => {
    expect(removeGroupMember(makeGroup(), ALICE, ALICE)).toBeNull()
  })
})

describe('updateGroupData', () => {
  it('admin can update name', () => {
    const result = updateGroupData(makeGroup(), { name: 'New Name' }, ALICE)
    expect(result!.name).toBe('New Name')
  })

  it('admin can update description', () => {
    const result = updateGroupData(makeGroup(), { description: 'new desc' }, ALICE)
    expect(result!.description).toBe('new desc')
  })

  it('admin can update picture', () => {
    const result = updateGroupData(makeGroup(), { picture: 'pic.jpg' }, ALICE)
    expect(result!.picture).toBe('pic.jpg')
  })

  it('returns null if actor is not admin', () => {
    expect(updateGroupData(makeGroup(), { name: 'Hack' }, BOB)).toBeNull()
  })
})

describe('addGroupAdmin', () => {
  it('admin can promote a member', () => {
    const result = addGroupAdmin(makeGroup(), BOB, ALICE)
    expect(result!.admins).toContain(BOB)
  })

  it('returns null if actor is not admin', () => {
    expect(addGroupAdmin(makeGroup(), CAROL, BOB)).toBeNull()
  })

  it('returns null if target is not a member', () => {
    expect(addGroupAdmin(makeGroup(), DAVE, ALICE)).toBeNull()
  })

  it('returns null if target is already admin', () => {
    expect(addGroupAdmin(makeGroup(), ALICE, ALICE)).toBeNull()
  })
})

describe('removeGroupAdmin', () => {
  it('admin can demote another admin', () => {
    const group = makeGroup({ admins: [ALICE, BOB] })
    const result = removeGroupAdmin(group, BOB, ALICE)
    expect(result!.admins).not.toContain(BOB)
  })

  it('returns null if actor is not admin', () => {
    const group = makeGroup({ admins: [ALICE, BOB] })
    expect(removeGroupAdmin(group, ALICE, CAROL)).toBeNull()
  })

  it('returns null if target is not admin', () => {
    expect(removeGroupAdmin(makeGroup(), BOB, ALICE)).toBeNull()
  })

  it('returns null if would remove last admin', () => {
    expect(removeGroupAdmin(makeGroup(), ALICE, ALICE)).toBeNull()
  })
})
