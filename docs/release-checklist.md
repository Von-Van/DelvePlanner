# Delve Planner Public Beta Release Checklist

This checklist gates publishing an existing draft GitHub Release. Record the tested build/tag and both machine environments in the release notes.

## One-time GitHub configuration

- [ ] Protect the `public-beta-publish` environment with required maintainer reviewers.
- [ ] Store the Developer ID `.p12` as base64 in `APPLE_CERTIFICATE`, its password in `APPLE_CERTIFICATE_PASSWORD`, the exact signing identity in `APPLE_SIGNING_IDENTITY`, and the raw App Store Connect `.p8` contents plus key ID/issuer in `APPLE_API_KEY`, `APPLE_API_KEY_ID`, and `APPLE_API_ISSUER`.
- [ ] Store the Tauri private signing key/password as `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, with its public key in the `TAURI_UPDATER_PUBKEY` repository variable.
- [ ] Configure Azure workload-identity secrets `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, and `AZURE_SUBSCRIPTION_ID`, plus `AZURE_SIGNING_ENDPOINT`, `AZURE_SIGNING_ACCOUNT`, and `AZURE_SIGNING_PROFILE` repository variables.

## Automated gates

- [ ] All Desktop CI jobs pass on the tagged commit.
- [ ] Production npm audit has no high/critical advisory.
- [ ] Rust advisory, license, and source-policy checks pass.
- [ ] Rust formatting, clippy, tests, frontend tests, and production builds pass.
- [ ] The pinned Ollama manifest, MIT notice, archive checksums, runtime version, and SBOM entries are verified.
- [ ] Three committed live-eval runs use the same recorded `qwen3:8b` digest.
- [ ] Every eval run meets 100% schema, 100% safety, 85% exact, and 95% field accuracy.
- [ ] Draft release contains notarized universal DMG, Azure-signed x64 NSIS installer, updater artifacts/signatures, `latest.json`, checksums, notes, and SBOM.

## macOS 13+ universal

- [ ] `codesign`, Gatekeeper assessment, notarization, and staple verification pass.
- [ ] Clean install and first-run onboarding work on Apple Silicon.
- [ ] Launch and core agenda flow work on Intel hardware or an Intel runner artifact.
- [ ] Bundled runtime missing/start failure, downloading/cancel/retry, model-not-installed, and model-ready states are clear.
- [ ] A running system Ollama on port 11434 is ignored; Delve Planner uses only its private managed endpoint and model directory.
- [ ] Runtime executables for both macOS architectures are signed inside the app bundle.
- [ ] Events/tasks persist across restart; closing the window retains tray delivery.
- [ ] Schema-1 migration creates a backup and preserves data.
- [ ] Corrupt-database startup offers recovery without deleting data.
- [ ] JSON export/import round-trip is exact; import backup restores correctly.
- [ ] Reminder fires while the window is closed; permission denial prevents event/proposal mutation.
- [ ] Stale proposal is rejected atomically.
- [ ] A planner proposal can be accepted in part: rejecting a new plan also rejects what depended on it, and only the accepted suggestions are written.
- [ ] Subscribing to a calendar link and importing an `.ics` file both show events on Today and Week; the link is saved in the login Keychain, and removing the calendar deletes it.
- [ ] Connecting a Google and an Outlook account signs in through the browser, the consent screen lists only read-only calendar access, the picker adds the chosen calendars, and their events appear on Today and Week.
- [ ] Disconnecting an account removes its calendars and events, deletes its Keychain item, and the grant is gone from the provider's account settings.
- [ ] After updating from the previous build, saved calendar links and account tokens refresh without a Keychain prompt.
- [ ] Blocking time for a task, releasing a finished task's future blocks, and planned-versus-available capacity work, including after changing working hours.
- [ ] What Delve Planner Knows: a day marked off holds no working time, an observation can be switched off and back on, and forgetting the profile or the history empties them without touching the schedule.
- [ ] Opening Delve Planner starts no `ollama` process; asking the planner something starts one, and quitting Delve Planner leaves no `ollama` or model-runner process behind (`pgrep -fl ollama`).
- [ ] After a force quit, the next launch ends the model server the previous one left running.
- [ ] A model installed outside Delve Planner appears in the picker, plans after its format check, and survives **Remove model**.
- [ ] Manual update from the previous beta shows notes, asks for confirmation, installs, and relaunches.

## Windows 10 22H2 / Windows 11 x64

- [ ] Authenticode status is valid and SmartScreen/install identity is reviewed.
- [ ] Clean NSIS install and first-run onboarding work.
- [ ] Bundled runtime missing/start failure, downloading/cancel/retry, model-not-installed, and model-ready states are clear.
- [ ] A running system Ollama on port 11434 is ignored and the bundled runtime executables are Authenticode signed.
- [ ] Events/tasks persist across restart; tray behavior is correct.
- [ ] Migration, recovery, JSON round-trip, stale-proposal rejection, and permission denial match macOS.
- [ ] Calendar links and account refresh tokens are saved in Credential Manager, calendars refresh while Delve Planner runs in the tray, and account connection, the picker, disconnect, time blocks, and capacity match macOS.
- [ ] Quitting Delve Planner ends `ollama.exe` and its model runner (Task Manager shows neither), and the model picker lists models installed outside Delve Planner.
- [ ] Installed-build notification fires while the window is closed and never includes notes.
- [ ] Manual update from the previous beta verifies and installs the signed NSIS updater.

## Publish approval

- [ ] Release notes document known limitations and reminder tray lifecycle.
- [ ] Diagnostic ZIP is manually inspected for commands, titles, notes, proposals, or paths.
- [ ] A maintainer confirms `native_smoke_tests_passed` and dispatches **Publish Approved Beta**.
