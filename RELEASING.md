# Releasing ctrace

ctrace ships through a **Homebrew tap** that builds from source. No code signing
or notarization is required, because Homebrew compiles on the user's machine,
and it works on both Apple Silicon and Intel. Users install with:

```
brew install chungchihhan/tap/ctrace
```

Releases are automated: **push a tag and a GitHub Action does the rest.**

## One-time setup

### 1. Create the tap (done once, shared by all your projects)

A "tap" is just a public GitHub repo named `homebrew-tap`. One tap can hold many
formulae (`Formula/ctrace.rb`, `Formula/other-project.rb`, …), so you only ever
create this once.

```
chungchihhan/homebrew-tap
└── Formula/
    └── ctrace.rb
```

Seed it with the current formula (the Action edits an existing file, so it must
exist before the first tagged release):

```
# from this repo
cp packaging/homebrew/ctrace.rb /path/to/homebrew-tap/Formula/ctrace.rb
# commit & push the tap repo
```

### 2. Add the COMMITTER_TOKEN secret

The Action runs in the `ctrace` repo but needs to push to `homebrew-tap`. The
default `GITHUB_TOKEN` can't write to another repo, so create a Personal Access
Token:

- **Fine-grained PAT** — Repository access: `chungchihhan/homebrew-tap`;
  Permissions: Contents → Read and write. (Or a classic PAT with `public_repo`.)

Then add it to the **ctrace** repo: Settings → Secrets and variables → Actions →
New repository secret, named `COMMITTER_TOKEN`.

## Per release

1. Bump `version` in `Cargo.toml`, run `cargo build` to refresh `Cargo.lock`,
   commit, and push `main`.
2. Tag and push:

   ```
   git tag v0.2.0
   git push origin v0.2.0
   ```

The `release` workflow then:
- creates a GitHub release with generated notes, and
- bumps `Formula/ctrace.rb` in the tap (new `url` + `sha256`).

Users get it with `brew upgrade ctrace` (or a fresh `brew install
chungchihhan/tap/ctrace`).

## Manual fallback

If you ever need to cut a release without CI, `scripts/cut-release.sh` does the
same formula bump locally: it tags, pushes, computes the source-tarball sha256,
and rewrites `packaging/homebrew/ctrace.rb`. Copy that into the tap by hand.

## Notes

- `depends_on "rust" => :build` pulls in a Rust toolchain at build time; users
  don't need Rust installed beforehand (they do need the Xcode Command Line
  Tools, which Homebrew already requires).
- The formula's `head` URL lets adventurous users run
  `brew install --HEAD chungchihhan/tap/ctrace` to build the latest `main`.
- `cargo install --git https://github.com/chungchihhan/ctrace` also works for
  anyone who already has Rust and doesn't want Homebrew.
- Want `brew install` to *not* compile on the user's machine? That means
  shipping prebuilt bottles from CI (and dealing with macOS notarization) —
  a larger change we can add later.
