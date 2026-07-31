import { deflateSync } from "node:zlib";
import { mkdirSync, writeFileSync } from "node:fs";

const crcTable = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k += 1) c = (c & 1) ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
function crc32(data) { let c = 0xffffffff; for (const byte of data) c = crcTable[(c ^ byte) & 255] ^ (c >>> 8); return (c ^ 0xffffffff) >>> 0; }
function chunk(type, data) { const name = Buffer.from(type); const out = Buffer.alloc(data.length + 12); out.writeUInt32BE(data.length, 0); name.copy(out, 4); data.copy(out, 8); out.writeUInt32BE(crc32(Buffer.concat([name, data])), data.length + 8); return out; }
function icon(size) {
  const rgba = Buffer.alloc(size * size * 4);
  const set = (x, y, [r, g, b, a = 255]) => { const i = (y * size + x) * 4; rgba.set([r, g, b, a], i); };
  const inside = (x, y) => x >= size * .14 && x <= size * .86 && y >= size * .08 && y <= size * .88 && !(y > size * .79 && (x < size * .27 || x > size * .73));
  for (let y = 0; y < size; y += 1) for (let x = 0; x < size; x += 1) {
    const edge = inside(x, y) && (!inside(x - 1, y) || !inside(x + 1, y) || !inside(x, y - 1) || !inside(x, y + 1));
    if (inside(x, y)) set(x, y, edge ? [37, 41, 34] : [255, 252, 243]);
  }
  const stroke = Math.max(2, Math.round(size * .055));
  const drawLine = (x0, y0, x1, y1, color) => { for (let y = y0; y <= y1; y += 1) for (let x = x0; x <= x1; x += 1) for (let w = 0; w < stroke; w += 1) set(x, y + w, color); };
  drawLine(Math.round(size*.29),Math.round(size*.34),Math.round(size*.70),Math.round(size*.34),[239,122,103]);
  drawLine(Math.round(size*.29),Math.round(size*.48),Math.round(size*.62),Math.round(size*.48),[109,167,139]);
  drawLine(Math.round(size*.29),Math.round(size*.62),Math.round(size*.68),Math.round(size*.62),[112,150,189]);
  const raw = Buffer.alloc((size * 4 + 1) * size); for (let y = 0; y < size; y += 1) rgba.copy(raw, y * (size * 4 + 1) + 1, y * size * 4, (y + 1) * size * 4);
  const ihdr = Buffer.alloc(13); ihdr.writeUInt32BE(size, 0); ihdr.writeUInt32BE(size, 4); ihdr.set([8, 6, 0, 0, 0], 8);
  return Buffer.concat([Buffer.from([137,80,78,71,13,10,26,10]), chunk("IHDR", ihdr), chunk("IDAT", deflateSync(raw)), chunk("IEND", Buffer.alloc(0))]);
}
mkdirSync("src-tauri/icons", { recursive: true });
for (const size of [32, 128, 256]) writeFileSync(`src-tauri/icons/${size}x${size}.png`, icon(size));
writeFileSync("src-tauri/icons/icon.png", icon(512));
