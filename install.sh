#!/bin/sh
set -eu

REPOSITORY="${AIRWIRE_REPOSITORY:-H34TB145T/airwire}"
VERSION="${AIRWIRE_VERSION:-latest}"
INSTALL_DIR="${AIRWIRE_INSTALL_DIR:-$HOME/.local/bin}"

fail() {
    printf 'airwire installer: %s\n' "$1" >&2
    exit 1
}

case "$(uname -s)" in
    Darwin) platform="macos" ;;
    Linux) platform="linux" ;;
    *) fail "unsupported operating system: $(uname -s)" ;;
esac

case "$(uname -m)" in
    x86_64 | amd64) architecture="x86_64" ;;
    arm64 | aarch64) architecture="aarch64" ;;
    *) fail "unsupported processor architecture: $(uname -m)" ;;
esac

asset="airwire-${platform}-${architecture}.tar.gz"
if [ -n "${AIRWIRE_DOWNLOAD_BASE:-}" ]; then
    download_base="${AIRWIRE_DOWNLOAD_BASE%/}"
elif [ "$VERSION" = "latest" ]; then
    download_base="https://github.com/${REPOSITORY}/releases/latest/download"
else
    download_base="https://github.com/${REPOSITORY}/releases/download/${VERSION}"
fi

temporary_directory="$(mktemp -d 2>/dev/null || mktemp -d -t airwire)"
trap 'rm -rf "$temporary_directory"' EXIT INT TERM

download() {
    source_url="$1"
    destination="$2"
    if command -v curl >/dev/null 2>&1; then
        curl --fail --location --silent --show-error "$source_url" --output "$destination"
    elif command -v wget >/dev/null 2>&1; then
        wget --quiet "$source_url" --output-document="$destination"
    else
        fail "curl or wget is required"
    fi
}

printf 'Downloading %s...\n' "$asset"
download "$download_base/$asset" "$temporary_directory/$asset"
download "$download_base/$asset.sha256" "$temporary_directory/$asset.sha256"

(
    cd "$temporary_directory"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum --check "$asset.sha256"
    elif command -v shasum >/dev/null 2>&1; then
        shasum --algorithm 256 --check "$asset.sha256"
    else
        fail "sha256sum or shasum is required to verify the download"
    fi
)

tar -xzf "$temporary_directory/$asset" -C "$temporary_directory"
mkdir -p "$INSTALL_DIR"
install -m 755 "$temporary_directory/airwire" "$INSTALL_DIR/airwire"

case ":${PATH:-}:" in
    *":$INSTALL_DIR:"*) ;;
    *)
        shell_name="$(basename "${SHELL:-sh}")"
        case "$shell_name" in
            zsh) profile="$HOME/.zshrc" ;;
            bash)
                if [ "$(uname -s)" = "Darwin" ]; then
                    profile="$HOME/.bash_profile"
                else
                    profile="$HOME/.bashrc"
                fi
                ;;
            *) profile="$HOME/.profile" ;;
        esac
        {
            printf '\n# Airwire command\n'
            printf 'export PATH="%s:$PATH"\n' "$INSTALL_DIR"
        } >>"$profile"
        printf 'Added %s to PATH in %s.\n' "$INSTALL_DIR" "$profile"
        ;;
esac

"$INSTALL_DIR/airwire" --version
printf 'Installed Airwire to %s\n' "$INSTALL_DIR/airwire"
printf 'Open a new terminal, then run: airwire --start\n'
