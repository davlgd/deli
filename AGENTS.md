# AGENTS.md

Guidance for coding agents working on DELI (Davlgd Exherbo Linux Installer). `README.md` is for users, `DESIGN.md` explains how and why DELI is built, including the Exherbo pitfalls already met: read it before changing the installer or the ISO.

## Layout

- `src/`: the `deli` binary (Rust, edition 2024, one crate).
  - `config.rs` parses and validates `deli.toml`; `storage.rs` turns it into a partition plan; `network.rs` writes the networkd and resolved settings, for the target and the live session, and checks the connection; `repositories.rs` reads which Exherbo repositories a root has and could add.
  - `system/` holds the `Sys` trait (`RealSys` acts, `DrySys` records), `probe.rs`, the read-only inspection of the machine, and `resolvers.rs`, which asks the network's resolvers whether they pass DNSSEC on.
  - `steps/` is the installation pipeline; `install.rs` orchestrates it. `image.rs` is `deli image`, which runs the steps that make a system bootable on a loop device, with `steps/image.rs` leaving the identity blank for cloud-init.
  - `launch.rs` is `deli install`: it finds a configuration (`--config`, `./deli.toml`, in the live session the kernel command line or a CIDATA volume) and installs it, or runs the interactive installer; `wizard/` is that installer (`form.rs` model, `progress.rs` installation state, `view.rs` drawing, `theme.rs` palette and symbols, `mod.rs` runtime).
- `iso/`: the ISO and cloud image build (`build.sh`, `Containerfile`, `assemble.sh`, kernel config, package list, live-only files, the cloud image's files in `cloud/`).
- `test/e2e/`: end-to-end tests on a Proxmox VE host: `proxmox.ts` for the ISO, `cloud.ts` for the cloud image, `pve.ts` for what they share.
- `examples/server.toml`: the reference of every configuration key, shipped in the live session.
- `docs/design/tui/`: the mockup the wizard was built from (80x24 screens, plain mode, 120x36, and two drawn with the console's eurlatgr font), before the Software screen: its look is still the reference when changing `wizard/view.rs` and `wizard/theme.rs`.
- `docs/screenshots/`: the README's screenshots, taken from `cargo test preview -- --ignored` (`target/wizard-preview.html`, every screen as the wizard draws it).

## Commands

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo run -- install --dry-run --source /         # the wizard on this terminal, nothing done
cargo run -- install --config examples/server.toml --dry-run
iso/build.sh                                      # x86_64 Linux with Docker
bun test/e2e/proxmox.ts [--interactive] out/deli-<version>-x86_64.iso
bun test/e2e/cloud.ts out/deli-<version>-x86_64-cloud.qcow2
```

All three checks must pass before a change is done. The end-to-end tests need the `DELI_*` variables documented at the top of `test/e2e/pve.ts`; they only ever replace a VM named `deli-e2e`.

## Rules

- Nothing that can be checked is left for after the disk is erased: validate in `config.rs` or in the preflight step, never in a step that runs after partitioning.
- Every side effect goes through `Sys`, so tests and `--dry-run` see it. Unit tests run steps against `DrySys` and assert on the recorded actions.
- Secrets (password hashes, typed passwords) never reach argv, logs, error messages or the screen: pass them on stdin, keep them in `Secret`.
- The wizard's model (`wizard/form.rs`) never touches the terminal; tests drive it with key presses and render screens into a `TestBackend`.
- The live console is a Linux VT: the wizard loads the `eurlatgr` font and sets the 8 basic colours to its palette (`wizard/theme.rs`), so only that font's glyphs and those 8 colours are available there. Check new symbols against that font, and give each an ASCII stand-in for plain mode.
- The release binary is optimised for size (`[profile.release]`); justify any new dependency.
- The repository holds no personal information: no real addresses, host names, Proxmox node or VM ids, local paths, user names or personal preferences in code, examples, tests or docs. Use documentation ranges (`192.0.2.0/24`) and `example.com`.
- Keep `README.md`, `DESIGN.md`, `CHANGELOG.md` and `examples/server.toml` in step with the code.
- Code, comments, docs and commit messages are in English. In Markdown, a paragraph is one line.
- Commits have a one-line message and no co-author trailer. Never commit, tag, push or publish a release unless asked.
