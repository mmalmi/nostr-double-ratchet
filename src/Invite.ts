import { generateSecretKey, getPublicKey, nip44, finalizeEvent, VerifiedEvent, UnsignedEvent, verifyEvent, Filter } from "nostr-tools";
import { INVITE_EVENT_KIND, NostrSubscribe, Unsubscribe, EncryptFunction, DecryptFunction, INVITE_RESPONSE_KIND } from "./types";
import { getConversationKey } from "nostr-tools/nip44";
import { Session } from "./Session.ts";
import { hexToBytes, bytesToHex } from "@noble/hashes/utils";

const TWO_DAYS = 2 * 24 * 60 * 60

const now = () => Math.round(Date.now() / 1000)
const randomNow = () => Math.round(now() - Math.random() * TWO_DAYS)

/**
 * Invite is a safe way to exchange session keys and initiate secret sessions.
 * 
 * It can be shared privately as an URL (e.g. QR code) or published as a Nostr event.
 * 
 * Even if inviter's or invitee's long-term private key (identity key) and the shared secret (link) is compromised,
 * forward secrecy is preserved as long as the session keys are not compromised.
 * 
 * Also make sure to keep the session key safe.
 */
export class Invite {
    constructor(
        public inviterEphemeralPublicKey: string,
        public sharedSecret: string,
        public inviter: string,
        public inviterEphemeralPrivateKey?: Uint8Array,
        public deviceId?: string,
        public maxUses?: number,
        public usedBy: string[] = [],
        public createdAt: number = now(),
    ) {
    }

    static createNew(inviter: string, deviceId?: string, maxUses?: number): Invite {
        if (!inviter) {
            throw new Error("Inviter public key is required");
        }
        const inviterEphemeralPrivateKey = generateSecretKey();
        const inviterEphemeralPublicKey = getPublicKey(inviterEphemeralPrivateKey);
        const sharedSecret = bytesToHex(generateSecretKey());
        return new Invite(
            inviterEphemeralPublicKey,
            sharedSecret,
            inviter,
            inviterEphemeralPrivateKey,
            deviceId,
            maxUses
        );
    }

    static fromUrl(url: string): Invite {
        const parsedUrl = new URL(url);
        const rawHash = parsedUrl.hash.slice(1);
        if (!rawHash) {
            throw new Error("No invite data found in the URL hash.");
        }

        const decodedHash = decodeURIComponent(rawHash);
        let data: { inviter?: string; ephemeralKey?: string; sharedSecret?: string };
        try {
            data = JSON.parse(decodedHash);
        } catch (err) {
            throw new Error("Invite data in URL hash is not valid JSON: " + err);
        }

        const { inviter, ephemeralKey, sharedSecret } = data;
        if (!inviter || !ephemeralKey || !sharedSecret) {
            throw new Error("Missing required fields (inviter, ephemeralKey, sharedSecret) in invite data.");
        }

        return new Invite(
            ephemeralKey,
            sharedSecret,
            inviter
        );
    }

    static deserialize(json: string): Invite {
        const data = JSON.parse(json);
        return new Invite(
            data.inviterEphemeralPublicKey,
            data.sharedSecret,
            data.inviter,
            data.inviterEphemeralPrivateKey ? new Uint8Array(data.inviterEphemeralPrivateKey) : undefined,
            data.deviceId,
            data.maxUses,
            data.usedBy,
            data.createdAt,
        );
    }

    static fromEvent(event: VerifiedEvent): Invite {
        if (!event.sig) {
            throw new Error("Event is not signed");
        }
        if (!verifyEvent(event)) {
            throw new Error("Event signature is invalid");
        }
        const { tags } = event;

        if (!tags) {
            throw new Error("Invalid invite event: missing tags");
        }

        const inviterEphemeralPublicKey = tags.find(([key]) => key === 'ephemeralKey')?.[1];
        const sharedSecret = tags.find(([key]) => key === 'sharedSecret')?.[1];
        const inviter = event.pubkey;

        // Extract deviceId from the "d" tag (format: double-ratchet/invites/<deviceId>)
        const deviceTag = tags.find(([key]) => key === 'd')?.[1]
        const deviceId = deviceTag?.split('/')?.[2]

        if (!inviterEphemeralPublicKey || !sharedSecret) {
            throw new Error("Invalid invite event: missing session key or sharedSecret");
        }

        return new Invite(
            inviterEphemeralPublicKey,
            sharedSecret,
            inviter,
            undefined, // inviterEphemeralPrivateKey not available when parsing from event
            deviceId
        );
    }

