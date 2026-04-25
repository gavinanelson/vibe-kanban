import { execFileSync, execSync, spawn } from 'child_process';
import path from 'path';
import fs from 'fs';
import os from 'os';
import type { DesktopBundleInfo } from './download';

type TauriPlatform = string | null;

interface SentinelMeta {
  type: string;
  appPath: string;
}

interface HyprlandClient {
  address?: string;
  class?: string;
  initialClass?: string;
  title?: string;
  mapped?: boolean;
  hidden?: boolean;
}

const PLATFORM_MAP: Record<string, string> = {
  'macos-arm64': 'darwin-aarch64',
  'macos-x64': 'darwin-x86_64',
  'linux-x64': 'linux-x86_64',
  'linux-arm64': 'linux-aarch64',
  'windows-x64': 'windows-x86_64',
  'windows-arm64': 'windows-aarch64',
};

// Map NPX-style platform names to Tauri-style platform names
export function getTauriPlatform(
  npxPlatformDir: string
): TauriPlatform {
  return PLATFORM_MAP[npxPlatformDir] || null;
}

// Extract .tar.gz using system tar (available on macOS, Linux, and Windows 10+)
function extractTarGz(archivePath: string, destDir: string): void {
  execSync(`tar -xzf "${archivePath}" -C "${destDir}"`, {
    stdio: 'pipe',
  });
}

function writeSentinel(dir: string, meta: SentinelMeta): void {
  fs.writeFileSync(
    path.join(dir, '.installed'),
    JSON.stringify(meta)
  );
}

function readSentinel(dir: string): SentinelMeta | null {
  const sentinelPath = path.join(dir, '.installed');
  if (!fs.existsSync(sentinelPath)) return null;
  try {
    return JSON.parse(
      fs.readFileSync(sentinelPath, 'utf-8')
    ) as SentinelMeta;
  } catch {
    return null;
  }
}

// Try to copy the .app to a destination directory, returning the final path on success
function tryCopyApp(
  srcAppPath: string,
  destDir: string
): string | null {
  try {
    const appName = path.basename(srcAppPath);
    const destAppPath = path.join(destDir, appName);

    // Ensure destination directory exists
    fs.mkdirSync(destDir, { recursive: true });

    // Remove existing app at destination if present
    if (fs.existsSync(destAppPath)) {
      fs.rmSync(destAppPath, { recursive: true, force: true });
    }

    // Use cp -R for macOS .app bundles (preserves symlinks and metadata)
    execSync(`cp -R "${srcAppPath}" "${destAppPath}"`, {
      stdio: 'pipe',
    });

    return destAppPath;
  } catch {
    return null;
  }
}

// macOS: extract .app.tar.gz, copy to /Applications, remove quarantine, launch with `open`
async function installAndLaunchMacOS(
  bundleInfo: DesktopBundleInfo
): Promise<number> {
  const { archivePath, dir } = bundleInfo;

  const sentinel = readSentinel(dir);
  if (sentinel?.appPath && fs.existsSync(sentinel.appPath)) {
    return launchMacOSApp(sentinel.appPath);
  }

  if (!archivePath || !fs.existsSync(archivePath)) {
    throw new Error('No archive to extract for macOS desktop app');
  }

  extractTarGz(archivePath, dir);

  const appName = fs.readdirSync(dir).find((f) => f.endsWith('.app'));
  if (!appName) {
    throw new Error(
      `No .app bundle found in ${dir} after extraction`
    );
  }

  const extractedAppPath = path.join(dir, appName);

  // Try to install to /Applications, then ~/Applications, then fall back to cache dir
  const userApplications = path.join(os.homedir(), 'Applications');
  const finalAppPath =
    tryCopyApp(extractedAppPath, '/Applications') ??
    tryCopyApp(extractedAppPath, userApplications) ??
    extractedAppPath;

  // Clean up extracted copy if we successfully copied elsewhere
  if (finalAppPath !== extractedAppPath) {
    try {
      fs.rmSync(extractedAppPath, { recursive: true, force: true });
    } catch {}
  }

  // Remove quarantine attribute (app is already signed and notarized in CI)
  try {
    execSync(`xattr -rd com.apple.quarantine "${finalAppPath}"`, {
      stdio: 'pipe',
    });
  } catch {}

  writeSentinel(dir, { type: 'app-tar-gz', appPath: finalAppPath });

  return launchMacOSApp(finalAppPath);
}

