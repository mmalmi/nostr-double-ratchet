import { ControlledMockRelay } from "./ControlledMockRelay"
import {
  createControlledMockSessionManager,
  createControlledMockDelegateSessionManager,
} from "./controlledMockSessionManager"
import { SessionManager } from "../../src/SessionManager"
import { Rumor } from "../../src/types"
import type { InMemoryStorageAdapter } from "../../src/StorageAdapter"
import { generateSecretKey, getPublicKey } from "nostr-tools"
import { DeviceManager } from "../../src/DeviceManager"

export type ActorId = "alice" | "bob"

export interface ControlledScenarioContext {
  relay: ControlledMockRelay
  actors: Record<ActorId, ActorState>
  /** Map of named event references to event IDs */
  eventRefs: Map<string, string>
  /** Map of subscription references to subscription IDs */
  subscriptionRefs: Map<string, string>
}

interface MessageWaiter {
  message: string
  targetCount: number
  resolve: () => void
  reject: (error: Error) => void
  timeout: ReturnType<typeof setTimeout>
}

interface ActorState {
  secretKey: Uint8Array
  publicKey: string
  devices: Map<string, DeviceState>
  mainDeviceManager?: DeviceManager
}

interface DeviceState {
  deviceId: string
  manager: SessionManager
  storage: InMemoryStorageAdapter
  events: Rumor[]
  messageCounts: Map<string, number>
  waiters: MessageWaiter[]
  unsub?: () => void
  subscriptionId?: string
  isDelegate?: boolean
  delegateDeviceManager?: DeviceManager
}

interface ActorDeviceRef {
  actor: ActorId
  deviceId: string
}

type WaitTarget = ActorDeviceRef | ActorDeviceRef[] | "all-recipient-devices"

// ============================================================================
// Step Types
// ============================================================================

type BaseStep =
  | { type: "addDevice"; actor: ActorId; deviceId: string }
  | { type: "addDelegateDevice"; actor: ActorId; deviceId: string; mainDeviceId: string }
  | { type: "close"; actor: ActorId; deviceId: string }
  | { type: "restart"; actor: ActorId; deviceId: string }
  | { type: "noop" }

type SendStep = {
  type: "send"
  from: ActorDeviceRef
  to: ActorId
  message: string
  /** Optional: name this event for later reference in delivery steps */
  ref?: string
  /**
   * Wait behavior after sending:
   * - undefined/not set: don't wait (manual delivery mode)
   * - "auto": deliver immediately and wait for receipt
   * - WaitTarget: deliver and wait for specific targets
   */
  waitOn?: WaitTarget | "auto"
}

type ExpectStep =
  | { type: "expect"; actor: ActorId; deviceId: string; message: string }
  | { type: "expectAll"; actor: ActorId; deviceId: string; messages: string[] }

// Delivery control steps
type DeliveryStep =
  | { type: "deliverNext" }
  | { type: "deliverAll" }
  | { type: "deliverEvent"; ref: string }
  | { type: "deliverInOrder"; refs: string[] }
  | { type: "deliverTo"; actor: ActorId; deviceId: string; ref: string }
  | { type: "deliverAllTo"; actor: ActorId; deviceId: string }
  | { type: "deliverNextAfter"; delayMs: number }
  | { type: "deliverAllWithDelay"; delayMs: number }
  | { type: "deliverWithJitter"; minMs: number; maxMs: number }

// Failure injection steps
type FailureStep =
  | { type: "dropEvent"; ref: string }
  | { type: "dropNext"; count?: number }
  | { type: "duplicateEvent"; ref: string }
  | { type: "simulateDisconnect"; clearPending?: boolean }
  | { type: "simulateReconnect" }

// EOSE control steps
type EoseStep =
  | { type: "sendEose"; actor: ActorId; deviceId: string }
  | { type: "sendEoseToAll" }
  | { type: "setAutoEose"; enabled: boolean }

// Relay state steps
type RelayStateStep =
  | { type: "clearPending" }
  | { type: "clearHistory" }
  | { type: "clearEvents" }

