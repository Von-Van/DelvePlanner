#!/usr/bin/env bash
set -euo pipefail

app=${1:?app path required}
dmg=${2:?DMG path required}
key_file=$(mktemp)
submission=$(mktemp -d)
trap 'rm -f "$key_file"; rm -rf "$submission"' EXIT
printf '%s' "$APPLE_API_KEY" > "$key_file"
ditto -c -k --sequesterRsrc --keepParent "$app" "$submission/Delve-Planner.zip"
xcrun notarytool submit "$submission/Delve-Planner.zip" --key "$key_file" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER" --wait
xcrun stapler staple "$app"
xcrun stapler validate "$app"
mkdir -p "$submission/dmg-root"
ditto "$app" "$submission/dmg-root/$(basename "$app")"
hdiutil create -volname "Delve Planner" -srcfolder "$submission/dmg-root" -ov -format UDZO "$dmg"
codesign --force --timestamp --sign "$APPLE_SIGNING_IDENTITY" "$dmg"
xcrun notarytool submit "$dmg" --key "$key_file" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER" --wait
xcrun stapler staple "$dmg"
xcrun stapler validate "$dmg"
spctl --assess --type open --context context:primary-signature -v "$dmg"
updater=${dmg%.dmg}.app.tar.gz
tar -czf "$updater" -C "$(dirname "$app")" "$(basename "$app")"
npx tauri signer sign "$updater"
