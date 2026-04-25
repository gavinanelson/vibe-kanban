#!/usr/bin/env node
const command = process.argv.slice(2).join(' ') || 'heavy validation';
const allowed = ['ALLOW_HEAVY_VIBE_VALIDATION', 'VIBE_ALLOW_HEAVY_VALIDATION', 'CI'].some(
  (name) => ['1', 'true', 'yes'].includes(String(process.env[name] || '').toLowerCase()),
);
if (!allowed) {
  console.error(`Blocked local heavyweight validation: ${command}`);
  console.error('This repo can saturate Host A/omarchy CPU and regenerate multi-GB Cargo target trees.');
  console.error('Use focused checks, CI/off-host validation, or rerun with ALLOW_HEAVY_VIBE_VALIDATION=1 when explicitly approved.');
  process.exit(42);
}
