#!/usr/bin/env node
'use strict';
const { execFileSync } = require('child_process');
const os = require('os');

const PLATFORM_MAP = {
  'darwin-arm64': '@actualai/actual-darwin-arm64',
  'darwin-x64':   '@actualai/actual-darwin-x64',
  'linux-x64':    '@actualai/actual-linux-x64',
  'linux-arm64':  '@actualai/actual-linux-arm64',
};

const key = `${os.platform()}-${os.arch()}`;
const pkg = PLATFORM_MAP[key];
if (!pkg) {
  console.error(`actual: unsupported platform: ${key}`);
  process.exit(1);
}

let bin;
try {
  bin = require.resolve(`${pkg}/bin/actual`);
} catch {
  console.error(
    `actual: platform package "${pkg}" not installed. Try reinstalling with npm install.`
  );
  process.exit(1);
}

try {
  execFileSync(bin, process.argv.slice(2), { stdio: 'inherit' });
} catch (e) {
  process.exit(e.status ?? 1);
}
