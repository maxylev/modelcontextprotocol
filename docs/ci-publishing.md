# CI & publishing

This page describes the repository's CI/CD setup conceptually. The workflow
files themselves live in `.github/workflows/` and are maintained separately;
this page documents what each workflow does and how the pieces fit
together.

## GitHub Actions workflows

| Workflow      | Triggers                                                                                                                               | What it does                                                                                                                                                                                                                                                                                                                                                                                        |
| ------------- | -------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `ci.yml`      | push to `main`, pull requests                                                                                                          | **Lint job** (ubuntu): `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo doc --no-deps` with `-D warnings`. **Test job** (ubuntu, macOS, Windows matrix): `cargo test --all-targets --all-features --locked` (unit + integration, spawning the real binary) plus a release build                                                                              |
| `release.yml` | tags `v*`                                                                                                                              | Creates a GitHub Release and uploads prebuilt binaries for macOS, Linux (glibc + fully static musl), Windows, iOS, and Android, each built with both the MSRV (1.97.1) and latest stable Rust. Archives are named `<bin>-<target>-rust-msrv` (MSRV) or `<bin>-<target>-rust-stable` (latest stable) and ship with SHA-256 checksums, so users without Rust can download and run the binary directly |
| `publish.yml` | tags `v*`                                                                                                                              | `cargo package --locked` (pack + metadata checks), then `cargo publish --locked` to crates.io using the `CARGO_REGISTRY_TOKEN` repository secret                                                                                                                                                                                                                                                    |
| `docs.yml`    | push to `main` touching `docs/**`, `src/**`, `README.md`, `Cargo.toml`, `rust-toolchain.toml`, or the workflow itself; manual dispatch | Builds and deploys this documentation site to GitHub Pages                                                                                                                                                                                                                                                                                                                                          |

The CI badge in the README reflects `ci.yml`; the docs badge reflects
`docs.yml`.

## Release pipeline

When a `v*` tag is pushed, `release.yml` (in parallel with `publish.yml`)
runs:

1. `create-release` creates the GitHub Release for the tag with generated
   notes.
2. `upload-assets` runs one job per matrix cell: 13 targets × 2 Rust
   toolchains. Each job installs the toolchain (`RUSTUP_TOOLCHAIN` overrides
   the pinned `rust-toolchain.toml`), installs a cross toolchain where
   needed, builds with `--locked`, and uploads a `.tar.gz` (unix) or `.zip`
   (windows) archive, a `.sha256` file, and the `install.sh` one-line
   installer script.

Target/configuration notes:

- **Linux** — glibc builds (`x86_64`, `aarch64`) and fully static musl
  builds (`x86_64`, `aarch64`, `armv7-eabihf`, `RUSTFLAGS
-C target-feature=+crt-static -C link-self-contained=yes`) run on
  `ubuntu-22.04` (glibc 2.35) for wide distribution compatibility.
- **macOS** — built on `macos-15` with `MACOSX_DEPLOYMENT_TARGET=10.12`
  (x86_64) or `11.0` (aarch64) so the binaries run on older macOS.
- **Windows** — MSVC targets with the static CRT (`-C
target-feature=+crt-static`), so no VC++ runtime is required on the
  target machine.
- **iOS** — `aarch64-apple-ios` builds unsigned; iOS has no user-installable
  `bin` folder, so this artifact is only for signing/embedding into an app.
- **Android** — `aarch64`, `armv7`, and `x86_64` NDK builds, usable e.g.
  under Termux.

Archive names carry a stable toolchain label rather than the exact rustc
version, e.g. `modelcontextprotocol-x86_64-apple-darwin-rust-msrv.tar.gz`
(built with the pinned MSRV, 1.97.1) or `...-rust-stable.tar.gz` (built with
the latest stable Rust). Because the labels never change, the
`/releases/latest/download/<name>` links in the README and
[getting-started](/getting-started) stay valid across releases and Rust
updates; the exact rustc version is visible in the CI run for each asset.

## Docs deployment pipeline

The published site lives at
<https://maxylev.github.io/modelcontextprotocol/> (Pages project site, base
path `/modelcontextprotocol/`). `docs.yml` performs:

1. Configure GitHub Pages and restore the npm and VitePress caches.
2. Install Node.js 24 and run `npm ci --no-audit --no-fund` in `docs/`
   (installs the exact pinned dependencies from `docs/package-lock.json`).
3. Run `npm run format:check` and build VitePress into
   `docs/.vitepress/dist`.
4. Install Rust 1.97.1 and run `cargo doc --no-deps --all-features`.
5. Copy `target/doc/` into `docs/.vitepress/dist/rustdoc/`.
6. Upload `docs/.vitepress/dist` as the Pages artifact.
7. Deploy the artifact with `actions/deploy-pages` in the protected
   `github-pages` environment.

That is why the
[Rust API docs](https://maxylev.github.io/modelcontextprotocol/rustdoc/)
link works: the rustdoc is generated with the same toolchain the crate
targets and shipped inside the site bundle. The workflow also creates a
small redirect at `/rustdoc/` because a single-crate `cargo doc` build does
not generate a root index page.

## Local simulation of the pipeline

```bash
cd docs && npm ci && npm run docs:build   # VitePress site
cargo doc --no-deps --all-features        # rustdoc
mkdir -p docs/.vitepress/dist/rustdoc
cp -a target/doc/. docs/.vitepress/dist/rustdoc/
npm run docs:preview                      # inspect locally at the base path
```
