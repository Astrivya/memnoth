#!/usr/bin/env node

// Keep npm's `latest` dist-tag monotonic across stable releases. Beta and
// nightly versions always remain isolated on their own channel tags.

const PACKAGE = '@astrivya/mcp';
const REGISTRY = 'https://registry.npmjs.org/@astrivya%2fmcp';

function stableParts(version) {
  const match = /^v?(\d+)\.(\d+)\.(\d+)$/.exec(version ?? '');
  return match ? match.slice(1).map(BigInt) : null;
}

function isGreater(left, right) {
  const a = stableParts(left);
  const b = stableParts(right);
  if (!a || !b) return false;
  for (let index = 0; index < 3; index += 1) {
    if (a[index] !== b[index]) return a[index] > b[index];
  }
  return false;
}

export function chooseTag(channel, version, currentLatest) {
  if (channel === 'beta' || channel === 'nightly') return channel;
  if (channel !== 'stable') throw new Error(`unsupported release channel: ${channel}`);
  if (!stableParts(version)) throw new Error(`stable release version is not x.y.z: ${version}`);
  return isGreater(currentLatest, version) ? 'release' : 'latest';
}

async function main() {
  if (process.argv[2] === '--self-test') {
    const cases = [
      ['stable', '0.1.2', '0.1.1', 'latest'],
      ['stable', '0.1.1', '0.1.2', 'release'],
      ['stable', '1.0.0', '0.9.99', 'latest'],
      ['stable', '0.2.0', '0.2.0-beta.3', 'latest'],
      ['beta', '0.2.0-beta.1', '0.1.1', 'beta'],
      ['nightly', '0.2.0-nightly.1', '0.1.1', 'nightly'],
    ];
    for (const [channel, version, current, expected] of cases) {
      if (chooseTag(channel, version, current) !== expected) {
        throw new Error(`npm dist-tag self-test failed for ${channel} ${version} over ${current}`);
      }
    }
    console.log('npm dist-tag self-test passed');
    return;
  }

  const [channel, version] = process.argv.slice(2);
  if (!channel || !version) throw new Error('usage: npm-dist-tag.mjs <stable|beta|nightly> <version>');

  let currentLatest;
  if (channel === 'stable') {
    const response = await fetch(`${REGISTRY}?memnoth_check=${Date.now()}`, { cache: 'no-store' });
    if (response.ok) {
      const packument = await response.json();
      currentLatest = packument['dist-tags']?.latest;
    } else if (response.status !== 404) {
      throw new Error(`npm registry returned HTTP ${response.status}`);
    }
  }

  const tag = chooseTag(channel, version, currentLatest);
  console.error(`${PACKAGE}@${version}: npm dist-tag ${tag}${currentLatest ? ` (current latest: ${currentLatest})` : ''}`);
  console.log(tag);
}

main().catch((error) => {
  console.error(error.message);
  process.exitCode = 1;
});
