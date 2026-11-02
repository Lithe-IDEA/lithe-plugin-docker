// Browser equivalents of the host's open/save dialogs for the loopback preview.
// Selected upload bytes are staged in preview-owned temporary storage. Downloads
// are served only through unguessable tickets after the real backend wrote them.
import { randomUUID } from 'node:crypto';
import { createWriteStream, createReadStream } from 'node:fs';
import { mkdir, rm, stat } from 'node:fs/promises';
import { join, basename } from 'node:path';
import { Transform } from 'node:stream';
import { pipeline } from 'node:stream/promises';

const MAXIMUM_BYTES = 256 * 1024 * 1024;

export class PreviewFiles {
  constructor(state) {
    this.directory = join(state, 'preview-files');
    this.files = new Map();
  }

  async save(name) {
    if (this.files.size >= 16) throw new Error('Too many preview file operations; finish or restart the preview.');
    if (typeof name !== 'string' || !name || name === '.' || name === '..' || name.length > 255 || basename(name) !== name || /[\u0000-\u001f\\]/.test(name)) throw new Error('Invalid preview filename.');
    await mkdir(this.directory, { recursive: true });
    const token = randomUUID();
    const directory = join(this.directory, token);
    await mkdir(directory, { mode: 0o700 });
    const path = join(directory, name);
    this.files.set(token, { path, directory, name, complete: false, upload: false });
    return { path, token };
  }

  async route(url, req, res, origin) {
    if (!url.pathname.startsWith('/__preview-files/')) return false;
    if (req.method === 'POST' && req.headers.origin !== origin) { res.writeHead(403).end(); return true; }
    if (url.pathname === '/__preview-files/upload' && req.method === 'POST') {
      const record = await this.save(url.searchParams.get('name'));
      const file = this.files.get(record.token);
      file.upload = true;
      let bytes = 0;
      try {
        await pipeline(req, new Transform({ transform(chunk, _, callback) {
          bytes += chunk.length;
          callback(bytes > MAXIMUM_BYTES ? new Error('The file exceeds 256 MiB.') : null, chunk);
        } }), createWriteStream(record.path, { flags: 'wx', mode: 0o600 }));
        file.complete = true;
        res.writeHead(200, { 'Content-Type': 'application/json' }).end(JSON.stringify({ path: record.path }));
      } catch (error) {
        this.files.delete(record.token); await rm(file.directory, { recursive: true, force: true }); throw error;
      }
      return true;
    }
    const token = url.pathname.slice('/__preview-files/'.length);
    const file = this.files.get(token);
    if (req.method !== 'GET' || !file || file.upload || !file.complete) { res.writeHead(404).end(); return true; }
    const info = await stat(file.path);
    res.writeHead(200, { 'Content-Type': 'application/octet-stream', 'Content-Length': info.size,
      'Content-Disposition': `attachment; filename*=UTF-8''${encodeURIComponent(file.name)}`, 'Cache-Control': 'no-store' });
    try { await pipeline(createReadStream(file.path), res); }
    finally { this.files.delete(token); await rm(file.directory, { recursive: true, force: true }); }
    return true;
  }

  async finish(fn, params, success) {
    const entry = [...this.files].find(([, file]) => file.path === (fn === 'files.download' ? params.destination : params.source));
    if (!entry) return null;
    const [token, file] = entry;
    if (fn === 'files.download' && success) { file.complete = true; return `/__preview-files/${token}`; }
    this.files.delete(token); await rm(file.directory, { recursive: true, force: true }); return null;
  }
}