    static fromUser(user: string, subscribe: NostrSubscribe, onInvite: (_invite: Invite) => void): Unsubscribe {
        const filter: Filter = {
            kinds: [INVITE_EVENT_KIND],
            authors: [user],
            "#l": ["double-ratchet/invites"]
        };
        const seenIds = new Set<string>()
        const unsub = subscribe(filter, (event) => {
            if (event.pubkey !== user) {
                console.error("Got invite event from wrong user", event.pubkey, "expected", user)
                return;
            }
            if (seenIds.has(event.id)) return
            seenIds.add(event.id)
            try {
                const inviteLink = Invite.fromEvent(event);
                onInvite(inviteLink);
            } catch {
            }
        });

        return unsub;
    }

    /**
     * Save Invite as JSON. Includes the inviter's session private key, so don't share this.
     */
    serialize(): string {
        return JSON.stringify({
            inviterEphemeralPublicKey: this.inviterEphemeralPublicKey,
            sharedSecret: this.sharedSecret,
            inviter: this.inviter,
            inviterEphemeralPrivateKey: this.inviterEphemeralPrivateKey ? Array.from(this.inviterEphemeralPrivateKey) : undefined,
            deviceId: this.deviceId,
            maxUses: this.maxUses,
            usedBy: this.usedBy,
            createdAt: this.createdAt,
        });
    }

    /**
     * Invite parameters are in the URL's hash so they are not sent to the server.
     */
    getUrl(root = "https://iris.to") {
        const data = {
            inviter: this.inviter,
            ephemeralKey: this.inviterEphemeralPublicKey,
            sharedSecret: this.sharedSecret
        };
        const url = new URL(root);
        url.hash = encodeURIComponent(JSON.stringify(data));
        return url.toString();
    }

    getEvent(): UnsignedEvent {
        if (!this.deviceId) {
            throw new Error("Device ID is required");
        }
        return {
            kind: INVITE_EVENT_KIND,
            pubkey: this.inviter,
            content: "",
            created_at: this.createdAt,
            tags: [
                ['ephemeralKey', this.inviterEphemeralPublicKey],
                ['sharedSecret', this.sharedSecret],
                ['d', 'double-ratchet/invites/' + this.deviceId],
                ['l', 'double-ratchet/invites']
            ],
        };
    }

    /**
     * Creates an "invite tombstone" event that clears the original content and removes the list tag.
     * Used when the inviter logs out and wants to make the invite invisible to other devices.
     */
    getDeletionEvent(): UnsignedEvent {
        if (!this.deviceId) {
            throw new Error("Device ID is required");
        }
        return {
            kind: INVITE_EVENT_KIND,
            pubkey: this.inviter,
            content: "", // deliberately empty
            created_at: Math.floor(Date.now() / 1000),
            tags: [
                ['ephemeralKey', this.inviterEphemeralPublicKey],
                ['sharedSecret', this.sharedSecret],
                ['d', 'double-ratchet/invites/' + this.deviceId], // same d tag
            ],
        };
    }

