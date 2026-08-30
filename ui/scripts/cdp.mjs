// Headless Chrome driver over the DevTools Protocol, for UI smoke tests.
// Usage: node scripts/cdp.mjs <url> <outdir> [step ...]
//   shot:<name>      screenshot -> <outdir>/<name>.png
//   eval:<js>        Runtime.evaluate (awaits promises), prints result JSON
//   click:<selector> querySelector(sel).click(), errors if missing
//   type:<text>      Input.insertText
//   key:<key>        keyDown+keyUp, e.g. Enter, Escape, ArrowDown, Ctrl+o
//   wait:<ms>        sleep
//   waitfor:<js>     poll expression every 100ms until truthy (10s timeout)
// Console messages / page exceptions are echoed to stderr. Exit 1 on failure.
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync, mkdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const die = (msg) => { throw new Error(msg); };

class CDP {
  constructor(ws) {
    this.ws = ws; this.id = 0; this.pending = new Map(); this.handlers = new Map();
    ws.addEventListener('message', (ev) => {
      const msg = JSON.parse(ev.data);
      if (msg.id !== undefined) {
        const p = this.pending.get(msg.id); if (!p) return;
        this.pending.delete(msg.id);
        msg.error ? p.reject(new Error(`${msg.method ?? 'cdp'}: ${msg.error.message}`)) : p.resolve(msg.result);
      } else for (const fn of this.handlers.get(msg.method) ?? []) fn(msg.params);
    });
  }
  send(method, params = {}) {
    const id = ++this.id;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject: (e) => reject(new Error(`${method} failed: ${e.message}`)) });
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }
  on(method, fn) { this.handlers.set(method, [...(this.handlers.get(method) ?? []), fn]); }
}

async function evaluate(cdp, expression) {
  const { result, exceptionDetails } = await cdp.send('Runtime.evaluate', {
    expression, awaitPromise: true, returnByValue: true,
  });
  if (exceptionDetails) die(`eval threw: ${exceptionDetails.exception?.description ?? exceptionDetails.text}`);
  return result.value;
}

const MODS = { alt: 1, ctrl: 2, meta: 4, shift: 8 };
async function pressKey(cdp, spec) {
  const parts = spec.split('+');
  const key = parts.pop();
  const modifiers = parts.reduce((m, p) => m | (MODS[p.toLowerCase()] ?? die(`unknown modifier: ${p}`)), 0);
  const named = { Enter: [13, '\r'], Escape: [27, ''], Tab: [9, '\t'], Backspace: [8, ''], Delete: [46, ''],
    ArrowDown: [40, ''], ArrowUp: [38, ''], ArrowLeft: [37, ''], ArrowRight: [39, ''], Home: [36, ''], End: [35, ''] };
  let code, vk, text;
  if (named[key]) { [vk, text] = named[key]; code = key; }
  else if (key.length === 1) {
    vk = key.toUpperCase().charCodeAt(0);
    code = /[a-z]/i.test(key) ? `Key${key.toUpperCase()}` : /[0-9]/.test(key) ? `Digit${key}` : key;
    text = modifiers & (MODS.ctrl | MODS.meta | MODS.alt) ? '' : key;
  } else die(`unsupported key: ${key}`);
  const base = { key, code, windowsVirtualKeyCode: vk, nativeVirtualKeyCode: vk, modifiers };
  await cdp.send('Input.dispatchKeyEvent', { ...base, type: text ? 'keyDown' : 'rawKeyDown', text, unmodifiedText: text });
  await cdp.send('Input.dispatchKeyEvent', { ...base, type: 'keyUp' });
}

async function runStep(cdp, step, outdir) {
  const i = step.indexOf(':');
  if (i < 0) die(`bad step (expected kind:arg): ${step}`);
  const kind = step.slice(0, i), arg = step.slice(i + 1);
  switch (kind) {
    case 'shot': {
      const { data } = await cdp.send('Page.captureScreenshot', { format: 'png' });
      const file = join(outdir, `${arg}.png`);
      writeFileSync(file, Buffer.from(data, 'base64'));
      console.log(`[shot] ${file}`);
      return;
    }
    case 'eval':
      console.log(JSON.stringify(await evaluate(cdp, arg)));
      return;
    case 'click':
      await evaluate(cdp, `(() => { const el = document.querySelector(${JSON.stringify(arg)});
        if (!el) throw new Error('no element for selector: ' + ${JSON.stringify(arg)});
        el.scrollIntoView({ block: 'center' }); el.click(); return true; })()`);
      return;
    case 'type':
      await cdp.send('Input.insertText', { text: arg });
      return;
    case 'key':
      await pressKey(cdp, arg);
      return;
    case 'wait':
      await sleep(Number(arg) || 0);
      return;
    case 'waitfor': {
      for (let t = 0; t < 10000; t += 100) {
        if (await evaluate(cdp, `!!(${arg})`)) return;
        await sleep(100);
      }
      die(`waitfor timed out after 10s: ${arg}`);
      return;
    }
    default: die(`unknown step kind: ${kind}`);
  }
}

async function main() {
  const [url, outdir, ...steps] = process.argv.slice(2);
  if (!url || !outdir) die('usage: node scripts/cdp.mjs <url> <outdir> [step ...]');
  mkdirSync(outdir, { recursive: true });
  const port = 10000 + Math.floor(Math.random() * 40000);
  const profile = mkdtempSync(join(tmpdir(), 'cdp-profile-'));
  const chrome = spawn('google-chrome-stable', ['--headless=new', '--disable-gpu', '--no-sandbox',
    `--remote-debugging-port=${port}`, '--window-size=1400,900', `--user-data-dir=${profile}`, 'about:blank'],
    { stdio: ['ignore', 'ignore', 'ignore'] });
  const cleanup = () => { try { chrome.kill('SIGKILL'); } catch {} rmSync(profile, { recursive: true, force: true }); };
  process.on('exit', cleanup);

  try {
    const base = `http://127.0.0.1:${port}`;
    for (let t = 0; ; t += 100) {
      try { await (await fetch(`${base}/json/version`, { signal: AbortSignal.timeout(1000) })).text(); break; }
      catch { if (t > 20000) die('Chrome did not start'); await sleep(100); }
    }
    const target = await (await fetch(`${base}/json/new?about:blank`, { method: 'PUT' })).json();
    const ws = new WebSocket(target.webSocketDebuggerUrl);
    await new Promise((res, rej) => { ws.addEventListener('open', res); ws.addEventListener('error', () => rej(new Error('websocket failed'))); });
    const cdp = new CDP(ws);
    cdp.on('Runtime.consoleAPICalled', (p) =>
      console.error(`[console] ${p.type}: ${p.args.map((a) => a.value ?? a.description ?? a.type).join(' ')}`));
    cdp.on('Runtime.exceptionThrown', (p) =>
      console.error(`[exception] ${p.exceptionDetails.exception?.description ?? p.exceptionDetails.text}`));

    await cdp.send('Page.enable');
    await cdp.send('Runtime.enable');
    const loaded = new Promise((res) => cdp.on('Page.loadEventFired', res));
    await cdp.send('Page.navigate', { url });
    await Promise.race([loaded, sleep(30000).then(() => die(`page load timed out: ${url}`))]);
    for (const step of steps) await runStep(cdp, step, outdir);
    ws.close();
  } finally { cleanup(); }
}

main().then(() => process.exit(0), (err) => { console.error(`error: ${err.message}`); process.exit(1); });