// Inspection/assertion steps
type InspectionStep =
  | { type: "expectPendingCount"; count: number }
  | { type: "expectDeliveryCount"; ref: string; count: number }
  | { type: "expectWasDeliveredTo"; ref: string; actor: ActorId; deviceId: string; delivered: boolean }

// Timing steps
type TimingStep =
  | { type: "wait"; ms: number }

export type ControlledScenarioStep =
  | BaseStep
  | SendStep
  | ExpectStep
  | DeliveryStep
  | FailureStep
  | EoseStep
  | RelayStateStep
  | InspectionStep
  | TimingStep

export type ControlledScenarioDefinition = {
  steps: ControlledScenarioStep[]
  /** Enable debug logging on the relay */
  debug?: boolean
}

// ============================================================================
// Scenario Runner
// ============================================================================

export async function runControlledScenario(
  def: ControlledScenarioDefinition
): Promise<ControlledScenarioContext> {
  const relay = new ControlledMockRelay({ debug: def.debug })
  const context: ControlledScenarioContext = {
    relay,
    actors: {
      alice: createActorState(),
      bob: createActorState(),
    },
    eventRefs: new Map(),
    subscriptionRefs: new Map(),
  }

  for (const step of def.steps) {
    if (def.debug) {
      console.log(`\n--- Executing step: ${JSON.stringify(step)} ---`)
    }
    await executeStep(context, step, def.debug)
  }

  return context
}

async function executeStep(
  context: ControlledScenarioContext,
  step: ControlledScenarioStep,
  _debug?: boolean
): Promise<void> {
  switch (step.type) {
    // Base steps
    case "addDevice":
      await addDevice(context, step.actor, step.deviceId)
      break
    case "addDelegateDevice":
      await addDelegateDevice(context, step.actor, step.deviceId, step.mainDeviceId)
      break
    case "close":
      closeDevice(context, { actor: step.actor, deviceId: step.deviceId })
      break
    case "restart":
      await restartDevice(context, { actor: step.actor, deviceId: step.deviceId })
      break
    case "noop":
      break

    // Send step
    case "send":
      await sendMessage(context, step.from, step.to, step.message, step.ref, step.waitOn)
      break

    // Expect steps
    case "expect":
      await expectMessage(context, step.actor, step.deviceId, step.message)
      break
    case "expectAll":
      await expectAllMessages(context, step.actor, step.deviceId, step.messages)
      break

    // Delivery control steps
    case "deliverNext":
      context.relay.deliverNext()
      break
    case "deliverAll":
      context.relay.deliverAll()
      break
    case "deliverEvent": {
      const eventId = getEventRef(context, step.ref)
      context.relay.deliverEvent(eventId)
      break
    }
    case "deliverInOrder": {
      const eventIds = step.refs.map((ref) => getEventRef(context, ref))
      context.relay.deliverInOrder(eventIds)
      break
    }
    case "deliverTo": {
      const eventId = getEventRef(context, step.ref)
      const device = getDevice(context, { actor: step.actor, deviceId: step.deviceId })
      if (device.subscriptionId) {
        context.relay.deliverTo(device.subscriptionId, eventId)
      }
      break
    }
    case "deliverAllTo": {
      const device = getDevice(context, { actor: step.actor, deviceId: step.deviceId })
      if (device.subscriptionId) {
        context.relay.deliverAllTo(device.subscriptionId)
      }
      break
    }
    case "deliverNextAfter":
      await context.relay.deliverNextAfter(step.delayMs)
      break
    case "deliverAllWithDelay":
      await context.relay.deliverAllWithDelay(step.delayMs)
      break
    case "deliverWithJitter":
      await context.relay.deliverWithJitter(step.minMs, step.maxMs)
      break

    // Failure injection steps
    case "dropEvent": {
      const eventId = getEventRef(context, step.ref)
      context.relay.dropEvent(eventId)
      break
    }
    case "dropNext":
      context.relay.dropNext(step.count ?? 1)
      break
    case "duplicateEvent": {
      const eventId = getEventRef(context, step.ref)
      context.relay.duplicateEvent(eventId)
      break
    }
    case "simulateDisconnect":
      context.relay.simulateDisconnect(step.clearPending)
      break
    case "simulateReconnect":
      context.relay.simulateReconnect()
      break

    // EOSE control steps
    case "sendEose": {
      const device = getDevice(context, { actor: step.actor, deviceId: step.deviceId })
      if (device.subscriptionId) {
        context.relay.sendEose(device.subscriptionId)
      }
      break
    }
    case "sendEoseToAll":
      context.relay.sendEoseToAll()
      break
    case "setAutoEose":
      context.relay.setAutoEose(step.enabled)
      break

    // Relay state steps
    case "clearPending":
      context.relay.clearPending()
      break
    case "clearHistory":
      context.relay.clearHistory()
      break
    case "clearEvents":
      context.relay.clearPending()
      break

    // Inspection steps
    case "expectPendingCount":
      if (context.relay.getPendingCount() !== step.count) {
        throw new Error(
          `Expected ${step.count} pending events, got ${context.relay.getPendingCount()}`
        )
      }
      break
    case "expectDeliveryCount": {
      const eventId = getEventRef(context, step.ref)
      const count = context.relay.getDeliveryCount(eventId)
      if (count !== step.count) {
        throw new Error(`Expected delivery count ${step.count} for ${step.ref}, got ${count}`)
      }
      break
    }
    case "expectWasDeliveredTo": {
      const eventId = getEventRef(context, step.ref)
      const device = getDevice(context, { actor: step.actor, deviceId: step.deviceId })
      if (device.subscriptionId) {
        const wasDelivered = context.relay.wasDeliveredTo(eventId, device.subscriptionId)
        if (wasDelivered !== step.delivered) {
          throw new Error(
            `Expected wasDeliveredTo(${step.ref}, ${step.actor}/${step.deviceId}) to be ${step.delivered}, got ${wasDelivered}`
          )
        }
      }
      break
    }

    // Timing steps
    case "wait":
      await new Promise((resolve) => setTimeout(resolve, step.ms))
      break

    default: {
      const exhaustive: never = step
      throw new Error(`Unhandled step ${JSON.stringify(exhaustive)}`)
    }
  }
}

