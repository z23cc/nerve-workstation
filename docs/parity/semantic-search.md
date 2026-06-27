# semantic_search parity notes

> **Superseded / historical (2026-06-24):** the in-kernel `semantic`/ONNX engine and the
> `semantic_search` tool were **removed** (no `semantic` cargo feature exists). Semantic recall, if
> wanted, is now consumed via the MCP-client seam tagged `deterministic:false` — see
> `docs/designs/trust-substrate.md` (INV-R2) and `docs/designs/code-graph.md`. Kept as a parity record.
>
> **Re-entered (2026-06-27) as the consume-via-MCP `semantic_search` tool.** Implemented per the plan above:
> a workstation-layer `RuntimeToolAdapter` (`crates/nerve-workstation/src/mcp/semantic.rs`), NOT in
> `nerve-core`. It consumes an external embedding MCP server (configured via the `semantic` key in
> `--mcp-config`; the server's tool must return `structuredContent.hits = [{ path, score?, ranges?, note? }]`),
> returns scout-shaped citations tagged **`deterministic: false`** (new `ToolCapability.deterministic` field +
> an in-band result marker), and is **isolated from `build_context` by construction** — the kernel never calls
> it, so a captured Run's deterministic context stays bit-for-bit replayable (INV-R1). The old neural stack
> (fastembed/ONNX/ANN) is NOT rebuilt; any embedding MCP server plugs in. A `SemanticBackend` trait keeps a
> deterministic test fake swappable for the real backend.

`semantic_search` is available only in native builds compiled with `--features semantic`.
Default builds do not list the tool and keep the existing on-demand/no-persistent-index behavior.

## Semantic-feature behavior

- Index lifetime: built on first `semantic_search`, then cached in memory for the provider/session.
- Persistence: semantic mode writes a per-workspace index cache containing chunk metadata, raw `f32` embeddings, and ANN metadata. Cache identity includes canonical roots, embedding model id, embedding dimension, chunker version, and schema version.
- Incrementality: subsequent sessions load the persisted cache and re-embed only files whose `FileSignature { modified, size }` changed; removed/changed file chunks are tombstoned and compacted past threshold.
- Cache dirs: `--semantic-cache-dir` controls workspace semantic index artifacts. `--semantic-model-cache-dir` controls fastembed model downloads only.
- Retrieval: code-symbol chunks with fixed-window fallback, dense ANN candidates, real chunk-level BM25 with IDF/avg chunk length, RRF fusion, optional rerank.
- Test backend: deterministic mock embeddings/reranker; CI must not download models.

## Model defaults

- Embedding: `jina-embeddings-v2-base-code` (`EmbeddingModel::JinaEmbeddingsV2BaseCode`).
- Reranker: `bge-reranker-base` (`RerankerModel::BGERerankerBase`).
- Supported CLI aliases also include full Hugging Face repo names.

## Eval harness

Mock/offline fixture run:

```bash
cargo run -p nerve-core --example semantic_eval --features semantic -- \
  crates/nerve-core/tests/fixtures "config validation"
```

Real-model sample repo run (downloads on first semantic use):

```bash
cargo run -p nerve-core --example semantic_eval --features semantic -- \
  /abs/sample/repo "where is auth handled" --real
```

The harness prints rerank on/off latency, chunk count, result count, and the top result spans.
