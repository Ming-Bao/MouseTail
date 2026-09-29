#!/bin/sh
# Records the raw bytes of the next 3 key presses (so Ctrl+T shows up as 0x14).
stty raw -echo
head -c 3 > /tmp/bm-raw
