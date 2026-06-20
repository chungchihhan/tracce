#!/usr/bin/env bash
set -euo pipefail

# Cut a ctrace release and update the Homebrew formula's url + sha256.
#
#   scripts/cut-release.sh [version]
#
# version defaults to the one in Cargo.toml. The script tags vX.Y.Z, pushes the
# tag, downloads the GitHub source tarball, computes its sha256, and rewrites
# packaging/homebrew/ctrace.rb. You then copy that formula into your tap repo.

REPO="chungchihhan/ctrace"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FORMULA="$ROOT/packaging/homebrew/ctrace.rb"

ver="${1:-$(grep -m1 '^version' "$ROOT/Cargo.toml" | sed -E 's/.*"([^"]+)".*/\1/')}"
tag="v$ver"

if [ -n "$(git -C "$ROOT" status --porcelain)" ]; then
  echo "error: working tree not clean; commit or stash first" >&2
  exit 1
fi

echo "==> Tagging $tag"
git -C "$ROOT" tag -a "$tag" -m "ctrace $ver"
git -C "$ROOT" push origin "$tag"

url="https://github.com/$REPO/archive/refs/tags/$tag.tar.gz"
echo "==> Fetching tarball to compute sha256"
echo "    $url"
sha="$(curl -fsSL "$url" | shasum -a 256 | awk '{print $1}')"
echo "    sha256 = $sha"

echo "==> Updating $FORMULA"
sed -i '' -E "s|  url \".*\"|  url \"$url\"|" "$FORMULA"
sed -i '' -E "s|  sha256 \".*\"|  sha256 \"$sha\"|" "$FORMULA"

cat <<EOF

Done. Next steps:
  1) Review the formula:        $FORMULA
  2) Copy it into the tap repo: Formula/ctrace.rb
  3) Commit & push the tap repo
  4) Verify:                    brew install --build-from-source chungchihhan/tap/ctrace
EOF
