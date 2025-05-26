#!/bin/bash

URL="https://boot.controlnet.space/files/envira"
OUTPUT="/tmp/envira"
curl -fsSL "$URL" -o "$OUTPUT"
chmod +x "$OUTPUT"
exec "$OUTPUT" "$@"