    /**
     * Called by the invitee. Accepts the invite and creates a new session with the inviter.
     *
     * @param nostrSubscribe - A function to subscribe to Nostr events
     * @param inviteePublicKey - The invitee's public key
     * @param encryptor - The invitee's secret key or a signing/encrypt function
     * @param deviceId - Optional device ID to identify the invitee's device
     * @returns An object containing the new session and an event to be published
     *
     * 1. Inner event: No signature, content encrypted with DH(inviter, invitee).
     *    Purpose: Authenticate invitee. Contains invitee session key and deviceId.
     * 2. Envelope: No signature, content encrypted with DH(inviter, random key).
     *    Purpose: Contains inner event. Hides invitee from others who might have the shared Nostr key.

     * Note: You need to publish the returned event on Nostr using NDK or another Nostr system of your choice,
     * so the inviter can create the session on their side.
     */
    async accept(
        nostrSubscribe: NostrSubscribe,
        inviteePublicKey: string,
        encryptor: Uint8Array | EncryptFunction,
        deviceId?: string,
    ): Promise<{ session: Session, event: VerifiedEvent }> {
        const inviteeSessionKey = generateSecretKey();
        const inviteeSessionPublicKey = getPublicKey(inviteeSessionKey);
        const inviterPublicKey = this.inviter || this.inviterEphemeralPublicKey;

        const sharedSecret = hexToBytes(this.sharedSecret);
        const session = Session.init(nostrSubscribe, this.inviterEphemeralPublicKey, inviteeSessionKey, true, sharedSecret, undefined);

        // should we take only Encrypt / Decrypt functions, not keys, to make it simpler and with less imports here?
        // common implementation problem: plaintext, pubkey params in different order
        const encrypt = typeof encryptor === 'function' ?
            encryptor :
            (plaintext: string, pubkey: string) => Promise.resolve(nip44.encrypt(plaintext, getConversationKey(encryptor, pubkey)));

        const payload = JSON.stringify({
            sessionKey: inviteeSessionPublicKey,
            deviceId: deviceId
        });
        const dhEncrypted = await encrypt(payload, inviterPublicKey);

        const innerEvent = {
            pubkey: inviteePublicKey,
            content: await nip44.encrypt(dhEncrypted, sharedSecret),
            created_at: Math.floor(Date.now() / 1000),
        };
        const innerJson = JSON.stringify(innerEvent);

        // Create a random keypair for the envelope sender
        const randomSenderKey = generateSecretKey();
        const randomSenderPublicKey = getPublicKey(randomSenderKey);

        const envelope = {
            kind: INVITE_RESPONSE_KIND,
            pubkey: randomSenderPublicKey,
            content: nip44.encrypt(innerJson, getConversationKey(randomSenderKey, this.inviterEphemeralPublicKey)),
            created_at: randomNow(),
            tags: [['p', this.inviterEphemeralPublicKey]],
        };

        return { session, event: finalizeEvent(envelope, randomSenderKey) };
    }

    listen(decryptor: Uint8Array | DecryptFunction, nostrSubscribe: NostrSubscribe, onSession: (_session: Session, _identity: string, _deviceId?: string) => void): Unsubscribe {
        if (!this.inviterEphemeralPrivateKey) {
            throw new Error("Inviter session key is not available");
        }
        
        const filter = {
            kinds: [INVITE_RESPONSE_KIND],
            '#p': [this.inviterEphemeralPublicKey],
        };

        return nostrSubscribe(filter, async (event) => {
            try {
                if (this.maxUses && this.usedBy.length >= this.maxUses) {
                    return;
                }

                // Decrypt the outer envelope first
                const decrypted = await nip44.decrypt(event.content, getConversationKey(this.inviterEphemeralPrivateKey!, event.pubkey));
                const innerEvent = JSON.parse(decrypted);

                const sharedSecret = hexToBytes(this.sharedSecret);
                const inviteeIdentity = innerEvent.pubkey;
                this.usedBy.push(inviteeIdentity);

                // Decrypt the inner content using shared secret first
                const dhEncrypted = await nip44.decrypt(innerEvent.content, sharedSecret);

                // Then decrypt using DH key
                const innerDecrypt = typeof decryptor === 'function' ?
                    decryptor :
                    (ciphertext: string, pubkey: string) => Promise.resolve(nip44.decrypt(ciphertext, getConversationKey(decryptor, pubkey)));

                const decryptedPayload = await innerDecrypt(dhEncrypted, inviteeIdentity);

                let inviteeSessionPublicKey: string;
                let deviceId: string | undefined;

                try {
                    const parsed = JSON.parse(decryptedPayload);
                    inviteeSessionPublicKey = parsed.sessionKey;
                    deviceId = parsed.deviceId;
                } catch {
                    inviteeSessionPublicKey = decryptedPayload;
                }

                const name = event.id;
                const session = Session.init(nostrSubscribe, inviteeSessionPublicKey, this.inviterEphemeralPrivateKey!, false, sharedSecret, name);

                onSession(session, inviteeIdentity, deviceId);
            } catch {
            }
        });
    }
}