// ============================================================================
// Helper Functions
// ============================================================================

function createActorState(): ActorState {
  const secretKey = generateSecretKey()
  const publicKey = getPublicKey(secretKey)
  return {
    secretKey,
    publicKey,
    devices: new Map(),
  }
}

function getEventRef(context: ControlledScenarioContext, ref: string): string {
  const eventId = context.eventRefs.get(ref)
  if (!eventId) {
    throw new Error(`Event ref '${ref}' not found. Make sure to use 'ref' in send step.`)
  }
  return eventId
}

async function sendMessage(
  context: ControlledScenarioContext,
  from: ActorDeviceRef,
  to: ActorId,
  message: string,
  ref?: string,
  waitOn?: WaitTarget | "auto"
) {
  const senderDevice = getDevice(context, from)
  const recipientActor = context.actors[to]
  if (!recipientActor) {
    throw new Error(`Unknown recipient actor '${to}'`)
  }

  // Send the message (with auto-deliver mode in sessionManager, it delivers immediately)
  await senderDevice.manager.sendMessage(recipientActor.publicKey, message)

  // Find the event ID of what we just sent (most recent event)
  const allEvents = context.relay.getAllEvents()
  const sentEvent = allEvents[allEvents.length - 1]

  if (sentEvent && ref) {
    context.eventRefs.set(ref, sentEvent.id)
  }

  // Handle wait behavior
  if (waitOn === "auto") {
    // Wait for all recipient devices to receive
    const waitTargets = resolveWaitTargets(context, "all-recipient-devices", recipientActor)
    await Promise.all(
      waitTargets.map((device) =>
        waitForMessage(device, deviceLabel(recipientActor, device), message, {
          existingOk: false,
        })
      )
    )
  } else if (waitOn) {
    // Wait for specific targets
    const waitTargets = resolveWaitTargets(context, waitOn as WaitTarget, recipientActor)
    await Promise.all(
      waitTargets.map((device) =>
        waitForMessage(device, deviceLabel(recipientActor, device), message, {
          existingOk: false,
        })
      )
    )
  }
  // If waitOn is undefined, just send without waiting
}

