# Frontier — L3 merge-bar enforcement, L6 ML calibrator, sigstore-keyless issuer identity

Status: **governing (design)** — the invariant-safe design for the three trust-substrate items deliberately
left unbuilt while L0–L6 were wired (PRs #5–#16). Each was adversarially critiqued against
INV-R1..R6 (`docs/designs/trust-substrate.md` §4); the designs below incorporate the fixes that critique
forced. **Read this before opening a build wave on any of them.** Nothing here is implemented yet.
Date: 2026-06-26.

These three are "the frontier" because each carries real **invariant blast radius** — done naively, L3 turns
the court reporter into a judge (INV-R1), L6 puts ML in the determinism kernel and lets an advisory signal go
load-bearing (INV-R2/R3), and sigstore breaks the receipt's offline portability (INV-R5). They are not hard
because the seams are missing (they all exist); they are hard because the *discipline* is unforgiving.

---

## 0. Cross-cutting guardrails (binding for all three)

1. **Determinism boundary is absolute (INV-R2).** Every pure decision/fold lives in `nerve-core` / `nerve-proto`
   over *already-content-addressed, wall-clock-excluded* data and is golden-tested byte-for-byte. All disk reads,
   policy loading, ML inference, network, certs, and clocks live in `nerve-runtime` / `nerve-workstation`.
2. **Court reporter, downgrade-only, never fabricate a pass (INV-R1).** No surface may *upgrade* a verdict to
   success or invent a clearance the receipt's own borrowed verdict did not support. New gating logic may only
   keep a pass or **downgrade** it to failure/neutral.
3. **Verdict stays execution-grounded; advisory signals are tiered (INV-R3).** The authoritative verdict bottoms
   out in the org's own checks. ML/log/judge signals are advisory, quarantined, and structurally unable to reach
   a verdict.
4. **Pin what is *signed*, not mutable host state, and fail to NEUTRAL/REFUSE — never to a silent weaker pass
   (INV-R1/R5).** Anything load-bearing must be covered by the receipt's signature; a host-local input must never
   be able to relax the gate.
5. **Enforcement runs strictly AFTER the wave-7 signature/integrity re-verification** (`gate.rs` verifies before
   `gate_outcome`); a receipt that fails `verify_receipt` is refused before any new logic is consulted.
6. **Neutrality + own-nothing-upstream + additive/versioned protocol (INV-R4/R5/R6).** Ship no first-party
   generation *or scoring* model and no trainer in any release/dist (release-check enforced). Protocol changes
   are additive and drift-checked; receipt-id churn is documented and all affected golden snapshots are
   regenerated **in the same PR**.

---

## 1. L3 — merge-bar enforcement  ·  build-NEXT  ·  invariant-risk: high (manageable)

**Goal.** Make `PolicyDoc.merge_bar.required_checks` + `required_evidence` — declared, sealed into
`policy_version`, served by `policy.get`, today **inert** — actually gate a merge, so a receipt reads `success`
(exit 0) only when the org's own declared required checks are all present-and-passed in the receipt and the
org's required evidence exists. Today `gate_outcome(receipt)` (`nerve-core/src/receipt_gate.rs:50-73`) maps only
the aggregate `VerdictStatus`, and `policy_version` is always `None` (`verify_runner.rs` issuance). This is the
substrate's headline lie: the org's declared bar sits unused.

**Why it stays court-reporter (INV-R1/R3).** The bar is the **org's own** declared requirement over the **org's
own** checks (`receipt.statement.checks`, borrowed verbatim from L2). Requiring "check X the org named must
appear and pass" is set-membership + status-equality over receipt-resident data — execution-grounded, never an
invented judgment. We attest *the org's bar was cleared by the org's checks*; we never assert "correct."

### Design (corrected by critique)

- **Pure kernel overlay (`nerve-core`, INV-R2).** Add
  `enforce_merge_bar(receipt, &MergeBar, &[EvidenceRequirement], pinned_policy_version: &str) -> GateOutcome`.
  It computes the existing `gate_outcome(receipt)` (unchanged), then applies a **downgrade-only** overlay over
  data already in the receipt. **No `Path`, no `fs`, no `PolicyPlane`, no clock** — the bar arrives as plain
  `nerve_proto::policy` data. Golden-tested as a pure fn.
- **Downgrade-only fold (INV-R1).** `base = gate_outcome(receipt)`. If `base.exit_code != 0`, return it with the
  bar report **appended** to the summary (never replacing the base rationale, never upgrading). If `base` is
  success but a required check is present-and-`Failed` → exit 1; if a required check is **missing** or evidence
  is **absent** → exit 2 (neutral — bar not exercised/incomplete, never a fabricated pass). Empty bar = pure
  pass-through (today's behavior; no regression).
- **🔒 Pin the bar the RECEIPT signed, not the host's disk (INV-R5 — critique's #1 fix).** The load-bearing bar
  MUST be the one the receipt pinned via `statement.provenance.policy_version`, resolved from a sealed/
  content-addressed policy the verifier fetches *by that version* — **never** the gate host's live
  `<root>/.nerve/policy-plane.json` (a malicious host could strip the bar). `pinned != in-force` resolves to
  **neutral drift (exit 2)** surfacing both versions; it never silently relaxes.
- **🔒 Bind check identity to content, not display name (INV-R1 — critique's #2 fix).** `required_checks` must
  not be satisfiable by a renamed or stubbed (`command:'true'`) check impersonating the org's real one. Match
  against a stable, content-addressed per-check identity derived from the sealed checkspec — not the free-form
  `ReceiptCheck.name`. (If only name-matching is initially feasible, the *checkspec hash* the receipt pinned must
  be the one the bar was authored against — surfaced as drift otherwise.)
- **Closed evidence-kind enum, fail-closed (INV-R3).** `EvidenceRequirement.kind` consumed by the gate is a
  CLOSED set of presence/identity predicates over receipt-resident provenance: `receipt` (exists), `replay`
  (non-empty `replay_manifest.root_hash`), `ledger` (`provenance.ledger_ref.is_some()`), `policy`
  (`provenance.policy_version == pinned`). Unknown kind = unsatisfied. **No** threshold/coverage/"diff touches
  tested files" predicates — those drift toward a judge.
- **Stamp `policy_version` at seal (INV-R5).** Thread the in-force `policy_version` into `issue_receipt_for_run`
  (was `None`) so it is part of the signed statement; a tampered version then breaks the wave-7 signature
  refusal. This changes the canonical statement bytes → **receipt-id migration**: regenerate + review all golden
  receipt snapshots in the same PR; document the churn.
- **Route the decision to L1.** Write a `PolicyDecisionRecord{ tool:"gate", policy_version, decision, reason:
  bar-clearance summary }` via the existing `LedgerEvidenceSink` (best-effort; degrades to `NullEvidenceSink`).

**Does NOT:** invent/score correctness; add a check the org didn't declare; upgrade a verdict; re-derive any
check; move IO into the kernel; bump `RUNTIME_PROTOCOL_VERSION` (MergeBar/EvidenceRequirement/policy_version
already exist); weaken the wave-7 refusal; make absent policy a hard error (empty sealed bar = pass-through only
when the receipt pinned the empty version).

**Build shape:** one green PR — pure `enforce_merge_bar` (golden) + populate `policy_version` at issuance
(+ golden migration) + gate.rs wiring after the wave-7 verify + L1 decision record. No proto bump.

---

## 2. Sigstore-keyless issuer identity  ·  build-LATER  ·  invariant-risk: high

**Goal.** `LocalEd25519Signer` self-mints a per-host key — `verify_receipt` proves tamper-evidence +
self-consistency but **not issuer identity**; wave-7 `--trusted-key` pins a *manually distributed* key. Real
sigstore-keyless (Fulcio short-lived cert bound to an OIDC workload identity + Rekor transparency-log inclusion)
establishes *who* signed without key distribution. The `Signer` trait, the `SigstoreKeylessSigner` stub, and the
`ReceiptSignature.bundle` field already exist.

### Design (corrected by critique)

- **🔒 Do NOT touch the kernel arity (INV-R2 — critique's #1 fix).** `nerve_core::receipt::verify_receipt` keeps
  its fixed 3-arg predicate `verify_sig(public_key, pae, sig) -> bool` and checks ONLY
  `statement_intact + predicate-said-yes`. **All** bundle / Fulcio-chain / SAN / OIDC-issuer / Merkle /
  inclusion-proof / SET / trust-root logic lives in a host verifier in `nerve-workstation`. nerve-proto's only
  change is the doc comment on the already-present `bundle: Option<String>` (regenerate the export).
- **🔒 Rekor inclusion proof + SET are MANDATORY/load-bearing (INV-R3 — critique fix).** For
  `backend = sigstore-keyless`, a valid Rekor inclusion proof + signed entry timestamp reconciling to the pinned
  checkpoint is **required** for `signature_valid` — never advisory, never warn-and-pass. Missing/invalid/forged
  log entry → refuse.
- **🔒 Offline at gate time (INV-R5 — critique fix).** The Fulcio chain, Rekor inclusion proof, SET, and
  checkpoint are **embedded in `bundle`**; only the versioned Sigstore trust roots are local. **No network call**
  in `nerve gate`. Trust-root staleness is a **first-class REFUSE reason** ("trust-roots-stale-or-missing"),
  distinct from "forged" — never a silent pass.
- **🔒 Identity pin compares the PROVEN cert SAN + OIDC issuer, never `keyid` (INV-R5 — carries wave-7's
  anti-spoof discipline).** Require BOTH `--trusted-identity` (matching the cert SAN — glob allowed but must
  include the workload path, not just the org) AND `--trusted-issuer`. Issuer-only or repo-only pinning lets any
  CI job under that issuer mint a gating receipt — refuse to pin on under-specification.
- **Opt-in, feature-gated, fallback intact (INV-R2/R6).** `sigstore-rs` pulls a large async (tokio) TLS/x509/
  protobuf tree that clashes with the synchronous workstation → behind a `sigstore` cargo feature, confined to
  the `nerve verify` / CI *signing* path; the daemon's per-capture issuance and `LocalEd25519` + `--trusted-key`
  stay the default/fallback, unweakened. Default to the **public** Fulcio/Rekor/TUF roots so receipts stay
  universally re-verifiable; a self-hosted instance stamps its trust-root identity into the output.
- **Testability split.** The SIGN ceremony (OIDC/network) is integration-tested behind the feature against a
  staging Fulcio/Rekor — not golden. The VERIFY path is a *pure function* of `bundle + pae + roots + pins` and
  MUST be deterministically golden-tested with checked-in fixture bundles: valid, tampered-cert, stripped-Rekor,
  SAN/issuer-mismatch, stale-root.

**Court reporter:** sigstore changes only *who signed* (signature provenance) — never *what* the receipt asserts;
the signed payload is the identical DSSE PAE over the borrowed-verdict statement (INV-R1).

**Depends on L3** (logical): the identity pin makes a *verdict* trustworthy, which is only meaningful once the
merge bar that verdict clears is real.

---

## 3. L6 — trained ML calibrator  ·  KEEP-DEFERRED  ·  invariant-risk: high

**Goal.** The `OutcomeCalibrator` seam ships `CorpusCalibrator` (pure deterministic fold) + `NoCalibrator`. A
*trained* model (flaky / contamination / ship-likelihood prediction) is the deferred upgrade. The design exists
so that *if/when* a meaningful labeled corpus accumulates, the build is grounded — but per §3 L6 this is the
**late dividend, NOT the day-one moat**, and the critique flagged it `needs-changes` with high risk. **Do not
build it next.**

### Design (corrected by critique) — for when the corpus justifies it

- **🔒 Structural non-load-bearing, not documentary (INV-R1/R3 — critique's headline fix).** The `model_calibration`
  envelope and the `ModelCalibrator` type are produced ONLY on the read-side `outcome.query` / `nerve_outcomes`
  path and must be **unreferenceable** from `verify_runner`, `receipt_gate`, the gate, the L1 ledger, or any
  Receipt — a compile-time/CI boundary, not a disclaimer string.
- **🔒 Kernel stays pure (INV-R2 — critique fix).** Any `outcome_features` fold in `nerve-core` may fold ONLY over
  already-content-addressed, golden-stable, wall-clock-excluded fields (label dispositions + Verdict
  CheckStatus/CheckKind/exit_code/timed_out/reproducible/runs/passed) — **not** raw Run-DAG events with timing.
  The model, inference runtime, floats, and subprocess live above the boundary.
- **🔒 Fail-LOUD, never silent substitution (INV-R3).** A configured-but-missing/corrupt/erroring/timed-out model
  emits `{ kind:"model", available:false, error, model_digest }` — never a silent `CorpusCalibrator` value
  dressed as the model's.
- **Non-reproducible + non-portable, stated (INV-R5).** The prediction is host-local, tagged `deterministic:false`,
  excluded from golden snapshots, and explicitly NOT part of any portable Receipt or re-verification closure.
- **Neutrality (INV-R4/R6).** Nerve ships NO weights and NO trainer in any release/dist (release-check enforced);
  the model is operator-supplied, content-addressed data (loaded, not compiled), trained offline/externally, and
  scored via a sandboxed subprocess sidecar with a hard timeout + resource bounds emitting a scalar permille
  triple ONLY. **No `explanation`/narrative field** — that is the neutrality soft spot (it trends toward
  generative judgment).
- **Feature-contract pinning (INV-R3).** A `features_version` is stamped in both the runtime `OutcomeFeatures`
  fold and the model artifact; mismatch → `available:false` (prevents training-serving skew producing a
  confidently-wrong advisory).

**Why deferred:** it needs (a) a real labeled corpus (merge/revert/incident outcomes — currently human/CI-fed and
sparse) and (b) the structural non-load-bearing bones above built *first*; calibration over an empty corpus is
the honest `None` we already ship.

---

## 4. Sequencing & recommendation

| Item | Value | Inv-risk | Verdict | Recommendation |
|---|---|---|---|---|
| **L3 merge-bar enforcement** | **High** — closes the substrate's headline lie (declared bar sits inert) | high (manageable) | needs-changes → designed | **build next** (one green PR; no proto bump) |
| **Sigstore-keyless** | High (strategic) — real issuer identity vs manual key distribution | high | needs-changes → designed | **build later** (after L3; feature-gated; opt-in) |
| **L6 ML calibrator** | Low–Medium (late dividend) | high | needs-changes → designed | **keep deferred** (needs a real corpus + the read-side bones) |

**Build order.** **PR1: L3 merge-bar** — all seams exist, no proto change, fully grounded; the only one that is
both highest-value and lowest-friction. Then, only if demand warrants: **sigstore-keyless** behind a `sigstore`
feature on the `nerve verify` signing path. **L6 ML stays deferred** until the outcome corpus is real; ship the
deterministic calibrator + flaky-rates (already done, PR #10/#16) as the standing answer.

**The line that governs all of it:** these features change *what we can prove about a run* (its bar was cleared,
who signed it, how it calibrates) — they must **never** change *what we assert* (the org's borrowed verdict). The
day any of them lets Nerve say "this code is correct," it is a bug against INV-R1, not a feature.

---

## 5. Relationship to other docs

Extends `trust-substrate.md` §3 (L3/L4/L6) and §4 (INV-R1..R6) — does not weaken any invariant. L3 makes
`MergeBar`/`EvidenceRequirement` (already in `nerve-proto`) load-bearing; sigstore fills the `Signer` /
`ReceiptSignature.bundle` seam; L6 fills the `OutcomeCalibrator` seam. Governance (`architecture-north-star.md`
§10): when this doc and the code disagree, fix one in the same PR.
