import * as ed from "@noble/ed25519";
import { sha512 } from "@noble/hashes/sha2.js";

ed.hashes.sha512 = sha512;

const { bytesToHex, hexToBytes } = ed.etc;

export interface Keypair {
  secretKey: string; // 64 hex chars (32 bytes)
  publicKey: string; // 64 hex chars (32 bytes)
}

export function generateKeypair(): Keypair {
  const secretKey = ed.utils.randomSecretKey();
  const publicKey = ed.getPublicKey(secretKey);
  return {
    secretKey: bytesToHex(secretKey),
    publicKey: bytesToHex(publicKey),
  };
}

/** Builds the canonical JSON string that must be signed.
 *  Takes the message object and removes the `signature` field,
 *  then JSON-stringifies the rest. Key order matches insertion order. */
export function canonicalPayload(msg: Record<string, unknown>): string {
  const { signature, ...rest } = msg;
  return JSON.stringify(rest);
}

/** Signs and produces the hex-encoded ed25519 signature. */
export function sign(secretKeyHex: string, message: string): string {
  const sig = ed.sign(new TextEncoder().encode(message), hexToBytes(secretKeyHex));
  return bytesToHex(sig);
}

/** Verify a hex-encoded signature against a message and public key. */
export function verify(
  signatureHex: string,
  message: string,
  publicKeyHex: string,
): boolean {
  return ed.verify(
    hexToBytes(signatureHex),
    new TextEncoder().encode(message),
    hexToBytes(publicKeyHex),
  );
}

/** Helper class that holds a keypair and can sign messages. */
export class Signer {
  readonly secretKey: string;
  readonly publicKey: string;

  constructor(secretKeyHex: string) {
    this.secretKey = secretKeyHex;
    this.publicKey = bytesToHex(ed.getPublicKey(hexToBytes(secretKeyHex)));
  }

  static generate(): Signer {
    const kp = generateKeypair();
    return new Signer(kp.secretKey);
  }

  sign(message: string): string {
    return sign(this.secretKey, message);
  }

  signPayload(msg: Record<string, unknown>): string {
    const canonical = canonicalPayload(msg);
    return this.sign(canonical);
  }
}
