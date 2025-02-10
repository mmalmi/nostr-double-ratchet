import { generateSecretKey, getPublicKey, nip44, finalizeEvent, VerifiedEvent, nip19 } from "nostr-tools";
import { base58 } from '@scure/base';
import { NostrSubscribe, Unsubscribe } from "./types";
import { getConversationKey } from "nostr-tools/nip44";
import { Channel } from "./Channel";
import { EVENT_KIND } from "./types";
import { EncryptFunction, DecryptFunction } from "./types";

/**
 * Invite link is a safe way to exchange session keys and initiate secret channels.
 * 
 * Even if inviter's or invitee's long-term private key (identity key) and the shared secret (link) is compromised,
 * forward secrecy is preserved as long as the session keys are not compromised.
 * 
 * Shared secret Nostr channel: inviter listens to it and invitees can write to it. Outside observers don't know who are communicating over it.
 * It is vulnerable to spam, so the link should only be given to trusted invitees or used with a reasonable maxUses limit.
 * 
 * Also make sure to keep the session key safe.
 */
export class InviteLink {
    constructor(
        public inviterSessionPublicKey: string,
        public linkSecret: string,
        public inviter: string,
        public inviterSessionPrivateKey?: Uint8Array,
        public label?: string,
        public maxUses?: number,
        public usedBy: string[] = [],
    ) {}

    static createNew(inviter: string, label?: string, maxUses?: number): InviteLink {
        const inviterSessionPrivateKey = generateSecretKey();
        const inviterSessionPublicKey = getPublicKey(inviterSessionPrivateKey);
        const linkSecret = base58.encode(generateSecretKey()).slice(8, 40);
        return new InviteLink(
            inviterSessionPublicKey,
            linkSecret,
            inviter,
            inviterSessionPrivateKey,
            label,
            maxUses
        );
    }

    static fromUrl(url: string): InviteLink {
        const parsedUrl = new URL(url);
        const inviter = parsedUrl.pathname.slice(1);
        const inviterSessionPublicKey = parsedUrl.searchParams.get('s');
        const linkSecret = parsedUrl.hash.slice(1);
        
        if (!inviter) {
            throw new Error("Inviter not found in the URL");
        }
        if (!inviterSessionPublicKey) {
            throw new Error("Session key not found in the URL");
        }

        const decodedInviter = nip19.decode(inviter);
        const decodedSessionKey = nip19.decode(inviterSessionPublicKey);

        if (typeof decodedInviter.data !== 'string') {
            throw new Error("Decoded inviter is not a string");
        }
        if (typeof decodedSessionKey.data !== 'string') {
            throw new Error("Decoded session key is not a string");
        }

        const inviterHexPub = decodedInviter.data;
        const inviterSessionPublicKeyHex = decodedSessionKey.data;
        
        return new InviteLink(inviterSessionPublicKeyHex, linkSecret, inviterHexPub);
    }

    static deserialize(json: string): InviteLink {
        const data = JSON.parse(json);
        return new InviteLink(
            data.inviterSessionPublicKey,
            data.linkSecret,
            data.inviter,
            data.inviterSessionPrivateKey ? new Uint8Array(data.inviterSessionPrivateKey) : undefined,
            data.label,
            data.maxUses,
            data.usedBy
        );
    }

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

    getUrl(root = "https://iris.to") {
        const url = new URL(`${root}/${nip19.npubEncode(this.inviter)}`)
        url.searchParams.set('s', nip19.npubEncode(this.inviterSessionPublicKey))
        url.hash = this.linkSecret
        return url.toString()
    }

    /**
     * Accepts the invite and creates a new channel with the inviter.
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
    async acceptInvite(
        nostrSubscribe: NostrSubscribe,
        inviteePublicKey: string,
        inviteeSecretKey: Uint8Array | EncryptFunction,
    ): Promise<{ channel: Channel, event: VerifiedEvent }> {
        const inviteeSessionKey = generateSecretKey();
        const inviteeSessionPublicKey = getPublicKey(inviteeSessionKey);
        const inviterPublicKey = this.inviter || this.inviterSessionPublicKey;

        const channel = Channel.init(nostrSubscribe, this.inviterSessionPublicKey, inviteeSessionKey, new Uint8Array(), undefined, true);

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
            kind: EVENT_KIND,
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
            kinds: [EVENT_KIND],
            '#p': [this.inviterSessionPublicKey],
        };

        return nostrSubscribe(filter, async (event) => {
            try {
                const decrypted = await nip44.decrypt(event.content, getConversationKey(this.inviterSessionPrivateKey!, event.pubkey));
                const innerEvent = JSON.parse(decrypted);

                if (!innerEvent.tags || !innerEvent.tags.some(([key, value]: [string, string]) => key === 'secret' && value === this.linkSecret)) {
                    console.error("Invalid secret from event", event);
                    return;
                }

                const innerDecrypt = typeof inviterSecretKey === 'function' ?
                    inviterSecretKey :
                    (ciphertext: string, pubkey: string) => Promise.resolve(nip44.decrypt(ciphertext, getConversationKey(inviterSecretKey, pubkey)));
    
                const inviteeSessionPublicKey = await innerDecrypt(innerEvent.content, innerEvent.pubkey);

                const name = event.id;
                const channel = Channel.init(nostrSubscribe, inviteeSessionPublicKey, this.inviterSessionPrivateKey!, new Uint8Array(), name, false);

                onChannel(channel, innerEvent.pubkey);
            } catch (error) {
                console.error("Error processing invite message:", error);
            }
        });
    }
}