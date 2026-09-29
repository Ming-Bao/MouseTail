#!/bin/bash
# Let this Linux machine be woken from sleep by MouseTail (Wake-on-LAN), so pushing the
# cursor towards it wakes it. Needs your password once; the setting sticks across reboots.
#   scripts/enable-wake-linux.sh
set -euo pipefail
command -v nmcli >/dev/null || { echo "This needs NetworkManager (nmcli)."; exit 1; }

nmcli -t -f NAME,TYPE,DEVICE connection show --active | while IFS=: read -r name type device; do
  case $type in
    802-11-wireless)
      echo "Wi-Fi \"$name\": wake on magic packet"
      sudo nmcli connection modify "$name" 802-11-wireless.wake-on-wlan magic
      ;;
    802-3-ethernet)
      echo "Ethernet \"$name\": wake on magic packet"
      sudo nmcli connection modify "$name" 802-3-ethernet.wake-on-lan magic
      ;;
    *) continue ;;
  esac
  # Apply now (reconnects briefly).
  sudo nmcli connection up "$name" >/dev/null
done
echo "Done. While this computer sleeps, moving your Mac's cursor towards it will wake it."
