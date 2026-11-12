import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { readFile } from 'node:fs/promises';

// Top-level scalars of the flat latest.yml and app-update.yml files that
// electron-builder writes; nested lists such as `files` are skipped.
export async function readUpdateYaml(filePath) {
  const values = {};
  for (const line of (await readFile(filePath, 'utf8')).split(/\r?\n/)) {
    const match = /^([A-Za-z]\w*): (.+)$/.exec(line);
    if (match) values[match[1]] = match[2].replace(/^'(.*)'$/, '$1');
  }
  return values;
}

// The digest format electron-updater checks downloads against.
export async function sha512Base64(filePath) {
  const hash = createHash('sha512');
  for await (const chunk of createReadStream(filePath)) hash.update(chunk);
  return hash.digest('base64');
}
