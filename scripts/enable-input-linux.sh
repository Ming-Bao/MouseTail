#!/bin/bash
# Let MouseTail control this computer on desktops that don't offer Wayland's virtual-input
# protocols (GNOME, KDE, X11). Grants the logged-in user access to /dev/uinput. Asks for your
# password once; the setting sticks across reboots.
#   scripts/enable-input-linux.sh
set -euo pipefail
rule=/etc/udev/rules.d/60-mousetail-uinput.rules
echo 'KERNEL=="uinput", SUBSYSTEM=="misc", TAG+="uaccess", OPTIONS+="static_node=uinput"' | sudo tee "$rule" >/dev/null
echo uinput | sudo tee /etc/modules-load.d/mousetail-uinput.conf >/dev/null
sudo modprobe uinput
sudo udevadm control --reload-rules
sudo udevadm trigger --name-match=uinput
systemctl --user restart mousetail.service 2>/dev/null || true
echo "Done. Other computers can now control this one."
