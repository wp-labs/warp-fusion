#!/usr/bin/env bash
set -euo pipefail

VERSION="${1:-}"
CHANNEL="${2:-stable}"

if [[ -z "$VERSION" ]]; then
  if [[ -f version.txt ]]; then
    VERSION="$(tr -d '[:space:]' < version.txt)"
  else
    echo "usage: $0 <version> [stable|alpha|beta]" >&2
    exit 1
  fi
fi

VERSION="${VERSION#v}"
TAG="v${VERSION}"

case "$CHANNEL" in
  stable|alpha|beta) ;;
  *)
    echo "invalid channel: $CHANNEL" >&2
    exit 1
    ;;
esac

PUBLISHED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
GIT_COMMIT="$(git rev-parse HEAD 2>/dev/null || true)"
if [[ -z "$GIT_COMMIT" ]]; then
  GIT_COMMIT=null
else
  GIT_COMMIT="\"$GIT_COMMIT\""
fi

OUT_DIR="updates/${CHANNEL}"
OUT="${OUT_DIR}/manifest.json"
HISTORY="${OUT_DIR}/versions/${TAG}.json"
mkdir -p "$OUT_DIR" "$(dirname "$HISTORY")"

cat > "$OUT" <<EOF
{
  "version": "${VERSION}",
  "channel": "${CHANNEL}",
  "published_at": "${PUBLISHED_AT}",
  "git_ref": "${TAG}",
  "git_commit": ${GIT_COMMIT},
  "assets": {
    "aarch64-apple-darwin": {
      "url": "https://github.com/wp-labs/warp-fusion/releases/download/${TAG}/warp-fusion-${TAG}-aarch64-apple-darwin.tar.gz",
      "sha256": "0000000000000000000000000000000000000000000000000000000000000000"
    },
    "aarch64-unknown-linux-gnu": {
      "url": "https://github.com/wp-labs/warp-fusion/releases/download/${TAG}/warp-fusion-${TAG}-aarch64-unknown-linux-gnu.tar.gz",
      "sha256": "0000000000000000000000000000000000000000000000000000000000000000"
    },
    "x86_64-unknown-linux-gnu": {
      "url": "https://github.com/wp-labs/warp-fusion/releases/download/${TAG}/warp-fusion-${TAG}-x86_64-unknown-linux-gnu.tar.gz",
      "sha256": "0000000000000000000000000000000000000000000000000000000000000000"
    }
  }
}
EOF

cp "$OUT" "$HISTORY"
echo "updated $OUT"
