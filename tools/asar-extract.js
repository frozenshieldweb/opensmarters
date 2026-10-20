// Minimal Electron .asar extractor (no dependencies).
//
// Usage:
//   node tools/asar-extract.js <path/to/app.asar> <output-dir>
//
// Format: 8-byte pickle (uint32 payload size, uint32 header size), then a
// pickled JSON string describing the file tree; file bodies follow the header
// at `8 + headerSize + offset`.
const fs = require('fs');
const path = require('path');

const [src, dest] = process.argv.slice(2);
if (!src || !dest) {
  console.error('usage: node asar-extract.js <app.asar> <output-dir>');
  process.exit(1);
}

const fd = fs.openSync(src, 'r');
const sizeBuf = Buffer.alloc(8);
fs.readSync(fd, sizeBuf, 0, 8, 0);
const headerSize = sizeBuf.readUInt32LE(4);
const headerBuf = Buffer.alloc(headerSize);
fs.readSync(fd, headerBuf, 0, headerSize, 8);
const strLen = headerBuf.readUInt32LE(4);
const header = JSON.parse(headerBuf.slice(8, 8 + strLen).toString());
const base = 8 + headerSize;

let files = 0;
const unpacked = [];
const root = path.resolve(dest);

function walk(node, rel) {
  for (const [name, e] of Object.entries(node.files || {})) {
    const p = path.join(rel, name);
    const out = path.join(dest, p);
    if (!path.resolve(out).startsWith(root)) throw new Error('path escape: ' + p);
    if (e.files) {
      fs.mkdirSync(out, { recursive: true });
      walk(e, p);
    } else if (e.unpacked) {
      unpacked.push(p); // lives in app.asar.unpacked/ next to the archive
    } else if (e.link) {
      // symlinks are skipped
    } else {
      const buf = Buffer.alloc(e.size);
      if (e.size) fs.readSync(fd, buf, 0, e.size, base + Number(e.offset));
      fs.mkdirSync(path.dirname(out), { recursive: true });
      fs.writeFileSync(out, buf);
      files++;
    }
  }
}

walk(header, '');
console.log('extracted', files, 'files; unpacked entries:', unpacked.length);
