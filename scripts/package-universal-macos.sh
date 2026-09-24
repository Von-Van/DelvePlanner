#!/usr/bin/env bash
set -euo pipefail

input=${1:?input directory required}
output=${2:?output directory required}
arm_app=$(find "$input" -path '*aarch64*' -name 'Delve Planner.app' -type d -print -quit)
intel_app=$(find "$input" -path '*x86_64*' -name 'Delve Planner.app' -type d -print -quit)
test -n "$arm_app"
test -n "$intel_app"
mkdir -p "$output"
ditto "$arm_app" "$output/Delve Planner.app"
lipo -create "$arm_app/Contents/MacOS/delve-planner-desktop" "$intel_app/Contents/MacOS/delve-planner-desktop" -output "$output/Delve Planner.app/Contents/MacOS/delve-planner-desktop"
find "$output/Delve Planner.app/Contents/Resources/resources/ollama" -type f -perm -111 -exec codesign --force --options runtime --timestamp --sign "$APPLE_SIGNING_IDENTITY" {} \;
codesign --force --deep --options runtime --timestamp --sign "$APPLE_SIGNING_IDENTITY" "$output/Delve Planner.app"
codesign --verify --deep --strict --verbose=2 "$output/Delve Planner.app"
