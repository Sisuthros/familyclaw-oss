#!/usr/bin/env bash
# FamilyClaw Installer — KERROS A (OSS) binary + KERROS B (private) service template
# Usage: bash install.sh [--user] [--prefix /custom/path] [--service-name familyclaw-<agent>]
#        sudo bash install.sh  # system-wide install (default)

set -euo pipefail

# ── Configuration ─────────────────────────────────────────────────
REPO_URL="https://github.com/Sisuthros/familyclaw-oss.git"
REPO_DIR="${HOME}/.local/share/familyclaw-source"
BINARY_NAME="familyclaw-gateway"
DEFAULT_PREFIX="/usr/local"
USER_PREFIX="${HOME}/.local"
SERVICE_NAME_DEFAULT="familyclaw-gateway"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

# ── Helpers ───────────────────────────────────────────────────────
log_info()  { echo -e "${BLUE}[INFO]${NC} $*"; }
log_ok()    { echo -e "${GREEN}[OK]${NC}  $*"; }
log_warn()  { echo -e "${YELLOW}[WARN]${NC} $*"; }
log_error() { echo -e "${RED}[ERROR]${NC} $*"; exit 1; }

# ── Parse args ────────────────────────────────────────────────────
PREFIX="${DEFAULT_PREFIX}"
SERVICE_NAME="${SERVICE_NAME_DEFAULT}"
INSTALL_USER=false
REPO_ONLY=false

while [[ $# -gt 0 ]]; do
    case $1 in
        --user)
            INSTALL_USER=true
            PREFIX="${USER_PREFIX}"
            ;;
        --prefix)
            PREFIX="$2"
            shift
            ;;
        --service-name)
            SERVICE_NAME="$2"
            shift
            ;;
        --repo-only)
            REPO_ONLY=true
            ;;
        --help|-h)
            cat <<EOF
FamilyClaw Installer — builds gateway binary + creates systemd template

Usage: sudo bash install.sh [OPTIONS]
       bash install.sh --user [OPTIONS]    # user install (no sudo)

Options:
  --user              Install to ~/.local (no sudo needed)
  --prefix PATH       Custom install prefix (default: /usr/local or ~/.local)
  --service-name NAME systemd unit name (default: familyclaw-gateway)
  --repo-only         Only clone/update repo, don't build/install
  --help, -h          Show this help

Environment (Layer B - never in repo):
  FAMILYCLAW_PROFILE_DIR   Path to private profiles (SOUL.md, keys, etc.)
  TELEGRAM_BOT_TOKEN       Telegram bot token
  FAMILYCLAW_TELEGRAM_CHANNEL_ID
  FAMILYCLAW_REPLY_TARGET  Telegram chat_id for replies
  FAMILYCLAW_GATEWAY_ADDR  Listen address (default: 127.0.0.1:8787)
  FAMILYCLAW_PROVIDERS     Provider table: prefix=url=KEY_ENV;...

Example (user install + enable service):
  bash install.sh --user --service-name familyclaw-agent
  systemctl --user enable --now familyclaw-agent
EOF
            exit 0
            ;;
        *)
            log_error "Unknown option: $1"
            ;;
    esac
    shift
done

BIN_DIR="${PREFIX}/bin"
SERVICE_DIR="/etc/systemd/system"
if [[ "${INSTALL_USER}" == true ]]; then
    SERVICE_DIR="${HOME}/.config/systemd/user"
fi

# ── Check Rust ────────────────────────────────────────────────────
check_rust() {
    log_info "Checking Rust toolchain..."
    if ! command -v cargo &>/dev/null; then
        log_warn "Rust not found. Installing via rustup..."
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
        source "${HOME}/.cargo/env"
    fi
    local version
    version=$(rustc --version | cut -d' ' -f2)
    log_ok "Rust ${version} available"
}

# ── Clone / Update repo ───────────────────────────────────────────
sync_repo() {
    log_info "Syncing repository..."
    if [[ -d "${REPO_DIR}/.git" ]]; then
        git -C "${REPO_DIR}" fetch --quiet origin
        git -C "${REPO_DIR}" reset --quiet --hard origin/main
        log_ok "Repository updated"
    else
        git clone --quiet "${REPO_URL}" "${REPO_DIR}"
        log_ok "Repository cloned"
    fi
}

# ── Build ─────────────────────────────────────────────────────────
build_binary() {
    log_info "Building ${BINARY_NAME} (release)..."
    cd "${REPO_DIR}"
    cargo build --release -p familyclaw-gateway --locked
    log_ok "Build complete"
}

# ── Install binary ────────────────────────────────────────────────
install_binary() {
    log_info "Installing binary to ${BIN_DIR}/..."
    mkdir -p "${BIN_DIR}"
    cp "${REPO_DIR}/target/release/${BINARY_NAME}" "${BIN_DIR}/${BINARY_NAME}"
    chmod +x "${BIN_DIR}/${BINARY_NAME}"
    log_ok "Binary installed at ${BIN_DIR}/${BINARY_NAME}"
}

