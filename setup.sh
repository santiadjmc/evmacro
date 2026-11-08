#!/usr/bin/env bash
set -euo pipefail

echo 'KERNEL=="uinput", SUBSYSTEM=="misc", GROUP="input", MODE="0660", OPTIONS+="static_node=uinput"' \
  | sudo tee /etc/udev/rules.d/99-evmacro.rules >/dev/null
echo uinput | sudo tee /etc/modules-load.d/evmacro.conf >/dev/null
sudo modprobe uinput
sudo usermod -aG input "$USER"
sudo udevadm control --reload-rules
sudo udevadm trigger

echo "Done. Log out and back in (group membership applies to new sessions)."
