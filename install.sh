#!/bin/sh
set -eu

# vram.supply agent installer
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/ohone/vram-supply-agent/main/install.sh | sh
#
# Pin a version:
#   VRAM_SUPPLY_AGENT_VERSION=v0.1.0 curl -fsSL ... | sh

REPO="ohone/vram-supply-agent"
INSTALL_DIR="${HOME}/.local/bin"
BINARY_NAME="vramsply"

main() {
    detect_platform
    resolve_version
    download_and_verify
    install_binary
    print_success
}

detect_platform() {
    OS="$(uname -s)"
    ARCH="$(uname -m)"

    case "${OS}" in
        Linux)  OS_TARGET="unknown-linux-gnu" ;;
        Darwin) OS_TARGET="apple-darwin" ;;
        *)
            echo "Error: unsupported operating system: ${OS}" >&2
            exit 1
            ;;
    esac

    case "${ARCH}" in
        x86_64|amd64)   ARCH_TARGET="x86_64" ;;
        aarch64|arm64)   ARCH_TARGET="aarch64" ;;
        *)
            echo "Error: unsupported architecture: ${ARCH}" >&2
            exit 1
            ;;
    esac

    TARGET="${ARCH_TARGET}-${OS_TARGET}"
    echo "Detected platform: ${TARGET}"
}

resolve_version() {
    if [ -n "${VRAM_SUPPLY_AGENT_VERSION:-}" ]; then
        VERSION="${VRAM_SUPPLY_AGENT_VERSION}"
        echo "Using pinned version: ${VERSION}"
    else
        echo "Fetching latest release..."
        VERSION="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
            | grep '"tag_name"' \
            | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')"
        if [ -z "${VERSION}" ]; then
            echo "Error: failed to determine latest release version" >&2
            exit 1
        fi
        echo "Latest version: ${VERSION}"
    fi
}

download_and_verify() {
    TMPDIR="$(mktemp -d)"
    trap 'rm -rf "${TMPDIR}"' EXIT

    BINARY_URL="https://github.com/${REPO}/releases/download/${VERSION}/${BINARY_NAME}-${TARGET}"
    CHECKSUMS_URL="https://github.com/${REPO}/releases/download/${VERSION}/SHA256SUMS.txt"

    echo "Downloading ${BINARY_NAME}-${TARGET}..."
    curl -fSL -o "${TMPDIR}/${BINARY_NAME}" "${BINARY_URL}"

    echo "Downloading checksums..."
    curl -fSL -o "${TMPDIR}/SHA256SUMS.txt" "${CHECKSUMS_URL}"

    echo "Verifying checksum..."
    EXPECTED="$(grep "${BINARY_NAME}-${TARGET}" "${TMPDIR}/SHA256SUMS.txt" | awk '{print $1}')"
    if [ -z "${EXPECTED}" ]; then
        echo "Error: no checksum found for ${BINARY_NAME}-${TARGET} in SHA256SUMS.txt" >&2
        exit 1
    fi

    case "${OS}" in
        Linux)
            ACTUAL="$(sha256sum "${TMPDIR}/${BINARY_NAME}" | awk '{print $1}')"
            ;;
        Darwin)
            ACTUAL="$(shasum -a 256 "${TMPDIR}/${BINARY_NAME}" | awk '{print $1}')"
            ;;
    esac

    if [ "${ACTUAL}" != "${EXPECTED}" ]; then
        echo "Error: checksum verification failed!" >&2
        echo "  Expected: ${EXPECTED}" >&2
        echo "  Actual:   ${ACTUAL}" >&2
        exit 1
    fi

    echo "Checksum verified."
}

install_binary() {
    mkdir -p "${INSTALL_DIR}"
    mv "${TMPDIR}/${BINARY_NAME}" "${INSTALL_DIR}/${BINARY_NAME}"
    chmod +x "${INSTALL_DIR}/${BINARY_NAME}"
}

print_success() {
    echo ""
    echo "Installed ${BINARY_NAME} to ${INSTALL_DIR}/${BINARY_NAME}"

    if ! echo "${PATH}" | tr ':' '\n' | grep -qx "${INSTALL_DIR}"; then
        echo ""
        echo "WARNING: ${INSTALL_DIR} is not in your PATH."
        echo "Add it by running:"
        echo "  export PATH=\"${INSTALL_DIR}:\${PATH}\""
        echo ""
        echo "To make this permanent, add the line above to your shell profile (~/.bashrc, ~/.zshrc, etc.)"
    fi

    echo ""
    "${INSTALL_DIR}/${BINARY_NAME}" --version
}

main
