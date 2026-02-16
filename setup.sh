#!/usr/bin/env bash
#
# setup.sh: Idempotent Development Environment Setup
#
# This script prepares the development environment by installing Nix, devenv,
# and direnv with nix-direnv for optimal performance.

set -euo pipefail

# --- Constants ---
DEVENV_VERSION="v1.11.2"

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
    if command_exists "devenv"; then
        log "devenv is already installed."
    else
        log "Installing devenv $DEVENV_VERSION..."
        nix profile install "github:cachix/devenv/$DEVENV_VERSION"
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
