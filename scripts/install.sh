#!/bin/bash
set -euo pipefail

# Tell Agent installer
# Usage: curl -sSL https://get.tell.rs/agent | bash -s -- --api-key YOUR_KEY
#    or: ./install.sh --api-key YOUR_KEY [--endpoint host:port] [--version latest]

REPO="tell-rs/tell-agent"
INSTALL_DIR="/usr/local/bin"
CONFIG_DIR="/etc/tell"
CONFIG_FILE="${CONFIG_DIR}/agent.toml"
SERVICE_FILE="/etc/systemd/system/tell-agent.service"
BINARY_NAME="tell-agent"

API_KEY=""
ENDPOINT="collect.tell.rs:50000"
VERSION="${TELL_AGENT_VERSION:-latest}"

# --- Parse arguments ---

while [[ $# -gt 0 ]]; do
    case "$1" in
        --api-key)
            API_KEY="$2"
            shift 2
            ;;
        --endpoint)
            ENDPOINT="$2"
            shift 2
            ;;
        --version)
            VERSION="$2"
            shift 2
            ;;
        *)
            echo "Unknown option: $1"
            exit 1
            ;;
    esac
done

if [[ -z "$API_KEY" ]]; then
    echo "Error: --api-key is required"
    echo "Usage: $0 --api-key YOUR_API_KEY [--endpoint host:port] [--version latest]"
    exit 1
fi

if [[ ${#API_KEY} -ne 32 ]]; then
    echo "Error: API key must be 32 hex characters"
    exit 1
fi

# --- Detect architecture ---

ARCH=$(uname -m)
case "$ARCH" in
    x86_64|amd64)
        ARCH="x86_64"
        ;;
    aarch64|arm64)
        ARCH="aarch64"
        ;;
    *)
        echo "Unsupported architecture: $ARCH"
        exit 1
        ;;
esac

OS=$(uname -s | tr '[:upper:]' '[:lower:]')
if [[ "$OS" != "linux" ]]; then
    echo "Unsupported OS: $OS (only Linux is supported)"
    exit 1
fi

TARGET="${ARCH}-unknown-linux-musl"

echo "Installing tell-agent (${TARGET})..."

# --- Resolve version ---

if [[ "$VERSION" == "latest" ]]; then
    VERSION=$(curl -sSf "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | sed -E 's/.*"v?([^"]+)".*/\1/')
    if [[ -z "$VERSION" ]]; then
        echo "Error: could not determine latest version"
        exit 1
    fi
fi

echo "Version: ${VERSION}"

# --- Download binary ---

DOWNLOAD_URL="https://github.com/${REPO}/releases/download/v${VERSION}/tell-agent-${TARGET}"
TMP_FILE=$(mktemp)

echo "Downloading from ${DOWNLOAD_URL}..."
if ! curl -sSfL -o "$TMP_FILE" "$DOWNLOAD_URL"; then
    rm -f "$TMP_FILE"
    echo "Error: failed to download tell-agent"
    exit 1
fi

chmod +x "$TMP_FILE"

# --- Install binary ---

echo "Installing to ${INSTALL_DIR}/${BINARY_NAME}..."
sudo mv "$TMP_FILE" "${INSTALL_DIR}/${BINARY_NAME}"

# --- Create user ---

if ! id -u tell-agent >/dev/null 2>&1; then
    echo "Creating tell-agent user..."
    sudo useradd --system --no-create-home --shell /usr/sbin/nologin tell-agent
fi

# --- Write config (only if not exists) ---

if [[ ! -f "$CONFIG_FILE" ]]; then
    echo "Writing config to ${CONFIG_FILE}..."
    sudo mkdir -p "$CONFIG_DIR"
    sudo tee "$CONFIG_FILE" > /dev/null <<TOML
api_key = "${API_KEY}"
endpoint = "${ENDPOINT}"

logs = [
    "/var/log/syslog",
    "/var/log/auth.log",
]
TOML
    sudo chmod 640 "$CONFIG_FILE"
    sudo chown root:tell-agent "$CONFIG_FILE"
else
    echo "Config already exists at ${CONFIG_FILE}, skipping..."
fi

# --- Install systemd unit ---

echo "Installing systemd service..."
sudo tee "$SERVICE_FILE" > /dev/null <<'UNIT'
[Unit]
Description=Tell Agent
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/tell-agent --config /etc/tell/agent.toml
Restart=always
RestartSec=5
User=tell-agent
Group=tell-agent
ReadOnlyPaths=/proc /sys /var/log
StateDirectory=tell-agent

[Install]
WantedBy=multi-user.target
UNIT

# --- Enable and start ---

sudo systemctl daemon-reload
sudo systemctl enable tell-agent
sudo systemctl start tell-agent

echo ""
echo "tell-agent installed and running!"
echo "  Binary:  ${INSTALL_DIR}/${BINARY_NAME}"
echo "  Config:  ${CONFIG_FILE}"
echo "  Service: systemctl status tell-agent"
echo "  Logs:    journalctl -u tell-agent -f"