# ── Create systemd service template ───────────────────────────────
create_service_template() {
    log_info "Creating systemd service template..."
    mkdir -p "${SERVICE_DIR}"

    local service_file="${SERVICE_DIR}/${SERVICE_NAME}.service"
    cat >"${service_file}" <<EOF
# FamilyClaw Gateway — ${SERVICE_NAME}
# KERROS B: This file is a TEMPLATE. Copy to /etc/systemd/system/ (or ~/.config/systemd/user/)
# and fill in your private Layer B values. NEVER commit real secrets to git.
#
# Generate with: install.sh --service-name ${SERVICE_NAME}
# Enable:        systemctl [--user] enable --now ${SERVICE_NAME}

[Unit]
Description=FamilyClaw Gateway (${SERVICE_NAME})
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
# ─── LAYER B: Fill these in ───
Environment=FAMILYCLAW_PROFILE_DIR=/path/to/your/private/profiles
Environment=TELEGRAM_BOT_TOKEN=your_bot_token_here
Environment=FAMILYCLAW_TELEGRAM_CHANNEL_ID=your_channel_id
Environment=FAMILYCLAW_REPLY_TARGET=your_chat_id
Environment=FAMILYCLAW_GATEWAY_ADDR=127.0.0.1:8787
# Example provider table (optional):
# Environment=FAMILYCLAW_PROVIDERS=openai=https://api.openai.com/v1=OPENAI_API_KEY;anthropic=https://api.anthropic.com/v1=ANTHROPIC_API_KEY
# ──────────────────────────────
ExecStart=${BIN_DIR}/${BINARY_NAME}
Restart=on-failure
RestartSec=5
StandardOutput=journal
StandardError=journal

# Security hardening
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=read-only
ReadWritePaths=${HOME}/.config/familyclaw %{if INSTALL_USER}~/.local/share/familyclaw%{else}/var/lib/familyclaw%{end}

[Install]
WantedBy=multi-user.target
EOF

    # Fix the conditional paths for systemd template
    sed -i "s|%{if INSTALL_USER}.*%{else}.*%{end}|$( [[ "${INSTALL_USER}" == true ]] && echo "${HOME}/.local/share/familyclaw" || echo "/var/lib/familyclaw" )|" "${service_file}"

    log_ok "Service template created: ${service_file}"
    log_warn "Edit ${service_file} with your Layer B values before enabling!"
}

# ── Print post-install instructions ───────────────────────────────
print_summary() {
    echo
    log_ok "═══════════════════════════════════════════════"
    log_ok "  FamilyClaw Gateway installed!"
    log_ok "═══════════════════════════════════════════════"
    echo
    echo "Binary: ${BIN_DIR}/${BINARY_NAME}"
    echo "Service template: ${SERVICE_DIR}/${SERVICE_NAME}.service"
    echo
    echo "Next steps (Layer B - private, never in repo):"
    echo "  1. Edit the service file with your secrets:"
    echo "     ${YELLOW}nano ${SERVICE_DIR}/${SERVICE_NAME}.service${NC}"
    echo "  2. Create your profile directory (FAMILYCLAW_PROFILE_DIR) with:"
    echo "     - SOUL.md (agent identity)"
    echo "     - familyclaw.toml (if you prefer TOML over env)"
    echo "  3. Enable and start:"
    if [[ "${INSTALL_USER}" == true ]]; then
        echo "     systemctl --user daemon-reload"
        echo "     systemctl --user enable --now ${SERVICE_NAME}"
        echo "     journalctl --user -u ${SERVICE_NAME} -f"
    else
        echo "     sudo systemctl daemon-reload"
        echo "     sudo systemctl enable --now ${SERVICE_NAME}"
        echo "     sudo journalctl -u ${SERVICE_NAME} -f"
    fi
    echo
    echo "Verify:"
    echo "  curl http://127.0.0.1:8787/healthz  # -> ok"
    echo "  curl http://127.0.0.1:8787/readyz   # -> ready (when bus is up)"
    echo
    echo "Docs: https://github.com/Sisuthros/familyclaw-oss"
}

# ── Main ──────────────────────────────────────────────────────────
main() {
    echo "═══════════════════════════════════════════════"
    echo "  FamilyClaw Gateway Installer"
    echo "  Repo: ${REPO_URL}"
    echo "  Prefix: ${PREFIX}"
    echo "  Service: ${SERVICE_NAME}"
    echo "═══════════════════════════════════════════════"
    echo

    check_rust
    sync_repo

    if [[ "${REPO_ONLY}" == true ]]; then
        log_ok "Repo synced to ${REPO_DIR}"
        exit 0
    fi

    build_binary
    install_binary
    create_service_template
    print_summary
}

main "$@"