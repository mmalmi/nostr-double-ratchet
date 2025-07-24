import { Session } from './Session';
import { NostrSubscribe } from './types';

interface DeviceRecord {
  deviceId: string;
  publicKey: string;
  activeSession?: Session;
  inactiveSessions: Session[];
  isStale: boolean;
  staleTimestamp?: number;
  lastActivity?: number;
}

/**
 * Manages sessions for a single user across multiple devices
 * Structure: UserRecord → DeviceRecord → Sessions
 */
export class UserRecord {
  private deviceRecords: Map<string, DeviceRecord> = new Map();
  private isStale: boolean = false;
  private staleTimestamp?: number;

  constructor(
    public readonly userId: string,
    private readonly nostrSubscribe: NostrSubscribe,
  ) {
  }

  // ============================================================================
  // Device Management
  // ============================================================================

  /**
   * Creates or updates a device record for this user
   */
  public upsertDevice(deviceId: string, publicKey: string): DeviceRecord {
    let record = this.deviceRecords.get(deviceId);
    
    if (!record) {
      record = {
        deviceId,
        publicKey,
        inactiveSessions: [],
        isStale: false,
        lastActivity: Date.now()
      };
      this.deviceRecords.set(deviceId, record);
    } else if (record.publicKey !== publicKey) {
      // Public key changed - mark old sessions as inactive and update
      if (record.activeSession) {
        record.inactiveSessions.push(record.activeSession);
        record.activeSession = undefined;
      }
      record.publicKey = publicKey;
      record.lastActivity = Date.now();
    }

    return record;
  }

  /**
   * Gets a device record by deviceId
   */
  public getDevice(deviceId: string): DeviceRecord | undefined {
    return this.deviceRecords.get(deviceId);
  }

  /**
   * Gets all device records for this user
   */
  public getAllDevices(): DeviceRecord[] {
    return Array.from(this.deviceRecords.values());
  }

  /**
   * Gets all active (non-stale) device records
   */
  public getActiveDevices(): DeviceRecord[] {
    if (this.isStale) return [];
    return Array.from(this.deviceRecords.values()).filter(device => !device.isStale);
  }

  /**
   * Removes a device record and closes all its sessions
   */
  public removeDevice(deviceId: string): boolean {
    const record = this.deviceRecords.get(deviceId);
    if (!record) return false;

    // Close all sessions for this device
    record.activeSession?.close();
    record.inactiveSessions.forEach(session => session.close());
    
    return this.deviceRecords.delete(deviceId);
  }

  // ============================================================================
  // Session Management
  // ============================================================================

  /**
   * Adds or updates a session for a specific device
   */
  public upsertSession(deviceId: string, session: Session, publicKey?: string): void {
    const device = this.upsertDevice(deviceId, publicKey || session.state?.theirNextNostrPublicKey || '');
    
    // If there's an active session, move it to inactive
    if (device.activeSession) {
      device.inactiveSessions.unshift(device.activeSession);
    }

    // Set the new session as active
    session.name = deviceId; // Ensure session name matches deviceId
    device.activeSession = session;
    device.lastActivity = Date.now();
  }

  /**
   * Gets the active session for a specific device
   */
  public getActiveSession(deviceId: string): Session | undefined {
    const device = this.deviceRecords.get(deviceId);
    return device?.isStale ? undefined : device?.activeSession;
  }

  /**
   * Gets all sessions (active + inactive) for a specific device
   */
  public getDeviceSessions(deviceId: string): Session[] {
    const device = this.deviceRecords.get(deviceId);
    if (!device) return [];

    const sessions: Session[] = [];
    if (device.activeSession) {
      sessions.push(device.activeSession);
    }
    sessions.push(...device.inactiveSessions);
    return sessions;
  }

