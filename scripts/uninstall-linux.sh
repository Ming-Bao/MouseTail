#!/bin/bash
# Remove MouseTail from this user account (undoes scripts/install-linux.sh).
#   scripts/uninstall-linux.sh [--forget]    --forget also deletes pairings and identity
set -uo pipefail
config_home=${XDG_CONFIG_HOME:-$HOME/.config}
plugin_id=nz.galengreen.mousetail
omarchy=$config_home/omarchy

systemctl --user disable --now mousetail.service 2>/dev/null
rm -f "$config_home/systemd/user/mousetail.service"
systemctl --user daemon-reload
rm -f "$HOME/.local/bin/mousetail"

if [[ -d $omarchy ]]; then
  rm -rf "$omarchy/plugins/$plugin_id"
  shell=$omarchy/shell.json
  if [[ -f $shell ]] && command -v jq >/dev/null; then
    jq --arg id "$plugin_id" '.bar.layout |= (if . == null then . else map_values(map(select(.id != $id))) end)' \
      "$shell" > "$shell.tmp" && mv "$shell.tmp" "$shell"
  fi
fi

[[ ${1:-} == --forget ]] && rm -rf "$config_home/mousetail"
echo "MouseTail removed."
