#!/usr/bin/env bash
set -euo pipefail

REPO="dongdong306/hailux"
INSTALL_DIR="${HOME}/.local/bin"
BINARY_NAME="hailux"

# Color output
info() { printf "\033[1;34m==>\033[0m %s\n" "$1"; }
error() { printf "\033[1;31mError:\033[0m %s\n" "$1" >&2; }
success() { printf "\033[1;32m==>\033[0m %s\n" "$1"; }

# Detect platform
OS="$(uname -s)"
ARCH="$(uname -m)"

case "${OS}-${ARCH}" in
    Linux-x86_64)  ASSET="hailux-linux-amd64" ;;
    *)
        error "Unsupported platform: ${OS}-${ARCH}"
        error "Currently only Linux x86_64 is supported."
        exit 1
        ;;
esac

info "Detected platform: ${OS} ${ARCH}"
info "Downloading ${BINARY_NAME}..."

DOWNLOAD_URL="https://github.com/${REPO}/releases/latest/download/${ASSET}"

TMP_FILE="$(mktemp)"
trap 'rm -f "${TMP_FILE}"' EXIT

if ! curl -fSL -o "${TMP_FILE}" "${DOWNLOAD_URL}"; then
    error "Failed to download ${DOWNLOAD_URL}"
    error "Please check your internet connection or try again later."
    exit 1
fi

info "Installing to ${INSTALL_DIR}/"
mkdir -p "${INSTALL_DIR}"
cp "${TMP_FILE}" "${INSTALL_DIR}/${BINARY_NAME}"
chmod +x "${INSTALL_DIR}/${BINARY_NAME}"

# Check if INSTALL_DIR is in PATH
case ":${PATH}:" in
    *":${INSTALL_DIR}:"*)
        ;;
    *)
        info "Adding ${INSTALL_DIR} to your PATH..."

        SHELL_NAME="$(basename "${SHELL:-bash}")"
        case "${SHELL_NAME}" in
            zsh)
                RC_FILE="${HOME}/.zshrc"
                ;;
            bash)
                RC_FILE="${HOME}/.bashrc"
                ;;
            fish)
                RC_FILE="${HOME}/.config/fish/config.fish"
                ;;
            *)
                RC_FILE="${HOME}/.profile"
                ;;
        esac

        EXPORT_LINE="export PATH=\"${INSTALL_DIR}:\$PATH\""

        # For fish, use a different syntax
        if [ "${SHELL_NAME}" = "fish" ]; then
            EXPORT_LINE="set -gx PATH ${INSTALL_DIR} \$PATH"
        fi

        if [ -f "${RC_FILE}" ]; then
            if ! grep -q "${INSTALL_DIR}" "${RC_FILE}" 2>/dev/null; then
                printf '\n# Added by hailux installer\n%s\n' "${EXPORT_LINE}" >> "${RC_FILE}"
                info "Added PATH entry to ${RC_FILE}"
                info "Run \`source ${RC_FILE}\` or restart your terminal to apply changes."
            fi
        else
            printf '\n# Added by hailux installer\n%s\n' "${EXPORT_LINE}" >> "${RC_FILE}"
            info "Created ${RC_FILE} with PATH entry."
        fi
        ;;
esac

success "${BINARY_NAME} installed successfully!"

if ! command -v "${BINARY_NAME}" >/dev/null 2>&1; then
    info "Restart your terminal or run: source ${RC_FILE:-~/.bashrc}"
fi

printf "\nRun \033[1mhailux\033[0m to get started.\n"
