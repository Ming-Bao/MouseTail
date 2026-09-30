#!/bin/sh
# Receives typed text in the keyboard spike.
read -r x
printf "%s" "$x" > /tmp/mousetail-kbd
