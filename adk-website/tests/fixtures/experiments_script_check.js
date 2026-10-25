// Runs the shipped experiments.js (spec 3.5 §7.12) in a stubbed browser and checks its
// protocol: arms, the redirect to the variant's file, one exposure per tab session, goals
// once, attribution from a fragment, and no failure when sessionStorage throws.
// Usage: node experiments_script_check.js <path to experiments.js>
const fs = require('fs');
const vm = require('vm');
const src = fs.readFileSync(process.argv[2], 'utf8');
function browser(data, opts = {}) {
  const sent = [], listeners = {}, storage = opts.storage || new Map();
  let replaced = null;
  const ctx = {
    JSON, Math, parseFloat, Uint32Array,
    document: { currentScript: { dataset: data }, addEventListener: (t, f) => (listeners[t] = listeners[t] || []).push(f) },
    navigator: { sendBeacon: (url, body) => { sent.push([url, JSON.parse(body)]); return true; } },
    location: { hash: opts.hash || '', search: '?utm=x', replace: (u) => { replaced = u; } },
    crypto: { getRandomValues: (a) => { a[0] = opts.r0 ?? 0x9f2c41d0; a[1] = opts.r1 ?? 0; return a; } },
  };
  ctx.window = ctx;
  ctx.window.sessionStorage = { getItem: (k) => storage.has(k) ? storage.get(k) : null, setItem: (k, v) => storage.set(k, String(v)) };
  vm.runInNewContext(src, ctx);
  return { sent, listeners, storage, replaced: () => replaced, ctx };
}
const beacon = 'https://edge.example/awp/v2/beacon';
let ok = true; const check = (c, m) => { if (!c) { ok = false; console.log('FAIL', m); } else console.log('ok  ', m); };
const ctrl = { awpExperiment: 'headline', arm: 'a', allocation: '0.5', variant: '/_exp/headline/', beacon };
const h = (t) => { let x = 0x811c9dc5; for (let i = 0; i < t.length; i++) { x ^= t.charCodeAt(i); x = Math.imul(x, 0x01000193) >>> 0; } return x >>> 0; };
const armOf = (bucket) => (h('headline:' + bucket) / 4294967296 < 0.5 ? 'b' : 'a');
// 1. A bucket that lands in A: exposure a, no redirect.
let bucketA = null, bucketB = null; for (let i = 0; i < 100 && (!bucketA || !bucketB); i++) { const k = 'k' + i; if (armOf(k) === 'a') bucketA = bucketA || k; else bucketB = bucketB || k; }
let s = new Map([['awp-bucket', bucketA]]);
let b = browser(ctrl, { storage: s });
check(b.replaced() === null, 'arm A stays on the control');
check(b.sent.length === 1 && b.sent[0][1].name === 'exp.headline.a.exposure' && b.sent[0][0] === beacon, 'one exposure for A');
// reload: no second exposure
const b2 = browser(ctrl, { storage: s });
check(b2.sent.length === 0, 'one exposure per tab session');
// goal: form submit marked
const form = { getAttribute: (n) => (n === 'data-awp-goal' ? 'newsletter.signups' : null) };
b2.listeners.submit.forEach((f) => f({ target: form }));
b2.listeners.submit.forEach((f) => f({ target: form }));
check(b2.sent.length === 1 && b2.sent[0][1].name === 'exp.headline.a.newsletter.signups', 'goal once per session');
// click inside a marked form does not count; a marked button does
const input = { closest: (sel) => null };
b2.listeners.click.forEach((f) => f({ target: input }));
const button = { closest: (sel) => ({ getAttribute: () => 'cta.click' }) };
b2.listeners.click.forEach((f) => f({ target: button }));
check(b2.sent.length === 2 && b2.sent[1][1].name === 'exp.headline.a.cta.click', 'a marked button counts; a click elsewhere does not');
// 2. A bucket that lands in B: redirect to the variant keeping query and fragment, no exposure on the control
s = new Map([['awp-bucket', bucketB]]); b = browser(ctrl, { storage: s, hash: '#top' });
check(b.replaced() === '/_exp/headline/?utm=x#top', 'arm B goes to the variant file: ' + b.replaced());
check(b.sent.length === 0, 'no exposure counted on the redirecting control');
const v = browser({ awpExperiment: 'headline', arm: 'b', allocation: '0.5', beacon }, { storage: s });
check(v.sent.length === 1 && v.sent[0][1].name === 'exp.headline.b.exposure', 'the variant counts its exposure');
// 3. Fresh tab: bucket drawn from crypto and stored
s = new Map(); b = browser(ctrl, { storage: s, r0: 0x9f2c41d0, r1: 0 });
check(s.get('awp-bucket') === '9f2c41d000000000', 'random bucket kept in sessionStorage: ' + s.get('awp-bucket'));
// 4. Goal page with fragment attribution (an email link)
s = new Map(); const g = browser({ awpExperiment: 'headline', arm: '', beacon }, { storage: s, hash: '#awp-exp=headline.b' });
check(g.sent.length === 0, 'a goal page counts no exposure');
g.ctx.awpExperiments.goal('email.unsubscribe_rate');
check(g.sent.length === 1 && g.sent[0][1].name === 'exp.headline.b.email.unsubscribe_rate', 'fragment attributes the goal to B');
// 5. Rust/JS agreement for the Rust test vector bucket
check(h('headline:9f2c41d0') === 0x336899c6, 'hash vector');
// 6. sessionStorage throwing (private mode): still works, no crash
const throwing = browser(ctrl, { storage: { has() { throw new Error('denied'); }, get() { throw new Error('denied'); }, set() { throw new Error('denied'); } } });
check(throwing.sent.length === 1 || throwing.replaced() !== null, 'private mode still assigns and counts');
process.exit(ok ? 0 : 1);