  /**
   * Gets all active sessions across all devices for this user
   */
  public getActiveSessions(): Session[] {
    const sessions: Session[] = [];

    for (const device of this.getActiveDevices()) {
      if (device.activeSession) {
        sessions.push(device.activeSession);
      }
    }

    // Sort by ability to send messages (prioritize ready sessions)
    sessions.sort((a, b) => {
      const aCanSend = !!(a.state?.theirNextNostrPublicKey && a.state?.ourCurrentNostrKey);
      const bCanSend = !!(b.state?.theirNextNostrPublicKey && b.state?.ourCurrentNostrKey);
      
      if (aCanSend && !bCanSend) return -1;
      if (!aCanSend && bCanSend) return 1;
      return 0;
    });

    return sessions;
  }

  /**
   * Gets all sessions (active + inactive) across all devices
   */
  public getAllSessions(): Session[] {
    const sessions: Session[] = [];

    for (const device of this.deviceRecords.values()) {
      if (device.activeSession) {
        sessions.push(device.activeSession);
      }
      sessions.push(...device.inactiveSessions);
    }

    return sessions;
  }

  /**
   * Gets sessions that are ready to send messages
   */
  public getSendableSessions(): Session[] {
    return this.getActiveSessions().filter(session => 
      !!(session.state?.theirNextNostrPublicKey && session.state?.ourCurrentNostrKey)
    );
  }

  // ============================================================================
  // Stale Management
  // ============================================================================

  /**
   * Marks a specific device as stale
   */
  public markDeviceStale(deviceId: string): void {
    const device = this.deviceRecords.get(deviceId);
    if (device) {
      device.isStale = true;
      device.staleTimestamp = Date.now();
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
   * Removes stale devices and sessions older than maxLatency
   */
  public pruneStaleRecords(maxLatency: number): void {
    const now = Date.now();

    // Remove stale devices
    for (const [deviceId, device] of this.deviceRecords.entries()) {
      if (device.isStale && device.staleTimestamp && 
          (now - device.staleTimestamp) > maxLatency) {
        this.removeDevice(deviceId);
      }
    }

    // Remove entire user record if stale
    if (this.isStale && this.staleTimestamp && 
        (now - this.staleTimestamp) > maxLatency) {
      this.close();
    }
  }

  // ============================================================================
  // Utility Methods
  // ============================================================================

  /**
   * Gets the most recently active device
   */
  public getMostActiveDevice(): DeviceRecord | undefined {
    const activeDevices = this.getActiveDevices();
    if (activeDevices.length === 0) return undefined;

    return activeDevices.reduce((most, current) => {
      const mostActivity = most.lastActivity || 0;
      const currentActivity = current.lastActivity || 0;
      return currentActivity > mostActivity ? current : most;
    });
  }

  /**
   * Gets the preferred session (from most active device)
   */
  public getPreferredSession(): Session | undefined {
    const mostActive = this.getMostActiveDevice();
    return mostActive?.activeSession;
  }

  /**
   * Checks if this user has any active sessions
   */
  public hasActiveSessions(): boolean {
    return this.getActiveSessions().length > 0;
  }

  /**
   * Gets total count of devices
   */
  public getDeviceCount(): number {
    return this.deviceRecords.size;
  }

  /**
   * Gets total count of active sessions
   */
  public getActiveSessionCount(): number {
    return this.getActiveSessions().length;
  }

  /**
   * Cleanup when destroying the user record
   */
  public close(): void {
    for (const device of this.deviceRecords.values()) {
      device.activeSession?.close();
      device.inactiveSessions.forEach(session => session.close());
    }
    this.deviceRecords.clear();
  }

  // ============================================================================
  // Legacy Compatibility Methods
  // ============================================================================

  /**
   * @deprecated Use upsertDevice instead
   */
  public conditionalUpdate(deviceId: string, publicKey: string): void {
    this.upsertDevice(deviceId, publicKey);
  }

  /**
   * @deprecated Use upsertSession instead  
   */
  public insertSession(deviceId: string, session: Session): void {
    this.upsertSession(deviceId, session);
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
    const device = this.getDevice(deviceId);
    if (!device) {
      throw new Error(`No device record found for ${deviceId}`);
    }

    const session = Session.init(
      this.nostrSubscribe,
      device.publicKey,
      ourCurrentPrivateKey,
      isInitiator,
      sharedSecret,
      name || deviceId
    );

    this.upsertSession(deviceId, session);
    return session;
  }
}
