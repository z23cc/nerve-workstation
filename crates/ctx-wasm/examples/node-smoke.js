// Runtime smoke for the wasm (nodejs) build.
// Build first:  wasm-pack build crates/ctx-wasm --target nodejs
const path = require('path');
const m = require(path.join(__dirname, '..', 'pkg', 'ctx_wasm.js'));

m.feed_files(JSON.stringify([
  { path: 'src/lib.rs',  content: 'pub fn needle() {}\npub struct Foo;\n' },
  { path: 'src/util.rs', content: 'use crate::Foo;\nfn run() { needle(); }\n' },
  { path: 'README.md',   content: 'needle appears in docs too\n' },
]));

const call = (name, args) => JSON.parse(m.handle_request(JSON.stringify({ name, arguments: args })));

const fs = call('file_search', { pattern: 'needle', mode: 'content' });
console.log('file_search content_matches =', (fs.structuredContent?.content_matches ?? []).length);
const cs = call('get_code_structure', { paths: ['src/lib.rs'] });
console.log('get_code_structure =', JSON.stringify(cs.structuredContent ?? cs).slice(0, 140));
const bc = call('build_context', { query: 'needle', token_budget: 200 });
console.log('build_context token_used =', bc.structuredContent?.manifest?.token_used ?? bc.manifest?.token_used ?? '?');
console.log('WASM_NODE_SMOKE_OK');
