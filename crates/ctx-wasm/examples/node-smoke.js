// Runtime smoke for the wasm (nodejs) build.
// Build first:  wasm-pack build crates/ctx-wasm --target nodejs
const path = require('path');
const m = require(path.join(__dirname, '..', 'pkg', 'ctx_wasm.js'));

m.feed_workspace('alpha', JSON.stringify([
  { path: 'src/lib.rs',  content: 'pub fn needle_alpha() {}\npub struct Foo;\n' },
  { path: 'src/util.rs', content: 'use crate::Foo;\nfn run() { needle_alpha(); }\n' },
]));

m.feed_workspace('beta', JSON.stringify([
  { path: 'src/lib.rs', content: 'pub fn needle_beta() {}\npub struct Bar;\n' },
  { path: 'README.md',  content: 'needle_beta appears in beta docs\n' },
]));

const call = (name, args) => JSON.parse(m.handle_request(JSON.stringify({ name, arguments: args })));

const alpha = call('file_search', { workspace: 'alpha', pattern: 'needle_alpha', mode: 'content' });
const beta = call('file_search', { workspace: 'beta', pattern: 'needle_beta', mode: 'content' });
const isolated = call('file_search', { workspace: 'alpha', pattern: 'needle_beta', mode: 'content' });

const alphaMatches = alpha.structuredContent?.content_matches ?? [];
const betaMatches = beta.structuredContent?.content_matches ?? [];
const isolatedMatches = isolated.structuredContent?.content_matches ?? [];

console.log('alpha content_matches =', alphaMatches.length);
console.log('beta content_matches =', betaMatches.length);
console.log('alpha search for beta matches =', isolatedMatches.length);

if (alphaMatches.length !== 2) throw new Error('expected alpha workspace matches');
if (betaMatches.length !== 2) throw new Error('expected beta workspace matches');
if (isolatedMatches.length !== 0) throw new Error('workspace isolation failed');

const cs = call('get_code_structure', { workspace: 'beta', paths: ['src/lib.rs'] });
console.log('beta get_code_structure =', JSON.stringify(cs.structuredContent ?? cs).slice(0, 140));
const bc = call('build_context', { workspace: 'alpha', query: 'needle_alpha', token_budget: 200 });
console.log('alpha build_context token_used =', bc.structuredContent?.manifest?.token_used ?? bc.manifest?.token_used ?? '?');
console.log('WASM_NODE_SMOKE_OK');
