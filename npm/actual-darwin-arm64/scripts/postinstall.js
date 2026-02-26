'use strict';
const path = require('path');
const fs = require('fs');
const { execFileSync } = require('child_process');

if (process.platform !== 'darwin') {
  process.exit(0);
}

const binPath = path.join(__dirname, '..', 'bin', 'actual');

if (!fs.existsSync(binPath)) {
  process.exit(0);
}

try {
  execFileSync('codesign', ['--sign', '-', binPath], { stdio: 'pipe' });
  console.log('actual: ad-hoc signed for macOS (Apple Developer signing coming soon)');
} catch (_err) {
  console.log(
    'actual: ad-hoc signing skipped \u2014 Apple Developer application is under review.\n' +
    'Apple-approved code signing is coming soon.\n' +
    'If macOS Gatekeeper blocks the binary, run:\n' +
    '  xattr -d com.apple.quarantine $(which actual)'
  );
  process.exit(0);
}
