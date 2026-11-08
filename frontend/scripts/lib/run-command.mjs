import { spawn } from 'node:child_process';
import path from 'node:path';

export function runCommand(command, args, {
  cwd,
  env = process.env,
  label = path.basename(command),
} = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd,
      env,
      stdio: 'inherit',
    });
    child.once('error', reject);
    child.once('exit', (code, signal) => {
      if (code === 0) resolve();
      else {
        reject(new Error(
          `${label} terminó con ${code ?? signal ?? 'estado desconocido'}`,
        ));
      }
    });
  });
}