async function expectMessage(
  context: ControlledScenarioContext,
  actor: ActorId,
  deviceId: string,
  message: string
) {
  const device = getDevice(context, { actor, deviceId })
  await waitForMessage(device, deviceLabel(context.actors[actor], device), message, {
    existingOk: true,
  })
}

async function expectAllMessages(
  context: ControlledScenarioContext,
  actor: ActorId,
  deviceId: string,
  messages: string[]
) {
  const actorState = context.actors[actor]
  const device = getDevice(context, { actor, deviceId })
  for (const msg of messages) {
    await waitForMessage(device, deviceLabel(actorState, device), msg, { existingOk: true })
  }
}

function closeDevice(context: ControlledScenarioContext, ref: ActorDeviceRef) {
  const device = getDevice(context, ref)
  rejectPendingWaiters(device, new Error(`Device ${refToString(ref)} closed`))
  device.unsub?.()
  device.manager.close()
}

async function restartDevice(context: ControlledScenarioContext, ref: ActorDeviceRef) {
  const actor = context.actors[ref.actor]
  if (!actor) {
    throw new Error(`Unknown actor '${ref.actor}'`)
  }
  const device = getDevice(context, ref)
  device.unsub?.()
  device.manager.close()

  const { manager: newManager } = await createControlledMockSessionManager(
    device.deviceId,
    context.relay,
    actor.secretKey,
    device.storage
  )

  device.manager = newManager
  device.unsub = attachManagerListener(actor, device)

  // Update subscription ID for the new manager
  const subs = context.relay.getSubscriptions()
  const latestSub = subs[subs.length - 1]
  if (latestSub) {
    device.subscriptionId = latestSub.id
  }
}

function attachManagerListener(_actor: ActorState, device: DeviceState): () => void {
  const onEvent = (event: Rumor) => {
    device.events.push(event)
    const content = event.content ?? ""
    const currentCount = device.messageCounts.get(content) ?? 0
    const nextCount = currentCount + 1
    device.messageCounts.set(content, nextCount)
    resolveWaiters(device, content, nextCount)
  }

  const unsubscribe = device.manager.onEvent(onEvent)
  return () => {
    unsubscribe()
  }
}

function resolveWaiters(device: DeviceState, content: string, count: number) {
  const pending = device.waiters.slice()
  for (const waiter of pending) {
    if (waiter.message === content && count >= waiter.targetCount) {
      waiter.resolve()
    }
  }
}

function waitForMessage(
  device: DeviceState,
  label: string,
  message: string,
  options: { existingOk: boolean }
): Promise<void> {
  const { existingOk } = options
  const currentCount = device.messageCounts.get(message) ?? 0
  if (existingOk && currentCount > 0) {
    return Promise.resolve()
  }

  return new Promise<void>((resolve, reject) => {
    const handleResolve = (waiter: MessageWaiter) => {
      clearTimeout(waiter.timeout)
      removeWaiter(device, waiter)
      resolve()
    }

    const handleReject = (waiter: MessageWaiter, error: Error) => {
      clearTimeout(waiter.timeout)
      removeWaiter(device, waiter)
      reject(error)
    }

    const waiter: MessageWaiter = {
      message,
      targetCount: currentCount + 1,
      resolve: () => handleResolve(waiter),
      reject: (error: Error) => handleReject(waiter, error),
      timeout: setTimeout(() => {
        handleReject(
          waiter,
          new Error(`Timed out waiting for message '${message}' on ${label}`)
        )
      }, 5000),
    }

    device.waiters.push(waiter)
  })
}

function removeWaiter(device: DeviceState, waiter: MessageWaiter) {
  const index = device.waiters.indexOf(waiter)
  if (index >= 0) {
    device.waiters.splice(index, 1)
  }
}

function rejectPendingWaiters(device: DeviceState, error: Error) {
  const waiters = device.waiters.slice()
  for (const waiter of waiters) {
    waiter.reject(error)
  }
}

function refToString(ref: ActorDeviceRef): string {
  return `${ref.actor}/${ref.deviceId}`
}

