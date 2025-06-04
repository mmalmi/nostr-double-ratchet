import { Session } from './Session';
import { NostrSubscribe } from './types';

interface DeviceRecord {
  publicKey: string;
  activeSession?: Session;
  inactiveSessions: Session[];
  isStale: boolean;
  staleTimestamp?: number;
}

/**
 * WIP: Conversation management system similar to Signal's Sesame
 * https://signal.org/docs/specifications/sesame/
 */
export class UserRecord {
  private deviceRecords: Map<string, DeviceRecord> = new Map();
  private isStale: boolean = false;
  private staleTimestamp?: number;

  constructor(
    public _userId: string,
    private _nostrSubscribe: NostrSubscribe,
  ) {
  }

  /**
   * Adds or updates a device record for this user
   */
  public conditionalUpdate(deviceId: string, publicKey: string): void {
    const existingRecord = this.deviceRecords.get(deviceId);
    
    // If device record doesn't exist or public key changed, create new empty record
    if (!existingRecord || existingRecord.publicKey !== publicKey) {
      this.deviceRecords.set(deviceId, {
        publicKey,
        inactiveSessions: [],
        isStale: false
      });
    }
  }

  /**
   * Inserts a new session for a device, making it the active session
   */
  public insertSession(deviceId: string, session: Session): void {
    const record = this.deviceRecords.get(deviceId);
    if (!record) {
      throw new Error(`No device record found for ${deviceId}`);
    }

    // Move current active session to inactive list if it exists
    if (record.activeSession) {
      record.inactiveSessions.unshift(record.activeSession);
    }

    // Set new session as active
    record.activeSession = session;
  }

  /**
   * Activates an inactive session for a device
   */
  public activateSession(deviceId: string, session: Session): void {
    const record = this.deviceRecords.get(deviceId);
    if (!record) {
      throw new Error(`No device record found for ${deviceId}`);
    }

    const sessionIndex = record.inactiveSessions.indexOf(session);
    if (sessionIndex === -1) {
      throw new Error('Session not found in inactive sessions');
    }

    // Remove session from inactive list
    record.inactiveSessions.splice(sessionIndex, 1);

    // Move current active session to inactive list if it exists
    if (record.activeSession) {
      record.inactiveSessions.unshift(record.activeSession);
    }

    // Set selected session as active
    record.activeSession = session;
  }

  /**
   * Marks a device record as stale
   */
  public markDeviceStale(deviceId: string): void {
    const record = this.deviceRecords.get(deviceId);
    if (record) {
      record.isStale = true;
      record.staleTimestamp = Date.now();
    }
  }

  /**
   * Marks the entire user record as stale
   */
  public markUserStale(): void {
    this.isStale = true;
    this.staleTimestamp = Date.now();
  }

  /**
   * Gets all non-stale device records with active sessions
   */
  public getActiveDevices(): Array<[string, Session]> {
    if (this.isStale) return [];

    return Array.from(this.deviceRecords.entries())
      .filter(([, record]) => !record.isStale && record.activeSession)
      .map(([deviceId, record]) => [deviceId, record.activeSession!]);
  }

  /**
   * Creates a new session for a device
   */
  public createSession(
    deviceId: string, 
    sharedSecret: Uint8Array,
    ourCurrentPrivateKey: Uint8Array,
    isInitiator: boolean,
    name?: string
  ): Session {
    const record = this.deviceRecords.get(deviceId);
    if (!record) {
      throw new Error(`No device record found for ${deviceId}`);
    }

    const session = Session.init(
      this._nostrSubscribe,
      record.publicKey,
      ourCurrentPrivateKey,
      isInitiator,
      sharedSecret,
      name
    );

    this.insertSession(deviceId, session);
    return session;
  }

  /**
   * Deletes stale records that are older than maxLatency
   */
  public pruneStaleRecords(maxLatency: number): void {
    const now = Date.now();

    // Delete stale device records
    for (const [deviceId, record] of this.deviceRecords.entries()) {
      if (record.isStale && record.staleTimestamp && 
          (now - record.staleTimestamp) > maxLatency) {
        // Close all sessions
        record.activeSession?.close();
        record.inactiveSessions.forEach(session => session.close());
        this.deviceRecords.delete(deviceId);
      }
    }

    // Delete entire user record if stale
    if (this.isStale && this.staleTimestamp && 
        (now - this.staleTimestamp) > maxLatency) {
      this.deviceRecords.forEach(record => {
        record.activeSession?.close();
        record.inactiveSessions.forEach(session => session.close());
      });
      this.deviceRecords.clear();
    }
  }

  /**
   * Cleanup when destroying the user record
   */
  public close(): void {
    this.deviceRecords.forEach(record => {
      record.activeSession?.close();
      record.inactiveSessions.forEach(session => session.close());
    });
    this.deviceRecords.clear();
  }
}
