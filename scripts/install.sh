#!/usr/bin/env bash
# Build release bundles of vocal_eq and install them for the current user.
#
#   macOS: ad-hoc codesign, copy to ~/Library/Audio/Plug-Ins/{CLAP,VST3}
#   Linux: copy to ~/.clap and ~/.vst3 (no codesigning)
#
# Usage: scripts/install.sh [--no-build]
set -euo pipefail

PLUGIN="vocal_eq"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUNDLE_DIR="$ROOT/target/bundled"

cd "$ROOT"

if [[ "${1:-}" != "--no-build" ]]; then
    echo "==> Building release bundles"
    cargo xtask bundle "$PLUGIN" --release
fi

CLAP_SRC="$BUNDLE_DIR/$PLUGIN.clap"
VST3_SRC="$BUNDLE_DIR/$PLUGIN.vst3"
for b in "$CLAP_SRC" "$VST3_SRC"; do
    [[ -e "$b" ]] || { echo "error: bundle not found: $b" >&2; exit 1; }
done

case "$(uname -s)" in
    Darwin)
        CLAP_DEST="$HOME/Library/Audio/Plug-Ins/CLAP"
        VST3_DEST="$HOME/Library/Audio/Plug-Ins/VST3"
        echo "==> Ad-hoc codesigning bundles"
        codesign --force --deep -s - "$CLAP_SRC"
        codesign --force --deep -s - "$VST3_SRC"
        ;;
    Linux)
        CLAP_DEST="$HOME/.clap"
        VST3_DEST="$HOME/.vst3"
        ;;
    *)
        echo "error: unsupported OS $(uname -s)" >&2
        exit 1
        ;;
esac

mkdir -p "$CLAP_DEST" "$VST3_DEST"

install_bundle() {
    local src="$1" dest_dir="$2"
    local dest="$dest_dir/$(basename "$src")"
    rm -rf "$dest"
    cp -R "$src" "$dest"
    echo "    installed $dest"
}

echo "==> Installing"
install_bundle "$CLAP_SRC" "$CLAP_DEST"
install_bundle "$VST3_SRC" "$VST3_DEST"

if [[ "$(uname -s)" == "Darwin" ]]; then
    echo "==> Verifying signatures"
    codesign --verify --deep --strict "$CLAP_DEST/$PLUGIN.clap"
    codesign --verify --deep --strict "$VST3_DEST/$PLUGIN.vst3"
fi

echo "==> Done. Rescan plug-ins in your DAW."
