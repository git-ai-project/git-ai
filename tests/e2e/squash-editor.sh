#!/bin/bash
# Change 'pick' to 'squash' for all commits except the very first one (in non-comment lines).
awk '
  /^#/ { print; next }
  /^pick / {
    if (++count == 1) {
      print
    } else {
      sub(/^pick /,"squash ")
      print
    }
    next
  }
  { print }
' "$1" > "$1.tmp"
mv "$1.tmp" "$1"