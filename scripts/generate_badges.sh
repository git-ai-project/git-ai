#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
CONFIG="$SCRIPT_DIR/badge_config.json"
ICONS_DIR="$REPO_ROOT/assets/docs/agents"
OUT_DIR="$REPO_ROOT/assets/docs/badges"

mkdir -p "$OUT_DIR"

height=100
right_width=${height}
padding=4
radius=12

count=$(jq length "$CONFIG")

for ((i = 0; i < count; i++)); do
  icon=$(jq -r ".[$i].icon" "$CONFIG")
  name=$(jq -r ".[$i].name" "$CONFIG")

  png="$ICONS_DIR/${icon}.png"
  if [[ ! -f "$png" ]]; then
    echo "WARNING: $png not found, skipping $icon"
    continue
  fi

  # Read PNG dimensions and compute icon width to preserve aspect ratio
  read png_w png_h < <(python3 -c "
import struct, zlib
with open('$png','rb') as f:
    f.read(16)
    w, h = struct.unpack('>II', f.read(8))
    print(w, h)
")
  # Per-icon padding overrides
  icon_padding=${padding}
  icon_y_offset=0
  icon_x_offset=0
  if [[ "$icon" == "claude_code" ]]; then
    icon_padding=12
  elif [[ "$icon" == "gemini" ]]; then
    icon_padding=10
    icon_y_offset=6
  elif [[ "$icon" == "rovodev" ]]; then
    icon_padding=6
    icon_y_offset=-3
  elif [[ "$icon" == "junie_white" ]]; then
    icon_padding=2
    icon_y_offset=-4
  elif [[ "$icon" == "cursor" ]]; then
    icon_y_offset=2
  elif [[ "$icon" == "copilot" ]]; then
    icon_padding=6
  elif [[ "$icon" == "droid" ]]; then
    icon_padding=6
  fi

  img_height=$(( height - icon_padding * 2 ))
  img_width=$(python3 -c "print(int(round($img_height * $png_w / $png_h)))")
  left_width=$(( img_width + icon_padding * 2 ))
  width=$(( left_width + right_width ))

  # Checkmark dimensions
  check_cx=$(( left_width + right_width / 2 ))
  check_cy=$(( height / 2 ))

  b64=$(base64 < "$png" | tr -d '\n')

  cat > "$OUT_DIR/${icon}.svg" <<SVGEOF
<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="${width}" height="${height}">
  <defs>
    <clipPath id="clip-${icon}">
      <rect width="${width}" height="${height}" rx="${radius}" ry="${radius}"/>
    </clipPath>
    <linearGradient id="green-${icon}" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0%" stop-color="#34D399"/>
      <stop offset="100%" stop-color="#16A34A"/>
    </linearGradient>
    <linearGradient id="gray-${icon}" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0%" stop-color="#F3F4F6"/>
      <stop offset="100%" stop-color="#E5E7EB"/>
    </linearGradient>
    <filter id="shadow-${icon}" x="-2%" y="-2%" width="104%" height="104%">
      <feDropShadow dx="0" dy="1" stdDeviation="1" flood-color="#000000" flood-opacity="0.1"/>
    </filter>
  </defs>
  <g clip-path="url(#clip-${icon})" filter="url(#shadow-${icon})">
    <rect width="${left_width}" height="${height}" fill="url(#gray-${icon})"/>
    <rect x="${left_width}" width="${right_width}" height="${height}" fill="url(#green-${icon})"/>
  </g>
  <image x="${icon_padding}" y="$(( icon_padding + icon_y_offset ))" width="${img_width}" height="${img_height}" xlink:href="data:image/png;base64,${b64}"/>
  <polyline points="$(( check_cx - 12 )),${check_cy} $(( check_cx - 4 )),$(( check_cy + 12 )) $(( check_cx + 14 )),$(( check_cy - 12 ))" fill="none" stroke="#FFFFFF" stroke-width="5" stroke-linecap="round" stroke-linejoin="round"/>
  <line x1="${left_width}" y1="0" x2="${left_width}" y2="${height}" stroke="#9CA3AF" stroke-width="1.5"/>
  <rect width="${width}" height="${height}" rx="${radius}" ry="${radius}" fill="none" stroke="#9CA3AF" stroke-width="3"/>
</svg>
SVGEOF

  echo "Generated $OUT_DIR/${icon}.svg"
done

echo "Done. ${count} badges generated in $OUT_DIR"
