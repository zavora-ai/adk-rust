#!/usr/bin/env bash
#
# setup.sh: Idempotent Development Environment Setup
#
# This script prepares the development environment by installing Nix, devenv,
# and direnv with nix-direnv for optimal performance.

set -euo pipefail

# --- Constants ---
DEVENV_VERSION="v2.0.6"

# --- Helper Functions ---

log() {
    echo "--- $1 ---"
}

command_exists() {
    command -v "$1" &> /dev/null
}

# --- Installation Functions ---

install_nix() {
    log "Checking Nix installation"
    if command_exists "nix"; then
        log "Nix is already installed."
    else
        log "Installing Nix..."
        curl -L https://nixos.org/nix/install | sh -s -- --daemon
        log "Nix installed. Please restart your shell or run: source /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh"

        # Explicitly source Nix environment profiles immediately after Nix installation
        # so subsequent script commands function properly in the current session.
        if [ -e '/nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh' ]; then
            source '/nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh'
        elif [ -e '/nix/var/nix/profiles/default/etc/profile.d/nix.sh' ]; then
            source '/nix/var/nix/profiles/default/etc/profile.d/nix.sh'
        fi

        # Append sourcing logic to ~/.bashrc to ensure Nix is available in non-login shells.
        # Only modify the file automatically if running in CI mode to prevent duplicate entries or overwriting user configs.
        if [ "${CI:-false}" = "true" ]; then
            if ! grep -q "nix-daemon.sh" "$HOME/.bashrc" 2>/dev/null; then
                echo 'if [ -e "/nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh" ]; then source "/nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh"; elif [ -e "/nix/var/nix/profiles/default/etc/profile.d/nix.sh" ]; then source "/nix/var/nix/profiles/default/etc/profile.d/nix.sh"; fi' >> "$HOME/.bashrc"
            fi
        else
            log "Manual step: Please add the following to your ~/.bashrc or shell profile to enable Nix:"
            echo 'if [ -e "/nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh" ]; then source "/nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh"; elif [ -e "/nix/var/nix/profiles/default/etc/profile.d/nix.sh" ]; then source "/nix/var/nix/profiles/default/etc/profile.d/nix.sh"; fi'
        fi

        # Configure nix.conf to enable experimental features required by devenv (nix-command flakes).
        mkdir -p "$HOME/.config/nix"
        if ! grep -q "experimental-features = nix-command flakes" "$HOME/.config/nix/nix.conf" 2>/dev/null; then
            echo "experimental-features = nix-command flakes" >> "$HOME/.config/nix/nix.conf"
        fi
    fi
}

install_cachix() {
    log "Checking Cachix installation"
    if command_exists "cachix"; then
        log "Cachix is already installed."
    else
        log "Installing Cachix..."
        nix-env -iA nixpkgs.cachix
    fi
    log "Configuring Cachix caches"
    cachix use devenv
    cachix use mikefaille
}

install_devenv() {
    log "Checking devenv installation"
    local installed_version=""
    if command_exists "devenv"; then
        installed_version=$(devenv version 2>/dev/null || devenv --version 2>/dev/null || true)
    fi

    if [[ "$installed_version" == *"devenv ${DEVENV_VERSION#v}"* || "$installed_version" == *"devenv $DEVENV_VERSION"* ]]; then
        log "devenv $DEVENV_VERSION is already installed."
    else
        log "Installing devenv $DEVENV_VERSION..."
        nix profile install "github:cachix/devenv/$DEVENV_VERSION#devenv" --extra-experimental-features 'nix-command flakes'
    fi
}

install_direnv() {
    log "Checking direnv installation"
    if command_exists "direnv"; then
        log "direnv is already installed."
    else
        log "Installing direnv..."
        if command_exists "apt-get"; then
            sudo apt-get update && sudo apt-get install -y direnv
        elif command_exists "brew"; then
            brew install direnv
        else
            log "Could not detect package manager. Please install direnv manually: https://direnv.net/docs/installation.html"
            return
        fi
    fi

    log "Configuring direnv for shell"
    local shell_name
    shell_name=$(basename "$SHELL")
    
    case "$shell_name" in
        bash)
            if ! grep -q "direnv hook bash" "$HOME/.bashrc"; then
                # shellcheck disable=SC2016
                echo 'eval "$(direnv hook bash)"' >> "$HOME/.bashrc"
                log "Added direnv hook to .bashrc"
            fi
            ;;
        zsh)
            if ! grep -q "direnv hook zsh" "$HOME/.zshrc"; then
                # shellcheck disable=SC2016
                echo 'eval "$(direnv hook zsh)"' >> "$HOME/.zshrc"
                log "Added direnv hook to .zshrc"
            fi
            ;;
        *)
            log "Manual step: Please add 'eval \"\$(direnv hook $shell_name)\"' to your shell config."
            ;;
    esac
}

# --- Main ---

main() {
    install_nix
    install_cachix
    install_devenv
    install_direnv
    
    log "Setup complete! Please run 'direnv allow' in this directory."
}

main "$@"
