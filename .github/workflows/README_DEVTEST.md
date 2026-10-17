# DevTest Build Workflow

This document explains how to use the `build-devtest.yml` workflow for building and testing.

## Overview

The DevTest workflow is for development and testing. It:
- Builds the macOS app for Apple Silicon (`aarch64-apple-darwin`), the only supported platform
- Has **code signing disabled by default** to speed up builds
- Allows **optional signing** via workflow dispatch input
- Uploads artifacts for testing

## Triggering the Workflow

The workflow runs via **manual dispatch only**:

1. Go to the **Actions** tab in the GitHub repository
2. Select **Build and Test - DevTest** from the left sidebar
3. Click **Run workflow**
4. Configure options:
   - **Branch**: the branch to build
   - **Sign the build**: check to enable code signing (default: unchecked)
   - **Upload build artifacts**: check to upload artifacts (default: checked)
5. Click **Run workflow** to start

## Workflow Options

### Sign the build
- **Unchecked (default)**: fast builds without code signing
- **Checked**: Apple code signing and notarization

### Upload build artifacts
- **Checked (default)**: artifacts are uploaded and available for download
- **Unchecked**: the build runs but no artifacts are saved

## Code Signing Details

When signing is enabled, the workflow:
- Uses the **Apple Developer Certificate** from secrets
- Performs **notarization** with the Apple ID
- Signs both the DMG and the `.app` bundle
- Verifies signatures with `codesign` and `spctl`

Tauri updater signing (Ed25519) always runs, so the `.sig` files are produced either way.

## Build Artifacts

Artifacts are retained for **14 days**: `*.dmg`, `*.app`, `*.app.tar.gz`, `*.app.tar.gz.sig`.

To download them, open the completed run in the **Actions** tab and use the **Artifacts** section.

## Hardware Acceleration

Whisper uses Metal and CoreML; the llama-helper sidecar is built with `--features metal`. See `ACCELERATION_GUIDE.md`.

## Troubleshooting

### Signing not working
1. Verify the required secrets are configured: `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID`, `KEYCHAIN_PASSWORD`, `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`
2. Check the workflow logs for the specific error
3. Run without signing first to isolate the issue

### Artifacts not available
1. Artifacts are only available after a successful build
2. Artifacts expire after 14 days
3. Ensure "Upload build artifacts" was checked

## Related Workflows

- `build-macos.yml`: macOS builds with signing, debug or release
- `build-test.yml`: signed builds through the shared `build.yml`
- `release.yml`: production release workflow
