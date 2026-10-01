#!/bin/bash
# Let other computers on your network reach MouseTail through this computer's firewall (ufw,
# as on Omarchy, or firewalld, as on Fedora): opens MouseTail's UDP port to local networks
# only. Asks for your password once; the setting sticks across reboots.
#   scripts/enable-firewall-linux.sh [port]
set -euo pipefail
port=${1:-}
if [[ -z $port ]]; then
  # The port MouseTail is using (its status starts "name (id)  UDP <port>").
  port=$("$HOME/.local/bin/mousetail" status 2>/dev/null | awk 'NR == 1 && $(NF-1) == "UDP" { print $NF }' || true)
fi
port=${port:-24802}
local_networks=(10.0.0.0/8 172.16.0.0/12 192.168.0.0/16)

if command -v ufw >/dev/null && grep -qsx 'ENABLED=yes' /etc/ufw/ufw.conf; then
  for net in "${local_networks[@]}"; do
    sudo ufw allow proto udp from "$net" to any port "$port" comment MouseTail >/dev/null
  done
elif command -v firewall-cmd >/dev/null && [[ $(firewall-cmd --state 2>/dev/null) == running ]]; then
  for net in "${local_networks[@]}"; do
    sudo firewall-cmd --permanent --quiet \
      --add-rich-rule="rule family=ipv4 source address=$net port port=$port protocol=udp accept"
  done
  sudo firewall-cmd --reload --quiet
else
  echo "This computer's firewall isn't on, so there's nothing to change."
  exit 0
fi
echo "Done. Other computers on your network can now reach MouseTail here."
