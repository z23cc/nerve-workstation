# Homebrew distribution

`ctx-mcp` (the binary from `crates/ctx-mcp`) is published through a Homebrew tap.

```bash
brew install z23cc/tap/ctx-mcp
ctx-mcp --version
```

## How it works

- **Tap repo:** [`z23cc/homebrew-tap`](https://github.com/z23cc/homebrew-tap)
  holds `Formula/ctx-mcp.rb`. The shorthand `z23cc/tap` expands to it.
- **macOS (Apple Silicon):** the formula ships a real Homebrew **bottle**. A bottle
  is *poured*, not built, so the install is instant (no compiler, no Rust/LLVM) and
  it **skips Homebrew's build-from-source Xcode-version gate** — which otherwise
  blocks installs on pre-release macOS (e.g. macOS 27 with an older Xcode).
- **Other macOS versions / Intel / Linux:** the bottle is tagged for the exact
  macOS it was built on (e.g. `arm64_golden_gate` for macOS 27). On a non-matching
  platform Homebrew falls back to building from source via `cargo install`
  (it installs a temporary `rust` build dependency).
- **Versioning:** starts at `0.0.1` and increments the patch by `+0.0.1` per
  release. The single source of truth is `version` under `[workspace.package]`
  in the root `Cargo.toml`; every crate inherits it via `version.workspace = true`.
- The release binary is stripped at link time (`[profile.release] strip = true`),
  which keeps a valid ad-hoc code signature. Do **not** run an external `strip`
  on the macOS binary — it corrupts the Mach-O and the binary is SIGKILLed on
  Apple Silicon even after re-signing.

## Cutting a release

Run from the repo root on a clean `main` (on an Apple Silicon Mac, so the bottle
gets built):

```bash
Scripts/release.sh            # bump +0.0.1, then release (0.0.1 -> 0.0.2 -> ...)
Scripts/release.sh --current  # release the CURRENT version without bumping
```

The script bumps the version, commits, tags `vX.Y.Z`, and publishes a GitHub
Release with a deterministic source tarball plus — when run on an arm64 Mac with
`brew` — a bottle named `ctx-mcp-X.Y.Z.<tag>.bottle.tar.gz`. It then regenerates
`Formula/ctx-mcp.rb` (adding a `bottle do` block whose `root_url` points at the
release) and pushes it to the tap. The bottle has a `--version` smoke test that
aborts the release if the binary cannot run.

## Notes / future work

- The bottle covers only the macOS version it was built on. To cover more macOS
  versions / Intel / Linux, build bottles on those hosts (or in CI) and add their
  `sha256 ... <tag>:` lines to the `bottle do` block.
- A GitHub Actions pipeline could build bottles for every platform automatically,
  but it needs a token with the `workflow` scope plus a cross-repo PAT to push the
  tap — so releases run locally for now.
- `brew test` spins up a build environment, so on pre-release macOS it hits the
  same Xcode gate and fails there; the actual install (pour) does not. The test
  block itself passes on a normally-configured machine.
