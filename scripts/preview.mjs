// Local UI verification against the real packaged backend and Docker engine.
// No example resources or alternate implementation of the plugin functions.
// The server is loopback-only; the backend is stopped with the server.
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { readFile, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve, extname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';
import { PreviewFiles } from './preview-files.mjs';

const root = fileURLToPath(new URL('..', import.meta.url));
// Verify the exact files users import, rather than a newer ui/dist build
// alongside an older packaged backend and archive.
const manifest = JSON.parse(await readFile(join(root, 'plugin.json'), 'utf8'));
const packageRoot = join(root, 'dist', manifest.id);
const { values } = parseArgs({ options: { port: { type: 'string', default: '4173' } } });
const port = Number(values.port);
if (!Number.isInteger(port) || port < 1024 || port > 65535) throw new Error('Choose a port from 1024 to 65535.');
const state = await mkdtemp(join(tmpdir(), 'lithe-docker-preview-'));
const previewFiles = new PreviewFiles(state);
const backend = spawn(join(packageRoot, manifest.backend.launch.path), ['--state', state], {
  cwd: state, stdio: ['pipe', 'pipe', 'inherit'],
});
const pending = new Map();
const calls = new Map();
let sequence = 0;
let buffer = Buffer.alloc(0);
let stopped = false;
function send(message) {
  const data = Buffer.from(JSON.stringify({ jsonrpc: '2.0', ...message }));
  backend.stdin.write(Buffer.concat([Buffer.from(`Content-Length: ${data.length}\r\n\r\n`), data]));
}
function request(method, params) {
  const id = ++sequence;
  const result = new Promise((accept, reject) => {
    const timer = setTimeout(() => {
      pending.delete(id); send({ method: '$/cancelRequest', params: { id } });
      reject({ code: 'requestTimeout', message: 'The preview backend did not answer in time.' });
    }, 60000);
    pending.set(id, { accept, reject, timer });
    send({ id, method, params });
  });
  return { id, result };
}
backend.stdout.on('data', chunk => {
  buffer = Buffer.concat([buffer, chunk]);
  while (true) {
    const head = buffer.indexOf('\r\n\r\n'); if (head < 0) break;
    const match = /Content-Length:\s*(\d+)/i.exec(buffer.subarray(0, head).toString());
    if (!match) throw new Error('Malformed backend frame');
    const length = Number(match[1]); if (buffer.length < head + 4 + length) break;
    const message = JSON.parse(buffer.subarray(head + 4, head + 4 + length).toString());
    buffer = buffer.subarray(head + 4 + length);
    const entry = pending.get(message.id);
    if (entry) {
      pending.delete(message.id); clearTimeout(entry.timer);
      if (message.error) entry.reject({ ...message.error, pluginErrorCode: message.error.data?.pluginErrorCode });
      else entry.accept(message.result);
    }
  }
});
backend.on('error', error => { console.error(error); void stop(1); });
backend.on('exit', () => { if (!stopped) void stop(1); });
const initialized = await request('initialize', {
  processId: process.pid, rootUri: `file://${state}/`,
  capabilities: { experimental: { litheFunctions: { versions: [1] } } },
}).result;
send({ method: 'initialized', params: {} });
const functions = initialized.capabilities.experimental.litheFunctions.functions.map(value => value.id);
let pageState = null;

const bridge = `
async function choosePreviewFile() {
  const input = document.createElement('input'); input.type = 'file'; input.hidden = true;
  document.body.append(input);
  try {
    const file = await new Promise(resolve => {
      input.addEventListener('change', () => resolve(input.files?.[0] || null), { once: true });
      input.addEventListener('cancel', () => resolve(null), { once: true });
      input.click();
    });
    if (!file) return { path: null };
    const response = await fetch('/__preview-files/upload?name=' + encodeURIComponent(file.name), { method: 'POST', body: file });
    const result = await response.json(); if (!response.ok) throw result.error;
    return result;
  } finally { input.remove(); }
}
window.lithe = {
  version: 2,
  context: { platform: 'macos', locale: new URL(location.href).searchParams.get('locale') || 'zh-CN', theme: new URL(location.href).searchParams.get('theme') || 'light' },
  on: () => () => {},
  request: async (method, params) => {
    if (method === 'dialog.openFile') return choosePreviewFile();
    if (method === 'functions.invoke' && params.function === 'browser.open') {
      const url = new URL(params.params.url);
      if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password) throw { code: 'docker.browserUnavailable' };
      const opened = window.open(url.href, '_blank');
      if (!opened) throw { code: 'docker.browserUnavailable' };
      opened.opener = null;
      return { value: { opened: true, source: 'web' } };
    }
    const response = await fetch('/__bridge', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ method, params }) });
    const answer = await response.json();
    if (answer.error) throw answer.error;
    if (answer.previewDownload) {
      const anchor = document.createElement('a'); anchor.href = answer.previewDownload;
      anchor.download = ''; document.body.append(anchor); anchor.click(); anchor.remove();
    }
    return answer.result;
  }
};
const dark = window.lithe.context.theme === 'dark';
document.documentElement.dataset.theme = dark ? 'dark' : 'light';
const variables = dark ? { background:'#1e1f22',surface:'#25262a',header:'#2b2d30','popup-background':'#2b2d30',foreground:'#dfe1e5','secondary-foreground':'#9da0a8',border:'#393b40',accent:'#548af7',success:'#59a869',error:'#f27675' } : { background:'#ffffff',surface:'#f7f8fa',header:'#f2f3f5','popup-background':'#ffffff',foreground:'#23252a','secondary-foreground':'#737780',border:'#e3e5e9',accent:'#3574f0',success:'#2f9454',error:'#c83b43' };
for (const [name, value] of Object.entries(variables)) document.documentElement.style.setProperty('--lithe-' + name, value);
`;
const mime = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml' };
const server = createServer(async (req, res) => {
  try {
    const url = new URL(req.url, `http://127.0.0.1:${port}`);
    if (await previewFiles.route(url, req, res, `http://127.0.0.1:${port}`)) return;
    if (url.pathname === '/__bridge' && req.method === 'POST') {
      if (req.headers.origin && req.headers.origin !== `http://127.0.0.1:${port}`) { res.writeHead(403).end(); return; }
      const chunks = []; let size = 0;
      for await (const chunk of req) { size += chunk.length; if (size > 1 << 20) throw new Error('Request too large'); chunks.push(chunk); }
      const { method, params } = JSON.parse(Buffer.concat(chunks).toString());
      let result;
      let previewDownload;
      if (method === 'ui.getState') result = { state: pageState };
      else if (method === 'ui.setState') { pageState = params.state; result = {}; }
      else if (method === 'functions.list') result = { functions };
      else if (method === 'dialog.saveFile') { result = { path: (await previewFiles.save(params.suggestedName)).path }; }
      else if (method === 'functions.cancel') {
        const id = calls.get(params.call); if (id) send({ method: '$/cancelRequest', params: { id } }); result = {};
      } else if (method === 'functions.invoke') {
        const operation = request('lithe/function/invoke', { functionID: params.function, params: params.params, operationID: params.call });
        calls.set(params.call, operation.id);
        let success = false;
        try { result = { value: await operation.result }; success = true; }
        finally {
          calls.delete(params.call);
          if (params.function === 'files.download' || params.function === 'files.uploadLocal') {
            previewDownload = await previewFiles.finish(params.function, params.params, success);
          }
        }
      } else throw new Error(`Unsupported preview method: ${method}`);
      res.writeHead(200, { 'Content-Type': 'application/json' }).end(JSON.stringify({ result, previewDownload }));
      return;
    }
    if (url.pathname === '/__preview-bridge.js') { res.writeHead(200, { 'Content-Type': 'text/javascript' }).end(bridge); return; }
    const path = url.pathname === '/' ? '/ui/index.html' : url.pathname;
    const base = path.startsWith('/icons/') ? packageRoot : join(packageRoot, 'ui');
    const relative = path.startsWith('/ui/') ? path.slice(4) : path.slice(1);
    const file = resolve(base, relative);
    if (!file.startsWith(`${base}/`)) { res.writeHead(403).end(); return; }
    let data = await readFile(file);
    if (extname(file) === '.html') data = Buffer.from(data.toString().replace('<script src="main.js">', '<script src="/__preview-bridge.js"></script><script src="main.js">'));
    res.writeHead(200, { 'Content-Type': mime[extname(file)] ?? 'application/octet-stream',
      'Content-Security-Policy': "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; form-action 'none'" }).end(data);
  } catch (error) {
    res.writeHead(500, { 'Content-Type': 'application/json' }).end(JSON.stringify({ error: { message: error.message, code: error.code, pluginErrorCode: error.pluginErrorCode } }));
  }
});
async function stop(code = 0) {
  if (stopped) return; stopped = true;
  server.closeAllConnections(); server.close();
  for (const entry of pending.values()) { clearTimeout(entry.timer); entry.reject(new Error('Preview stopped')); }
  backend.stdin.end(); backend.kill('SIGTERM');
  const force = setTimeout(() => backend.kill('SIGKILL'), 2000); force.unref();
  await rm(state, { recursive: true, force: true });
  process.exitCode = code;
}
process.on('SIGINT', () => void stop()); process.on('SIGTERM', () => void stop());
server.listen(port, '127.0.0.1', () => console.log(`Real Docker preview: http://127.0.0.1:${port}/ui/index.html`));
