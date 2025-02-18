import { generateSecretKey, getPublicKey, nip44, finalizeEvent, VerifiedEvent, UnsignedEvent, verifyEvent, Filter } from "nostr-tools";
import { INVITE_EVENT_KIND, NostrSubscribe, Unsubscribe } from "./types";
import { getConversationKey } from "nostr-tools/nip44";
import { Channel } from "./Channel";
import { MESSAGE_EVENT_KIND } from "./types";
import { EncryptFunction, DecryptFunction } from "./types";
import { hexToBytes, bytesToHex } from "@noble/hashes/utils";

/**
 * Invite is a safe way to exchange session keys and initiate secret channels.
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
        public inviterSessionPublicKey: string,
        public linkSecret: string,
        public inviter: string,
        public inviterSessionPrivateKey?: Uint8Array,
        public label?: string,
        public maxUses?: number,
        public usedBy: string[] = [],
    ) {}

    static createNew(inviter: string, label?: string, maxUses?: number): Invite {
        const inviterSessionPrivateKey = generateSecretKey();
        const inviterSessionPublicKey = getPublicKey(inviterSessionPrivateKey);
        const linkSecret = bytesToHex(generateSecretKey());
        return new Invite(
            inviterSessionPublicKey,
            linkSecret,
            inviter,
            inviterSessionPrivateKey,
            label,
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
        let data: any;
        try {
            data = JSON.parse(decodedHash);
        } catch (err) {
            throw new Error("Invite data in URL hash is not valid JSON: " + err);
        }

        const { inviter, sessionKey, linkSecret } = data;
        if (!inviter || !sessionKey || !linkSecret) {
            throw new Error("Missing required fields (inviter, sessionKey, linkSecret) in invite data.");
        }

        return new Invite(
            sessionKey,
            linkSecret,
            inviter
        );
    }

    static deserialize(json: string): Invite {
        const data = JSON.parse(json);
        return new Invite(
            data.inviterSessionPublicKey,
            data.linkSecret,
            data.inviter,
            data.inviterSessionPrivateKey ? new Uint8Array(data.inviterSessionPrivateKey) : undefined,
            data.label,
            data.maxUses,
            data.usedBy
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
        const inviterSessionPublicKey = tags.find(([key]) => key === 'sessionKey')?.[1];
        const linkSecret = tags.find(([key]) => key === 'linkSecret')?.[1];
        const inviter = event.pubkey;

        if (!inviterSessionPublicKey || !linkSecret) {
            throw new Error("Invalid invite event: missing session key or link secret");
        }

        return new Invite(
            inviterSessionPublicKey,
            linkSecret,
            inviter
        );
    }

    static fromUser(user: string, subscribe: NostrSubscribe): Promise<Invite | undefined> {
        const filter: Filter = {
            kinds: [INVITE_EVENT_KIND],
            authors: [user],
            limit: 1,
            "#d": ["nostr-double-ratchet/invite"],
        };
        return new Promise((resolve) => {
            const unsub = subscribe(filter, (event) => {
                try {
                    const inviteLink = Invite.fromEvent(event);
                    unsub();
                    resolve(inviteLink);
                } catch (error) {
                    unsub();
                    resolve(undefined);
                }
            });

            // Set timeout to unsubscribe and return undefined after 10 seconds
            setTimeout(() => {
                unsub();
                resolve(undefined);
            }, 10000);
        });
    }

    /**
     * Save Invite as JSON. Includes the inviter's session private key, so don't share this.
     */
    serialize(): string {
        return JSON.stringify({
            inviterSessionPublicKey: this.inviterSessionPublicKey,
            linkSecret: this.linkSecret,
            inviter: this.inviter,
            inviterSessionPrivateKey: this.inviterSessionPrivateKey ? Array.from(this.inviterSessionPrivateKey) : undefined,
            label: this.label,
            maxUses: this.maxUses,
            usedBy: this.usedBy,
        });
    }

    /**
     * Invite parameters are in the URL's hash so they are not sent to the server.
     */
    getUrl(root = "https://iris.to") {
        const data = {
            inviter: this.inviter,
            sessionKey: this.inviterSessionPublicKey,
            linkSecret: this.linkSecret
        };
        const url = new URL(root);
        url.hash = encodeURIComponent(JSON.stringify(data));
        console.log('url', url.toString())
        return url.toString();
    }

    getEvent(): UnsignedEvent {
        return {
            kind: INVITE_EVENT_KIND,
            pubkey: this.inviter,
            content: "",
            created_at: Math.floor(Date.now() / 1000),
            tags: [['sessionKey', this.inviterSessionPublicKey], ['linkSecret', this.linkSecret], ['d', 'nostr-double-ratchet/invite']],
        };
    }

    /**
     * Called by the invitee. Accepts the invite and creates a new channel with the inviter.
     * 
     * @param inviteeSecretKey - The invitee's secret key or a signing function
     * @param nostrSubscribe - A function to subscribe to Nostr events
     * @returns An object containing the new channel and an event to be published
     * 
     * 1. Inner event: No signature, content encrypted with DH(inviter, invitee).
     *    Purpose: Authenticate invitee. Contains invitee session key.
     * 2. Envelope: No signature, content encrypted with DH(inviter, random key).
     *    Purpose: Contains inner event. Hides invitee from others who might have the shared Nostr key.

     * Note: You need to publish the returned event on Nostr using NDK or another Nostr system of your choice,
     * so the inviter can create the channel on their side.
     */
    async accept(
        nostrSubscribe: NostrSubscribe,
        inviteePublicKey: string,
        inviteeSecretKey: Uint8Array | EncryptFunction,
    ): Promise<{ channel: Channel, event: VerifiedEvent }> {
        const inviteeSessionKey = generateSecretKey();
        const inviteeSessionPublicKey = getPublicKey(inviteeSessionKey);
        const inviterPublicKey = this.inviter || this.inviterSessionPublicKey;

        const sharedSecret = hexToBytes(this.linkSecret);
        const channel = Channel.init(nostrSubscribe, this.inviterSessionPublicKey, inviteeSessionKey, true, sharedSecret, undefined);

        // Create a random keypair for the envelope sender
        const randomSenderKey = generateSecretKey();
        const randomSenderPublicKey = getPublicKey(randomSenderKey);

        const encrypt = typeof inviteeSecretKey === 'function' ?
            inviteeSecretKey :
            (plaintext: string, pubkey: string) => Promise.resolve(nip44.encrypt(plaintext, getConversationKey(inviteeSecretKey, pubkey)));

        const innerEvent = {
            pubkey: inviteePublicKey,
            tags: [['secret', this.linkSecret]],
            content: await encrypt(inviteeSessionPublicKey, inviterPublicKey),
            created_at: Math.floor(Date.now() / 1000),
        };

        const envelope = {
            kind: MESSAGE_EVENT_KIND,
            pubkey: randomSenderPublicKey,
            content: nip44.encrypt(JSON.stringify(innerEvent), getConversationKey(randomSenderKey, this.inviterSessionPublicKey)),
            created_at: Math.floor(Date.now() / 1000),
            tags: [['p', this.inviterSessionPublicKey]],
        };

        return { channel, event: finalizeEvent(envelope, randomSenderKey) };
    }

    listen(inviterSecretKey: Uint8Array | DecryptFunction, nostrSubscribe: NostrSubscribe, onChannel: (channel: Channel, identity?: string) => void): Unsubscribe {
        if (!this.inviterSessionPrivateKey) {
            throw new Error("Inviter session key is not available");
        }
        
        const filter = {
            kinds: [MESSAGE_EVENT_KIND],
            '#p': [this.inviterSessionPublicKey],
        };

        return nostrSubscribe(filter, async (event) => {
            try {
                if (this.maxUses && this.usedBy.length >= this.maxUses) {
                    console.error("Invite has reached maximum number of uses");
                    return;
                }

                const decrypted = await nip44.decrypt(event.content, getConversationKey(this.inviterSessionPrivateKey!, event.pubkey));
                const innerEvent = JSON.parse(decrypted);

                if (!innerEvent.tags || !innerEvent.tags.some(([key, value]: [string, string]) => key === 'secret' && value === this.linkSecret)) {
                    console.error("Invalid secret from event", event);
                    return;
                }

                const innerDecrypt = typeof inviterSecretKey === 'function' ?
                    inviterSecretKey :
                    (ciphertext: string, pubkey: string) => Promise.resolve(nip44.decrypt(ciphertext, getConversationKey(inviterSecretKey, pubkey)));
    
                const inviteeIdentity = innerEvent.pubkey;
                this.usedBy.push(inviteeIdentity);

                const inviteeSessionPublicKey = await innerDecrypt(innerEvent.content, inviteeIdentity);
                const sharedSecret = hexToBytes(this.linkSecret);

                const name = event.id;
                const channel = Channel.init(nostrSubscribe, inviteeSessionPublicKey, this.inviterSessionPrivateKey!, false, sharedSecret, name);

                onChannel(channel, inviteeIdentity);
            } catch (error) {
                console.error("Error processing invite message:", error);
            }
        });
    }
}