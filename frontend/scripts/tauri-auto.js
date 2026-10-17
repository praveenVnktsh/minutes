#!/usr/bin/env node
/**
 * Run `tauri dev|build` with the Apple Silicon acceleration feature.
 *
 * whisper-rs already builds with Metal + CoreML on macOS; the `coreml` Cargo
 * feature is passed explicitly so the build matches what it always used on
 * Apple Silicon. Override with TAURI_GPU_FEATURE (e.g. `metal`, or `none`).
 */

const { execSync } = require('child_process');

const command = process.argv[2];
if (!command || !['dev', 'build'].includes(command)) {
  console.error('Usage: node tauri-auto.js [dev|build]');
  process.exit(1);
}

const feature = process.env.TAURI_GPU_FEATURE || 'coreml';

let tauriCmd = `tauri ${command}`;
if (feature !== 'none') {
  tauriCmd += ` -- --features ${feature}`;
}
console.log(`Running: ${tauriCmd}`);

try {
  execSync(tauriCmd, { stdio: 'inherit' });
} catch (err) {
  process.exit(err.status || 1);
}