function launchMacOSApp(appPath: string): Promise<number> {
  const appName = path.basename(appPath);
  console.error(`Launching ${appName}...`);
  const proc = spawn('open', ['--wait-apps', appPath], {
    stdio: 'inherit',
  });
  return new Promise((resolve) => {
    proc.on('exit', (code) => resolve(code || 0));
  });
}

// Linux: extract AppImage.tar.gz, chmod +x, run
async function installAndLaunchLinux(
  bundleInfo: DesktopBundleInfo
): Promise<number> {
  const { archivePath, dir } = bundleInfo;

  const sentinel = readSentinel(dir);
  if (sentinel?.appPath && fs.existsSync(sentinel.appPath)) {
    return launchLinuxAppImage(sentinel.appPath);
  }

  if (!archivePath || !fs.existsSync(archivePath)) {
    throw new Error('No archive to extract for Linux desktop app');
  }

  extractTarGz(archivePath, dir);

  const appImage = fs
    .readdirSync(dir)
    .find((f) => f.endsWith('.AppImage'));
  if (!appImage) {
    throw new Error(`No .AppImage found in ${dir} after extraction`);
  }

  const appImagePath = path.join(dir, appImage);
  fs.chmodSync(appImagePath, 0o755);

  writeSentinel(dir, {
    type: 'appimage-tar-gz',
    appPath: appImagePath,
  });

  return launchLinuxAppImage(appImagePath);
}

function launchLinuxAppImage(appImagePath: string): Promise<number> {
  return withLinuxLaunchLock(async () => {
    if (focusExistingLinuxWindow()) {
      return 0;
    }

    if (canInspectHyprlandWindows()) {
      await killHeadlessLinuxInstances(appImagePath);

      if (focusExistingLinuxWindow()) {
        return 0;
      }
    }

    return spawnLinuxAppImage(appImagePath);
  });
}

async function withLinuxLaunchLock(
  launch: () => Promise<number>
): Promise<number> {
  const runtimeDir =
    process.env.XDG_RUNTIME_DIR ||
    path.join(os.tmpdir(), `user-${process.getuid?.() ?? 'unknown'}`);
  const lockDir = path.join(runtimeDir, 'vibe-kanban-desktop-launch.lock');
  const pidPath = path.join(lockDir, 'pid');
  let acquired = false;

  for (let attempt = 0; attempt < 2; attempt++) {
    try {
      fs.mkdirSync(lockDir, { mode: 0o700 });
      fs.writeFileSync(pidPath, `${process.pid}\n`);
      acquired = true;
      break;
    } catch (err: unknown) {
      if (!isFileExistsError(err)) {
        break;
      }

      const lockPid = readPid(pidPath);
      if (lockPid && isProcessAlive(lockPid)) {
        focusExistingLinuxWindow();
        return 0;
      }

      try {
        fs.rmSync(lockDir, { recursive: true, force: true });
      } catch {}
    }
  }

  try {
    return await launch();
  } finally {
    if (acquired) {
      try {
        fs.rmSync(lockDir, { recursive: true, force: true });
      } catch {}
    }
  }
}

function isFileExistsError(err: unknown): boolean {
  return (
    typeof err === 'object' &&
    err !== null &&
    'code' in err &&
    (err as NodeJS.ErrnoException).code === 'EEXIST'
  );
}

function readPid(pidPath: string): number | null {
  try {
    const pid = Number.parseInt(
      fs.readFileSync(pidPath, 'utf-8').trim(),
      10
    );
    return Number.isFinite(pid) ? pid : null;
  } catch {
    return null;
  }
}

function isProcessAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function focusExistingLinuxWindow(): boolean {
  const clients = getHyprlandClients();
  if (!clients) return false;

  const client = clients.find(isVisibleVibeKanbanClient);
  if (!client?.address) return false;

  try {
    execFileSync('hyprctl', [
      'dispatch',
      'focuswindow',
      `address:${client.address}`,
    ]);
    return true;
  } catch {
    return false;
  }
}

function getHyprlandClients(): HyprlandClient[] | null {
  if (!process.env.HYPRLAND_INSTANCE_SIGNATURE) {
    return null;
  }

  try {
    const output = execFileSync('hyprctl', ['clients', '-j'], {
      encoding: 'utf-8',
      stdio: ['ignore', 'pipe', 'ignore'],
    });
    const clients = JSON.parse(output) as unknown;
    return Array.isArray(clients) ? (clients as HyprlandClient[]) : null;
  } catch {
    return null;
  }
}

function canInspectHyprlandWindows(): boolean {
  return getHyprlandClients() !== null;
}

function isVisibleVibeKanbanClient(client: HyprlandClient): boolean {
  if (client.mapped === false || client.hidden === true) {
    return false;
  }

  return [client.title, client.class, client.initialClass].some((value) =>
    /vibe[- ]kanban/i.test(value || '')
  );
}

