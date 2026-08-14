# CI & publishing

This page describes the repository's CI/CD setup conceptually. The workflow
files themselves live in `.github/workflows/` and are maintained separately;
this page documents what each workflow does and how the pieces fit
together.

## GitHub Actions workflows

| Workflow      | Triggers                                                                                                                               | What it does                                                                                                                                                                                                                                                                                                           |
| ------------- | -------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `ci.yml`      | push to `main`, pull requests                                                                                                          | **Lint job** (ubuntu): `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo doc --no-deps` with `-D warnings`. **Test job** (ubuntu, macOS, Windows matrix): `cargo test --all-targets --all-features --locked` (unit + integration, spawning the real binary) plus a release build |
| `publish.yml` | tags `v*`                                                                                                                              | `cargo package --locked` (pack + metadata checks), then `cargo publish --locked` to crates.io using the `CARGO_REGISTRY_TOKEN` repository secret                                                                                                                                                                       |
| `docs.yml`    | push to `main` touching `docs/**`, `src/**`, `README.md`, `Cargo.toml`, `rust-toolchain.toml`, or the workflow itself; manual dispatch | Builds and deploys this documentation site to GitHub Pages                                                                                                                                                                                                                                                             |

The CI badge in the README reflects `ci.yml`; the docs badge reflects
`docs.yml`.

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

That is why the [Rust API docs](/rustdoc/modelcontextprotocol/index.html)
link in this site's navigation works: the rustdoc is generated with the
same toolchain the crate targets and shipped inside the site bundle.

## Local simulation of the pipeline

```bash
cd docs && npm ci && npm run docs:build   # VitePress site
cargo doc --no-deps --all-features        # rustdoc
mkdir -p docs/.vitepress/dist/rustdoc
cp -a target/doc/. docs/.vitepress/dist/rustdoc/
npm run docs:preview                      # inspect locally at the base path
```