async function addDevice(
  context: ControlledScenarioContext,
  actorId: ActorId,
  deviceId: string
) {
  const actor = getActor(context, actorId)
  if (actor.devices.has(deviceId)) {
    throw new Error(`Device '${deviceId}' already exists for actor '${actorId}'`)
  }

  const { manager, mockStorage, deviceManager } = await createControlledMockSessionManager(
    deviceId,
    context.relay,
    actor.secretKey
  )

  // Track the first device's DeviceManager as the main one for this actor
  if (!actor.mainDeviceManager) {
    actor.mainDeviceManager = deviceManager
  }

  const deviceState = createDeviceState(actor, deviceId, manager, mockStorage)

  // Track subscription ID for delivery control
  const subs = context.relay.getSubscriptions()
  const latestSub = subs[subs.length - 1]
  if (latestSub) {
    deviceState.subscriptionId = latestSub.id
    context.subscriptionRefs.set(`${actorId}/${deviceId}`, latestSub.id)
  }

  actor.devices.set(deviceId, deviceState)
  return deviceState
}

async function addDelegateDevice(
  context: ControlledScenarioContext,
  actorId: ActorId,
  deviceId: string,
  mainDeviceId: string
) {
  const actor = getActor(context, actorId)
  if (actor.devices.has(deviceId)) {
    throw new Error(`Device '${deviceId}' already exists for actor '${actorId}'`)
  }

  const mainDevice = actor.devices.get(mainDeviceId)
  if (!mainDevice) {
    throw new Error(`Main device '${mainDeviceId}' not found for actor '${actorId}'`)
  }

  if (!actor.mainDeviceManager) {
    throw new Error(`No main DeviceManager found for actor '${actorId}'`)
  }

  const { manager, mockStorage, delegateDeviceManager } =
    await createControlledMockDelegateSessionManager(
      deviceId,
      context.relay,
      actor.mainDeviceManager
    )

  const deviceState = createDeviceState(actor, deviceId, manager, mockStorage)
  deviceState.isDelegate = true
  deviceState.delegateDeviceManager = delegateDeviceManager

  // Track subscription ID
  const subs = context.relay.getSubscriptions()
  const latestSub = subs[subs.length - 1]
  if (latestSub) {
    deviceState.subscriptionId = latestSub.id
    context.subscriptionRefs.set(`${actorId}/${deviceId}`, latestSub.id)
  }

  actor.devices.set(deviceId, deviceState)
  return deviceState
}

function getActor(context: ControlledScenarioContext, actorId: ActorId): ActorState {
  const actor = context.actors[actorId]
  if (!actor) {
    throw new Error(`Unknown actor '${actorId}'`)
  }
  return actor
}

function getDevice(context: ControlledScenarioContext, ref: ActorDeviceRef): DeviceState {
  const actor = getActor(context, ref.actor)
  const device = actor.devices.get(ref.deviceId)
  if (!device) {
    throw new Error(`Device '${ref.deviceId}' not registered for actor '${ref.actor}'`)
  }
  return device
}

function deviceLabel(actor: ActorState, device: DeviceState): string {
  return `${actor.publicKey.slice(0, 8)}.../${device.deviceId}`
}

function createDeviceState(
  actor: ActorState,
  deviceId: string,
  manager: SessionManager,
  storage: InMemoryStorageAdapter
): DeviceState {
  const deviceState: DeviceState = {
    deviceId,
    manager,
    storage,
    events: [],
    messageCounts: new Map(),
    waiters: [],
  }

  deviceState.unsub = attachManagerListener(actor, deviceState)
  return deviceState
}

function resolveWaitTargets(
  context: ControlledScenarioContext,
  waitOn: WaitTarget | undefined,
  recipient: ActorState
): DeviceState[] {
  if (!waitOn) {
    const devices = Array.from(recipient.devices.values())
    if (devices.length === 0) {
      throw new Error("Recipient actor has no devices. Add one before sending.")
    }
    return devices
  }

  if (waitOn === "all-recipient-devices") {
    const devices = Array.from(recipient.devices.values())
    if (devices.length === 0) {
      throw new Error("Recipient has no devices to wait on")
    }
    return devices
  }

  const refs = Array.isArray(waitOn) ? waitOn : [waitOn]
  return refs.map((ref) => getDevice(context, ref))
}