async function killHeadlessLinuxInstances(
  appImagePath: string
): Promise<void> {
  const pids = getVibeKanbanLinuxPids(appImagePath);
  if (pids.length === 0) return;

  for (const pid of pids) {
    try {
      process.kill(pid, 'SIGTERM');
    } catch {}
  }

  for (let attempt = 0; attempt < 15; attempt++) {
    if (pids.every((pid) => !isProcessAlive(pid))) {
      return;
    }
    await sleep(100);
  }

  for (const pid of pids) {
    if (!isProcessAlive(pid)) continue;
    try {
      process.kill(pid, 'SIGKILL');
    } catch {}
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function getVibeKanbanLinuxPids(appImagePath: string): number[] {
  let output: string;
  try {
    output = execFileSync('ps', ['-eo', 'pid=,args='], {
      encoding: 'utf-8',
      stdio: ['ignore', 'pipe', 'ignore'],
    });
  } catch {
    return [];
  }

  const appImageName = path.basename(appImagePath);
  return output
    .split('\n')
    .map((line) => {
      const match = line.match(/^\s*(\d+)\s+(.*)$/);
      if (!match) return null;
      return { pid: Number.parseInt(match[1], 10), args: match[2] };
    })
    .filter(
      (processInfo): processInfo is { pid: number; args: string } =>
        !!processInfo &&
        processInfo.pid !== process.pid &&
        Number.isFinite(processInfo.pid) &&
        isVibeKanbanLinuxProcess(
          processInfo.args,
          appImagePath,
          appImageName
        )
    )
    .map((processInfo) => processInfo.pid);
}

function isVibeKanbanLinuxProcess(
  args: string,
  appImagePath: string,
  appImageName: string
): boolean {
  return (
    args.includes('vibe-kanban-tauri') ||
    args.includes(appImagePath) ||
    args.includes(appImageName)
  );
}

function spawnLinuxAppImage(appImagePath: string): Promise<number> {
  const appImage = path.basename(appImagePath);
  console.error(`Launching ${appImage}...`);
  const proc = spawn(appImagePath, [], {
    stdio: 'ignore',
    detached: true,
    env: getLinuxDesktopEnv(),
  });
  proc.unref();
  return Promise.resolve(0);
}

function getLinuxDesktopEnv(): NodeJS.ProcessEnv {
  return {
    ...process.env,
    XDG_SESSION_TYPE: process.env.XDG_SESSION_TYPE || 'wayland',
    XDG_CURRENT_DESKTOP:
      process.env.XDG_CURRENT_DESKTOP ||
      (process.env.HYPRLAND_INSTANCE_SIGNATURE ? 'Hyprland' : undefined),
    GDK_BACKEND: process.env.GDK_BACKEND || 'wayland,x11',
    QT_QPA_PLATFORM: process.env.QT_QPA_PLATFORM || 'wayland;xcb',
  };
}

// Windows: run NSIS setup.exe silently, then launch installed app
async function installAndLaunchWindows(
  bundleInfo: DesktopBundleInfo
): Promise<number> {
  const { dir } = bundleInfo;

  const sentinel = readSentinel(dir);
  if (sentinel?.appPath) {
    const appExe = path.join(sentinel.appPath, 'Vibe Kanban.exe');
    if (fs.existsSync(appExe)) {
      return launchWindowsApp(appExe);
    }
  }

  // Find the NSIS installer
  const files = fs.readdirSync(dir);
  const installer = files.find(
    (f) =>
      f.endsWith('-setup.exe') ||
      (f.endsWith('.exe') && f !== '.installed')
  );
  if (!installer) {
    throw new Error(`No installer found in ${dir}`);
  }

  const installerPath = path.join(dir, installer);
  const installDir = path.join(dir, 'app');

  console.error('Installing Vibe Kanban...');
  try {
    // NSIS supports /S for silent install and /D= for install directory
    execSync(`"${installerPath}" /S /D="${installDir}"`, {
      stdio: 'inherit',
      timeout: 120000,
    });
  } catch {
    // If silent install fails (e.g. UAC denied), try interactive
    console.error(
      'Silent install failed, launching interactive installer...'
    );
    execSync(`"${installerPath}"`, { stdio: 'inherit' });
    // For interactive install, the default location is used
    const defaultDir = path.join(
      process.env.LOCALAPPDATA || '',
      'vibe-kanban'
    );
    if (fs.existsSync(path.join(defaultDir, 'Vibe Kanban.exe'))) {
      writeSentinel(dir, {
        type: 'nsis-exe',
        appPath: defaultDir,
      });
      return launchWindowsApp(
        path.join(defaultDir, 'Vibe Kanban.exe')
      );
    }
    console.error(
      'Installation complete. Please launch Vibe Kanban from your Start menu.'
    );
    return 0;
  }

  writeSentinel(dir, { type: 'nsis-exe', appPath: installDir });

  const appExe = path.join(installDir, 'Vibe Kanban.exe');
  if (fs.existsSync(appExe)) {
    return launchWindowsApp(appExe);
  }

  console.error(
    'Installation complete. Please launch Vibe Kanban from your Start menu.'
  );
  return 0;
}

function launchWindowsApp(appExe: string): number {
  console.error('Launching Vibe Kanban...');
  spawn(appExe, [], { detached: true, stdio: 'ignore' }).unref();
  return 0;
}

export async function installAndLaunch(
  bundleInfo: DesktopBundleInfo,
  osPlatform: NodeJS.Platform
): Promise<number> {
  if (osPlatform === 'darwin') {
    return installAndLaunchMacOS(bundleInfo);
  } else if (osPlatform === 'linux') {
    return installAndLaunchLinux(bundleInfo);
  } else if (osPlatform === 'win32') {
    return installAndLaunchWindows(bundleInfo);
  }
  throw new Error(
    `Desktop app not supported on platform: ${osPlatform}`
  );
}

export function cleanOldDesktopVersions(
  desktopBaseDir: string,
  currentTag: string
): void {
  try {
    const entries = fs.readdirSync(desktopBaseDir, {
      withFileTypes: true,
    });
    for (const entry of entries) {
      if (entry.isDirectory() && entry.name !== currentTag) {
        const oldDir = path.join(desktopBaseDir, entry.name);
        try {
          fs.rmSync(oldDir, { recursive: true, force: true });
        } catch {
          // Ignore errors (e.g. EBUSY on Windows if app is running)
        }
      }
    }
  } catch {
    // Ignore cleanup errors
  }
}
