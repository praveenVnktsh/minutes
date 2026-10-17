# GitHub Actions Workflows Overview

Minutes is a Mac-only app (Apple Silicon, macOS 13+). Every workflow that builds the app targets `aarch64-apple-darwin` on a `macos-latest` runner. Jobs that only manage releases or validate configuration run on `ubuntu-latest`, because they build nothing.

## Automatic Workflows

### pr-check.yml (PR Check)
Runs `scripts/pr-check.sh` on every pull request to `main` and on every push to `main`. It must pass before a pull request can merge.

### canary-release.yml (Canary Release)
Runs on every push to `main`. Builds the macOS app through `build.yml` and publishes it to the rolling `canary` release, which canary installs auto-update from.

### promote-stable.yml (Promote Stable)
Runs daily. Promotes the newest canary build to a stable `vX.Y.Z` release. It refuses to promote a canary whose `latest.json` has no `darwin-aarch64` entry.

### pages.yml
Publishes the `site/` directory to GitHub Pages.

## Manual Workflows

### build-devtest.yml (DevTest Builds)
Unsigned by default, with optional signing. 14-day artifact retention. Use it for routine development builds.

### build-macos.yml (macOS Standalone Builds)
Debug or release builds with optional Apple signing, notarization and signature verification. Outputs a `.dmg` installer and an `.app` bundle.

### build-test.yml (Signed Test Builds)
Calls `build.yml` with signing on. 30-day artifact retention, artifacts prefixed `meetily-test-`.

### release.yml (Production Release)
Creates a draft GitHub Release, builds the macOS app into it, and adds the stable-named alias `minutes-macos-arm64.dmg`. Outputs the DMG installer, `app.tar.gz` (updater), `.sig`, and `latest.json`.

### pr-main-check.yml (Release Validation)
Checks the version format and branch before a release. It builds nothing.

### build.yml (Reusable Build Workflow)
Not triggered directly. `build-test.yml`, `release.yml` and `canary-release.yml` call it.

## Workflow Dependencies

```
build.yml (reusable)
    |-- build-test.yml
    |-- release.yml
    |-- canary-release.yml

Standalone:
    |-- pr-check.yml
    |-- build-macos.yml
    |-- build-devtest.yml
    |-- promote-stable.yml
    |-- pr-main-check.yml
    |-- pages.yml
```

## Required Secrets

### macOS Signing
- `APPLE_CERTIFICATE`: Developer ID certificate (base64)
- `APPLE_CERTIFICATE_PASSWORD`: certificate password
- `APPLE_ID`: Apple ID email
- `APPLE_PASSWORD`: app-specific password
- `APPLE_TEAM_ID`: team ID
- `KEYCHAIN_PASSWORD`: temporary keychain password

### Tauri Updater
- `TAURI_SIGNING_PRIVATE_KEY`: Ed25519 private key
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`: key password

### Application Configuration
- `MEETILY_RSA_PUBLIC_KEY`: license validation public key
- `SUPABASE_URL`: online license verification
- `SUPABASE_ANON_KEY`: Supabase anonymous key

## Troubleshooting

### Version errors
Tauri and Apple require a plain numeric `X.Y.Z` version in `tauri.conf.json`. Use `0.1.3`, not `0.1.2-pro-trial`.

### Signing fails
Verify the secrets are configured and unexpired, then read the workflow logs for the specific error.

### Artifacts not available
Check the build succeeded, that `upload-artifacts` was enabled, and that the retention period has not passed.

## Related Documentation

- `README_DEVTEST.md` for DevTest builds
- `ACCELERATION_GUIDE.md` for Metal and CoreML
