// Small helpers for base64 encoding/decoding across Node + browser.
// We intentionally avoid depending on transitive base64 libs.

export function base64Encode(bytes: Uint8Array): string {
  // Prefer Node's Buffer when available without introducing a hard dependency.
  const nodeBuffer = (globalThis as { Buffer?: { from: (...args: unknown[]) => Uint8Array & { toString: (encoding?: string) => string } } }).Buffer;
  if (nodeBuffer) {
    return nodeBuffer.from(bytes).toString("base64");
  }

  if (typeof btoa !== "function") {
    throw new Error("base64Encode: no base64 encoder available");
  }

  // btoa expects a binary string.
  let binary = "";
  const chunkSize = 0x8000;
  for (let i = 0; i < bytes.length; i += chunkSize) {
    const chunk = bytes.subarray(i, i + chunkSize);
    binary += String.fromCharCode(...chunk);
  }
  return btoa(binary);
}

export function base64Decode(b64: string): Uint8Array {
  const nodeBuffer = (globalThis as { Buffer?: { from: (...args: unknown[]) => Uint8Array } }).Buffer;
  if (nodeBuffer) {
    return new Uint8Array(nodeBuffer.from(b64, "base64"));
  }

  if (typeof atob !== "function") {
    throw new Error("base64Decode: no base64 decoder available");
  }

  const binary = atob(b64);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    out[i] = binary.charCodeAt(i);
  }
  return out;
}
