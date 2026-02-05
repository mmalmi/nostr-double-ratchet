import { describe, expect, it } from "vitest"
import {
  RECEIPT_KIND,
  TYPING_KIND,
  isReceiptType,
  isTyping,
  parseReceipt,
  shouldAdvanceReceiptStatus,
} from "../src"

describe("receipt helpers", () => {
  it("parses delivered receipts with message ids", () => {
    const rumor = {
      kind: RECEIPT_KIND,
      content: "delivered",
      tags: [
        ["e", "msg-1"],
        ["p", "peer"],
        ["e", "msg-2"],
      ],
    }

    expect(parseReceipt(rumor)).toEqual({
      type: "delivered",
      messageIds: ["msg-1", "msg-2"],
    })
  })

  it("returns null for receipts without message ids", () => {
    const rumor = {
      kind: RECEIPT_KIND,
      content: "seen",
      tags: [["p", "peer"]],
    }

    expect(parseReceipt(rumor)).toBeNull()
  })

  it("returns null for invalid receipt type", () => {
    const rumor = {
      kind: RECEIPT_KIND,
      content: "read",
      tags: [["e", "msg-1"]],
    }

    expect(parseReceipt(rumor)).toBeNull()
  })

  it("returns null for non-receipt kind", () => {
    const rumor = {
      kind: 999,
      content: "seen",
      tags: [["e", "msg-1"]],
    }

    expect(parseReceipt(rumor)).toBeNull()
  })

  it("detects valid receipt types", () => {
    expect(isReceiptType("delivered")).toBe(true)
    expect(isReceiptType("seen")).toBe(true)
    expect(isReceiptType("read")).toBe(false)
  })

  it("advances receipt status in the right order", () => {
    expect(shouldAdvanceReceiptStatus(undefined, "delivered")).toBe(true)
    expect(shouldAdvanceReceiptStatus("delivered", "seen")).toBe(true)
    expect(shouldAdvanceReceiptStatus("seen", "delivered")).toBe(false)
    expect(shouldAdvanceReceiptStatus("seen", "seen")).toBe(false)
  })
})

describe("typing helpers", () => {
  it("detects typing events by kind", () => {
    expect(isTyping({ kind: TYPING_KIND, content: "typing" })).toBe(true)
    expect(isTyping({ kind: TYPING_KIND, content: "other" })).toBe(true)
    expect(isTyping({ kind: 1, content: "typing" })).toBe(false)
  })
})
