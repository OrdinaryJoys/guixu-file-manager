#!/bin/zsh
PROJECT_DIR="${0:A:h}"
WEB_DIR="$PROJECT_DIR/apps/desktop/web"
URL="http://127.0.0.1:8765/?demo=1"

cd "$WEB_DIR" || exit 1
open "$URL"
exec python3 -m http.server 8765 --bind 127.0.0.1
