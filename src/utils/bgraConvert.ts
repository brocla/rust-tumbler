/**
 * Convert BGRA byte array to RGBA in-place.
 * Swaps the B and R channels (bytes 0 and 2 in each 4-byte pixel).
 */
export function bgraToRgba(buffer: ArrayBuffer): Uint8ClampedArray {
  const bytes = new Uint8ClampedArray(buffer);
  for (let i = 0; i < bytes.length; i += 4) {
    const b = bytes[i];
    bytes[i] = bytes[i + 2]; // R
    bytes[i + 2] = b; // B
  }
  return bytes;
}
