# Windows distribution (Scoop)

`Nerve Workstation` ships a Windows binary (`nerve.exe`) on every release, installable via
[Scoop](https://scoop.sh).

```powershell
scoop bucket add z23cc https://github.com/z23cc/scoop-bucket
scoop install nerve-workstation
nerve --version
```

## How it works

- **Binary:** the `windows` GitHub Actions workflow (`.github/workflows/windows.yml`)
  runs on `windows-latest` on every published release (and on manual dispatch): it
  runs the full test suite, builds a release `nerve.exe`, smoke-tests `--version`,
  and uploads `nerve.exe` + `nerve.exe.sha256` to the GitHub Release.
- **Bucket:** the same job then regenerates the Scoop manifest (version, exe url +
  sha256, `bin`, `checkver`/`autoupdate`) and pushes it to
  [`z23cc/scoop-bucket`](https://github.com/z23cc/scoop-bucket) at
  `bucket/nerve-workstation.json`, via the `SCOOP_DEPLOY_KEY` deploy key. This mirrors the
  Homebrew tap flow for macOS/Linux.
- **No compiler needed:** Scoop downloads the prebuilt `nerve.exe`.

## Notes

- The engine is portable (no Unix-only syscalls; catalog paths are normalized to
  `/`), so the same code that powers macOS/Linux runs on Windows.
- `nerve install` (Claude Code / Codex auto-config) auto-detects the CLIs by a
  bare name; on Windows it may not find `claude.cmd` / `codex.cmd` yet and will
  print the manual `claude mcp add` / `codex mcp add` command instead.
- **winget** is not published; it would require a manifest PR to
  `microsoft/winget-pkgs`.
