# Releasing ctrace

ctrace ships through a **Homebrew tap** that builds from source. No code signing
or notarization is required, because Homebrew compiles on the user's machine,
and it works on both Apple Silicon and Intel. Users install with:

```
brew install chungchihhan/tap/ctrace
```

## One-time setup: create the tap

A "tap" is just a public GitHub repo named `homebrew-tap`.

1. Create a public repo `chungchihhan/homebrew-tap` (web UI, or
   `gh auth login` to github.com then `gh repo create chungchihhan/homebrew-tap --public`).
2. Add the formula at `Formula/ctrace.rb` — copy it from this repo's
   `packaging/homebrew/ctrace.rb`.
3. Commit and push.

That's it. `brew install chungchihhan/tap/ctrace` resolves
`chungchihhan/tap` → the `homebrew-tap` repo → `Formula/ctrace.rb`.

## Per release

1. Bump `version` in `Cargo.toml`, update `Cargo.lock` (`cargo build`), commit.
2. From this repo, run:

   ```
   scripts/cut-release.sh           # uses Cargo.toml version
   # or: scripts/cut-release.sh 0.2.0
   ```

   This tags `vX.Y.Z`, pushes the tag, computes the source-tarball sha256, and
   rewrites `packaging/homebrew/ctrace.rb` with the new `url` + `sha256`.
3. Copy the updated `packaging/homebrew/ctrace.rb` into the tap repo as
   `Formula/ctrace.rb`, then commit and push the tap.
4. Verify a clean install:

   ```
   brew update
   brew install --build-from-source chungchihhan/tap/ctrace
   ctrace --version
   ```

Users upgrade with `brew upgrade ctrace`.

## Notes

- `depends_on "rust" => :build` pulls in a Rust toolchain at build time; users
  don't need Rust installed beforehand. They do need the Xcode Command Line
  Tools, which Homebrew already requires.
- The formula's `head` URL lets adventurous users run `brew install --HEAD
  chungchihhan/tap/ctrace` to build the latest `main`.
- A `cargo install --git https://github.com/chungchihhan/ctrace` path also works
  for anyone who already has Rust and doesn't want Homebrew.
