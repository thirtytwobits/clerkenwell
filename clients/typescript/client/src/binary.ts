/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Binary payload codecs for projection transports that carry update frames as base64 JSON fields.
 */
/** Bytes per `String.fromCharCode` call, well inside every engine's argument limit. */
const ENCODE_CHUNK = 0x2000;

export function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  for (let start = 0; start < bytes.length; start += ENCODE_CHUNK) {
    binary += String.fromCharCode(...bytes.subarray(start, start + ENCODE_CHUNK));
  }
  return btoa(binary);
}

export function base64ToBytes(value: string): Uint8Array {
  const binary = atob(value);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }
  return bytes;
}
