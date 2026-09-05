// Generates src-tauri/icons/icon-source.png (1024x1024 RGBA) without any
// dependencies — a minimal PNG encoder using node:zlib.
// The tauri CLI then derives icon.ico + all PNG sizes from this source.

import { deflateSync } from "node:zlib";
import { mkdirSync, writeFileSync } from "node:fs";

const W = 1024;
const H = 1024;

const raw = Buffer.alloc(H * (W * 4 + 1));

for (let y = 0; y < H; y++) {
  raw[y * (W * 4 + 1)] = 0; // filter: none
  for (let x = 0; x < W; x++) {
    const i = y * (W * 4 + 1) + 1 + x * 4;
    let r = 37;
    let g = 99;
    let b = 235;

    // White downward arrow: triangle + bar.
    const inBar = y >= 740 && y <= 800 && x >= 256 && x <= 768;
    const halfW = ((y - 430) / 260) * 256;
    const inTri = y >= 430 && y <= 690 && Math.abs(x - 512) <= halfW;

    if (inTri || inBar) {
      r = 255;
      g = 255;
      b = 255;
    }

    raw[i] = r;
    raw[i + 1] = g;
    raw[i + 2] = b;
    raw[i + 3] = 255;
  }
}

// --- minimal PNG writer ---

const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) {
      c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    }
    table[n] = c >>> 0;
  }
  return table;
})();

function crc32(buf) {
  let c = 0xffffffff;
  for (const byte of buf) {
    c = CRC_TABLE[(c ^ byte) & 0xff] ^ (c >>> 8);
  }
  return (c ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const typeBuf = Buffer.from(type, "ascii");
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(Buffer.concat([typeBuf, data])));
  return Buffer.concat([len, typeBuf, data, crc]);
}

const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(W, 0);
ihdr.writeUInt32BE(H, 4);
ihdr[8] = 8; // bit depth
ihdr[9] = 6; // color type: RGBA

const png = Buffer.concat([
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  chunk("IHDR", ihdr),
  chunk("IDAT", deflateSync(raw)),
  chunk("IEND", Buffer.alloc(0)),
]);

mkdirSync("src-tauri/icons", { recursive: true });
writeFileSync("src-tauri/icons/icon-source.png", png);
console.log("wrote src-tauri/icons/icon-source.png");