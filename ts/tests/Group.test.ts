import { describe, it, expect } from 'vitest'
import {
  isGroupAdmin,
  generateGroupSecret,
  createGroupData,
  buildGroupMetadataContent,
  parseGroupMetadata,
  validateMetadataUpdate,
  validateMetadataCreation,
  applyMetadataUpdate,
  addGroupMember,
  removeGroupMember,
  updateGroupData,
  addGroupAdmin,
  removeGroupAdmin,
  GROUP_METADATA_KIND,
  GROUP_INVITE_RUMOR_KIND,
  type GroupData,
  type GroupMetadata,
} from '../src/Group'

const ALICE = 'aaaa'.repeat(16)
const BOB = 'bbbb'.repeat(16)
const CAROL = 'cccc'.repeat(16)
const DAVE = 'dddd'.repeat(16)

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
  it('GROUP_METADATA_KIND is 40', () => {
    expect(GROUP_METADATA_KIND).toBe(40)
  })

  it('GROUP_INVITE_RUMOR_KIND is 10445', () => {
    expect(GROUP_INVITE_RUMOR_KIND).toBe(10445)
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

describe('buildGroupMetadataContent', () => {
  it('serializes group metadata to JSON', () => {
    const group = makeGroup({ description: 'desc', picture: 'pic.jpg' })
    const json = buildGroupMetadataContent(group)
    const parsed = JSON.parse(json)
    expect(parsed.id).toBe(group.id)
    expect(parsed.name).toBe(group.name)
    expect(parsed.members).toEqual(group.members)
    expect(parsed.admins).toEqual(group.admins)
    expect(parsed.description).toBe('desc')
    expect(parsed.picture).toBe('pic.jpg')
    expect(parsed.secret).toBe(group.secret)
  })

  it('excludes secret when excludeSecret is true', () => {
    const group = makeGroup()
    const json = buildGroupMetadataContent(group, { excludeSecret: true })
    const parsed = JSON.parse(json)
    expect(parsed.secret).toBeUndefined()
  })

  it('omits empty description and picture', () => {
    const group = makeGroup()
    const json = buildGroupMetadataContent(group)
    const parsed = JSON.parse(json)
    expect(parsed.description).toBeUndefined()
    expect(parsed.picture).toBeUndefined()
  })
})

describe('parseGroupMetadata', () => {
  it('parses valid metadata', () => {
    const meta: GroupMetadata = {
      id: 'g1', name: 'G', members: [ALICE], admins: [ALICE], secret: 'x'.repeat(64)
    }
    const result = parseGroupMetadata(JSON.stringify(meta))
    expect(result).toEqual(meta)
  })

  it('returns null for missing id', () => {
    expect(parseGroupMetadata(JSON.stringify({ name: 'G', members: [ALICE], admins: [ALICE] }))).toBeNull()
  })

  it('returns null for empty admins', () => {
    expect(parseGroupMetadata(JSON.stringify({ id: 'g1', name: 'G', members: [ALICE], admins: [] }))).toBeNull()
  })

  it('returns null for invalid JSON', () => {
    expect(parseGroupMetadata('not json')).toBeNull()
  })

  it('returns null for non-array members', () => {
    expect(parseGroupMetadata(JSON.stringify({ id: 'g1', name: 'G', members: 'bad', admins: [ALICE] }))).toBeNull()
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
      admins: [ALICE], description: 'new desc', secret: 'b'.repeat(64)
    }
    const updated = applyMetadataUpdate(group, meta)
    expect(updated.name).toBe('Updated')
    expect(updated.members).toEqual([ALICE, BOB, CAROL])
    expect(updated.description).toBe('new desc')
    expect(updated.secret).toBe('b'.repeat(64))
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
