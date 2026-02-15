#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
CONFIG="$SCRIPT_DIR/badge_config.json"
ICONS_DIR="$REPO_ROOT/assets/docs/agents"
OUT_DIR="$REPO_ROOT/assets/docs/badges"

mkdir -p "$OUT_DIR"

width=80
height=35
left_width=54
right_width=$(( width - left_width ))
icon_size=33
icon_pad_x=$(( (left_width - icon_size) / 2 ))
icon_pad_y=$(( (height - icon_size) / 2 ))
radius=8

# Checkmark dimensions
check_cx=$(( left_width + right_width / 2 ))
check_cy=$(( height / 2 ))

count=$(jq length "$CONFIG")

for ((i = 0; i < count; i++)); do
  icon=$(jq -r ".[$i].icon" "$CONFIG")
  name=$(jq -r ".[$i].name" "$CONFIG")

  png="$ICONS_DIR/${icon}.png"
  if [[ ! -f "$png" ]]; then
    echo "WARNING: $png not found, skipping $icon"
    continue
  fi

  b64=$(base64 < "$png" | tr -d '\n')

  cat > "$OUT_DIR/${icon}.svg" <<SVGEOF
<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="${width}" height="${height}">
  <clipPath id="clip-${icon}">
    <rect width="${width}" height="${height}" rx="${radius}" ry="${radius}"/>
  </clipPath>
  <g clip-path="url(#clip-${icon})">
    <rect width="${left_width}" height="${height}" fill="#FFFFFF"/>
    <rect x="${left_width}" width="${right_width}" height="${height}" fill="#4ADE80"/>
  </g>
  <image x="${icon_pad_x}" y="${icon_pad_y}" width="${icon_size}" height="${icon_size}" xlink:href="data:image/png;base64,${b64}"/>
  <polyline points="$(( check_cx - 6 )),${check_cy} $(( check_cx - 2 )),$(( check_cy + 5 )) $(( check_cx + 7 )),$(( check_cy - 5 ))" fill="none" stroke="#FFFFFF" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"/>
  <rect width="${width}" height="${height}" rx="${radius}" ry="${radius}" fill="none" stroke="#F97316" stroke-width="1.5"/>
</svg>
SVGEOF

  echo "Generated $OUT_DIR/${icon}.svg"
done

echo "Done. ${count} badges generated in $OUT_DIR"
