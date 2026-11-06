#!/usr/bin/env python3
"""Horizon against real Fabric servers, end to end (docs/fabric-servers.md).

Prepares a Fabric server in a scratch folder, starts it (offline mode, 127.0.0.1, a free port in
26170-26199), lets Horizon's headless client (`HORIZON_NET_REPORT`, game/net_harness.rs) join it, chat and
run `/list`, then checks what both sides logged:

- joined, chunks received, the chat line came back, no packet the client couldn't decode, no disconnect;
- Fabric's handshake: the server's own debug log (a log4j config this tool passes, with DEBUG for Fabric
  API's registry sync and networking loggers) shows which registries it synced and that the client
  answered `fabric:registry/sync/complete`;
- one check per Fabric API module Horizon ports (packet splitter, extended menus, recipe sync, custom
  ingredient sync, attachment sync, extended block particles, modded block state ids). Each finds out from
  the server's mod jars and logs whether a mod there exercises it; when none does it says "skipped". A
  module a mod exercises but Horizon doesn't take part in yet is "unsupported" (`--strict` makes it fail).

Profiles (`--profile`, repeatable, run one after the other):

- `vanilla-perfected`: a copy of a prepared server folder (`--server-dir`, default Constantin's Vanilla
  Perfected copy with 41 mods). The source folder is never touched.
- `minimal`: Fabric Loader 0.19.5 + Fabric API 0.162.0+26.3 for 26.3, downloaded once into
  `.local/fabric-e2e/` (Fabric meta API, Modrinth, sha512-checked), a flat world.

It never downloads or runs third-party mods: only Fabric's own loader and Fabric API, or a server folder
you prepared yourself.

    python3 tools/fabric_e2e.py --profile minimal --profile vanilla-perfected
    python3 tools/fabric_e2e.py --profile minimal --bin-dir target/fast --keep

Every process it starts is stopped when it ends (also on Ctrl-C). The game runs with fresh HORIZON_HOME,
launcher and XDG folders and without audio. It prints one JSON summary per run (and a list of them for
several profiles) and exits non-zero when a check fails. Never commit what lands in the cache: game jars.
"""
import argparse
import atexit
import hashlib
import json
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse
import urllib.request
import zipfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import lab  # noqa: E402
from integration_env import game_environment  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
CACHE = ROOT / '.local' / 'fabric-e2e'
MINECRAFT = '26.3'
LOADER = '0.19.5'
INSTALLER = '1.1.2'
FABRIC_API = '0.162.0+26.3'
PORTS = range(26170, 26200)
PLAYER = 'HorizonE2E'
CHAT = 'hello from the fabric e2e'
VANILLA_PERFECTED = Path('/Users/constantinkautz/.claude/jobs/324f7c35/tmp/vpserver')
USER_AGENT = 'horizon-fabric-e2e (github.com/horizon-rust/horizon)'

# What in a mod's classes or data shows it uses a module (constant-pool strings of the Fabric API
# classes it calls). Fabric API's own jar is not scanned for these: it carries every module.
MARKERS = {
    'menu': [b'net/fabricmc/fabric/api/menu/v1/ExtendedMenuType'],
    'recipe': [b'net/fabricmc/fabric/api/recipe/v1/sync/RecipeSynchronization'],
    'ingredient': [b'net/fabricmc/fabric/api/recipe/v1/ingredient/CustomIngredient', b'"fabric:type"'],
    'attachment': [b'net/fabricmc/fabric/api/attachment/v1/AttachmentSyncPredicate'],
    'particles': [b'net/fabricmc/fabric/api/particle/v1/FabricBlockParticleOption'],
    'splitter': [b'registerLarge'],
}

# Fabric API's loggers this tool turns to DEBUG (names from the 0.162 sources): which registries the
# server syncs (RegistrySyncManager), what the client sends in configuration (AbstractChanneledNetworkAddon,
# named after the player), the common packet version.
DEBUG_LOGGERS = ['FabricRegistrySync', 'fabric-networking-api-v1', f'ServerConfigurationNetworkAddon for {PLAYER}',
                 f'ServerLoginNetworkAddon for {PLAYER}', 'fabric-menu-api-v1/server', 'fabric-data-attachment-api-v1']

LOG4J = """<?xml version="1.0" encoding="UTF-8"?>
<Configuration status="WARN">
  <Appenders>
    <Console name="Out" target="SYSTEM_OUT"><PatternLayout pattern="[%d{{HH:mm:ss}}] [%t/%level] [%logger]: %msg{{nolookups}}%n"/></Console>
  </Appenders>
  <Loggers>
{loggers}
    <Root level="info"><AppenderRef ref="Out"/></Root>
  </Loggers>
</Configuration>
"""

# server.properties every profile gets (the rest of a copied folder's file is kept).
PROPERTIES = {
    'online-mode': 'false',
    'white-list': 'false',
    'enforce-whitelist': 'false',
    'server-ip': '127.0.0.1',
    'enable-rcon': 'false',
    'enable-query': 'false',
    'management-server-enabled': 'false',
    'enforce-secure-profile': 'false',
    'pause-when-empty-seconds': '-1',
    'spawn-protection': '0',
}
MINIMAL_PROPERTIES = {
    'level-type': 'minecraft\\:flat',
    'generator-settings': '{"layers":[{"block":"minecraft:bedrock","height":1},{"block":"minecraft:dirt","height":2},'
                          '{"block":"minecraft:grass_block","height":1}],"biome":"minecraft:plains"}',
    'generate-structures': 'false',
    'view-distance': '4',
    'simulation-distance': '4',
    'motd': 'Horizon fabric e2e',
}

# The parts of a Fabric server's log the checks read.
DONE = re.compile(r'\]: Done \([\d.,]+s\)!')
SYNCING = re.compile(r'Syncing registry: (\S+)')
INBOUND = re.compile(r'ServerConfigurationNetworkAddon for (\S+)\]: Handling inbound packet from channel with name "([^"]+)"')
LOGGED_IN = re.compile(r'\]: (\S+)\[/[\d.:]+\] logged in with entity id')
LOST = re.compile(r'\]: (\S+) (?:\(\S+\) )?lost connection: (.*)')
REFUSED = 'This server requires'
# The server stopped (or crashed) before this tool stopped it.
SERVER_DOWN = ['Stopping server', 'Starting shutdown process', 'This crash report has been saved to',
               'Encountered an unexpected exception']


def set_properties(text, updates):
    """server.properties with `updates` applied: existing keys rewritten in place, new ones appended."""
    lines, seen = [], set()
    for line in text.splitlines():
        key = line.split('=', 1)[0].strip()
        if not line.startswith('#') and '=' in line and key in updates:
            lines.append(f'{key}={updates[key]}')
            seen.add(key)
        else:
            lines.append(line)
    lines += [f'{k}={v}' for k, v in updates.items() if k not in seen]
    return '\n'.join(lines) + '\n'


def pick_port(ports, is_free):
    """The first port of `ports` that `is_free` accepts."""
    for port in ports:
        if is_free(port):
            return port
    raise RuntimeError(f'no free port in {ports.start}-{ports.stop - 1}')


def port_is_free(port):
    with socket.socket() as s:
        try:
            s.bind(('127.0.0.1', port))
            return True
        except OSError:
            return False


def log4j_config(loggers):
    rows = '\n'.join(f'    <Logger name="{n}" level="debug" additivity="false"><AppenderRef ref="Out"/></Logger>'
                     for n in loggers)
    return LOG4J.format(loggers=rows)


def scan_jar(data, markers=MARKERS, depth=0):
    """The modules whose markers a mod jar (bytes) contains, with the entries that contain them. Jars nested
    under META-INF/jars are scanned too, except bundled Fabric API modules (`fabric-*.jar`), and so are the
    mod's classes and its data pack's JSON (`data/`), not Fabric's own classes (`net/fabricmc/`)."""
    import io
    found = {}
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        for name in archive.namelist():
            if name.endswith('.jar'):
                if depth < 2 and not name.rsplit('/', 1)[-1].startswith('fabric-'):
                    for module, entries in scan_jar(archive.read(name), markers, depth + 1).items():
                        found.setdefault(module, []).extend(f'{name}!{e}' for e in entries)
                continue
            if name.startswith('net/fabricmc/'):
                continue
            if not (name.endswith('.class') or (name.endswith('.json') and name.startswith('data/'))):
                continue
            body = archive.read(name)
            for module, needles in markers.items():
                if any(n in body for n in needles):
                    found.setdefault(module, []).append(name)
    return found


def scan_mods(mods_dir):
    """{module: [mod jar names]} for the server's mods, Fabric API itself left out."""
    uses = {}
    for jar in sorted(Path(mods_dir).glob('*.jar')):
        if jar.name.startswith('fabric-api-'):
            continue
        try:
            found = scan_jar(jar.read_bytes())
        except zipfile.BadZipFile:
            continue
        for module in found:
            uses.setdefault(module, []).append(jar.name)
    return uses


def parse_server_log(lines, player=PLAYER):
    """What the checks need from a Fabric server's log."""
    out = {'synced': [], 'inbound': [], 'logged_in': False, 'lost': None, 'refused': False, 'chat': [],
           'errors': []}
    for line in lines:
        if m := SYNCING.search(line):
            if m.group(1) not in out['synced']:
                out['synced'].append(m.group(1))
        elif (m := INBOUND.search(line)) and m.group(1) == player:
            out['inbound'].append(m.group(2))
        elif (m := LOGGED_IN.search(line)) and m.group(1) == player:
            out['logged_in'] = True
        elif (m := LOST.search(line)) and m.group(1) == player:
            out['lost'] = m.group(2)
        if REFUSED in line:
            out['refused'] = True
        if f'<{player}>' in line:
            out['chat'].append(line.split(f'<{player}>', 1)[1].strip())
        if '/ERROR]' in line and player in line:
            out['errors'].append(line)
    return out


def decode_errors(client_log):
    """The packets the client skipped as undecodable (remote.rs logs the first 20)."""
    return [l.split('skipped undecodable packet', 1)[1].strip() for l in client_log.splitlines()
            if 'skipped undecodable packet' in l]


def result(name, status, detail=''):
    return {'name': name, 'status': status, 'detail': detail}


# Each module check: (name, the MARKERS key a mod shows it uses, the configuration channel the client sends
# when it takes part (None: no channel of its own), the play packets the module changes). `run_checks`
# decides applicable/skipped from the scan and the registry sync, then pass/fail/unsupported from both logs.
MODULES = [
    ('splitter', 'splitter', None, ['custom_payload']),
    ('menu', 'menu', None, ['open_screen']),
    ('recipe_sync', 'recipe', 'fabric:recipe_sync/supported_serializers', ['custom_payload', 'update_recipes']),
    ('ingredient_sync', 'ingredient', 'fabric:custom_ingredient_sync', ['update_recipes', 'recipe_book_add']),
    ('attachment_sync', 'attachment', 'fabric:accepted_attachments_v1', ['custom_payload']),
    ('particles', 'particles', None, ['level_particles']),
    ('state_ids', None, None, ['level_chunk_with_light', 'block_update', 'section_blocks_update']),
]
# What Horizon says when it refuses a Fabric server's content itself (horizon.multiplayer.fabric_unsupported).
HORIZON_REFUSES = "mods add content Horizon can't play yet"


def run_checks(ctx):
    """The checks' results. `ctx`: report (the client's JSON report or None), client_log, server (from
    parse_server_log), uses (from scan_mods), fabric_api (whether the server runs Fabric API)."""
    report = ctx['report'] or {}
    server = ctx['server']
    errors = decode_errors(ctx['client_log'])
    error = report.get('error') or ('no report' if not ctx['report'] else None)
    checks = []
    joined = report.get('position') is not None and server['logged_in']
    # Horizon declined the server's content (a known gap, named by the namespaces): not a regression.
    gap = not joined and HORIZON_REFUSES in (error or '')
    if joined:
        checks.append(result('joined', 'pass'))
    elif gap:
        checks.append(result('joined', 'unsupported', error))
    else:
        checks.append(result('joined', 'fail', ('the server refused it: ' if server['refused'] else '') + str(error)))
    after = 'skipped' if gap else 'fail'
    chunks = report.get('chunks') or 0
    checks.append(result('chunks', 'pass' if chunks > 0 else after, f'{chunks} chunks'))
    heard = any(CHAT in c for c in report.get('chat') or [])
    logged = CHAT in server['chat']
    # /list's answer goes to the player only: the client shows it.
    listed = any(re.search(r'There are \d+ of a max of \d+ players online', c) for c in report.get('chat') or [])
    checks.append(result('chat_round_trip', 'pass' if heard and logged and listed else after,
                         f'client saw it: {heard}, server logged it: {logged}, /list answered: {listed}'))
    checks.append(result('no_decode_errors', 'fail' if errors else 'pass' if joined else after,
                         '; '.join(errors[:5])))
    lost = server['lost']
    down = server.get('went_down', False)
    clean = error is None and lost in (None, 'Disconnected') and not down
    checks.append(result('no_disconnect', 'pass' if clean and joined else after,
                         f'client: {error}, server: {lost}' + (', the server went down' if down else '')))
    if not ctx['fabric_api']:
        checks.append(result('registry_sync', 'skipped', 'the server runs no Fabric API'))
    elif not server['synced']:
        checks.append(result('registry_sync', 'skipped' if joined else after,
                             'no modded synced registry (or the debug log is off): nothing to sync'))
    else:
        done = 'fabric:registry/sync/complete' in server['inbound']
        status = 'pass' if done and joined else 'unsupported' if gap else 'fail'
        checks.append(result('registry_sync', status,
                             f"synced {', '.join(server['synced'])}; client answered complete: {done}"))
    uses = ctx['uses']
    for name, marker, channel, packets in MODULES:
        if not ctx['fabric_api']:
            checks.append(result(name, 'skipped', 'the server runs no Fabric API'))
            continue
        if name == 'state_ids':
            mods = ['minecraft:block (registry sync)'] if 'minecraft:block' in server['synced'] else []
        elif name == 'particles':
            # Fabric API itself sends block particles with a position (falls, brushes, the warden...).
            mods = uses.get(marker, []) or ['fabric-api']
        else:
            mods = uses.get(marker, [])
        if not mods:
            checks.append(result(name, 'skipped', 'no mod on the server exercises it'))
            continue
        by = ', '.join(mods[:6]) + (f' (+{len(mods) - 6})' if len(mods) > 6 else '')
        broke = [e for e in errors if e.split(':', 1)[0].strip() in packets]
        if broke:
            checks.append(result(name, 'fail', f'used by {by}; ' + '; '.join(broke[:3])))
        elif gap:
            checks.append(result(name, 'unsupported', f'used by {by}; Horizon declined the server'))
        elif not joined:
            checks.append(result(name, 'fail', f'used by {by}; not joined'))
        elif channel and channel not in server['inbound']:
            checks.append(result(name, 'unsupported', f'used by {by}; the client never sent {channel}'))
        else:
            checks.append(result(name, 'pass', f'used by {by}; joined and played with no decode error'
                                 + (f', the client sent {channel}' if channel else '')))
    return checks


def verdict(checks, strict=False):
    bad = {'fail', 'unsupported'} if strict else {'fail'}
    return not any(c['status'] in bad for c in checks)


# Downloads into the cache.

def fetch(url):
    request = urllib.request.Request(url, headers={'User-Agent': USER_AGENT})
    with urllib.request.urlopen(request, timeout=120) as response:
        return response.read()


def cached(name, url, sha512=None):
    """`name` in the cache, downloaded from `url` once (and checked against `sha512`)."""
    path = CACHE / name
    if path.is_file() and (sha512 is None or hashlib.sha512(path.read_bytes()).hexdigest() == sha512):
        return path
    data = fetch(url)
    if sha512 and hashlib.sha512(data).hexdigest() != sha512:
        raise RuntimeError(f'{url}: sha512 mismatch')
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + '.part')
    tmp.write_bytes(data)
    tmp.replace(path)
    return path


def modrinth_file(slug, version_number=None):
    """(file name, url, sha512, required dependency project ids) of a mod's 26.3 Fabric version."""
    query = urllib.parse.urlencode({'game_versions': json.dumps([MINECRAFT]), 'loaders': json.dumps(['fabric'])})
    versions = json.loads(fetch(f'https://api.modrinth.com/v2/project/{slug}/version?{query}'))
    if version_number:
        versions = [v for v in versions if v['version_number'] == version_number]
    if not versions:
        raise RuntimeError(f'Modrinth has no {MINECRAFT} Fabric version of {slug} {version_number or ""}')
    version = versions[0]
    file = next((f for f in version['files'] if f['primary']), version['files'][0])
    deps = [d['project_id'] for d in version['dependencies'] if d['dependency_type'] == 'required' and d['project_id']]
    return file['filename'], file['url'], file['hashes']['sha512'], deps


def mod_jars(slugs):
    """The cached jars of `slugs` and their required dependencies (Fabric API pinned to FABRIC_API)."""
    jars, todo, seen = [], list(slugs), set()
    while todo:
        slug = todo.pop(0)
        if slug in seen:
            continue
        seen.add(slug)
        pinned = FABRIC_API if slug in ('fabric-api', 'P7dR8mSH') else None
        if pinned:
            seen.update({'fabric-api', 'P7dR8mSH'})
        name, url, sha, deps = modrinth_file(slug, pinned)
        jars.append(cached(f'mods/{name}', url, sha))
        todo += deps
    return jars


# The server.

def prepare(profile, scratch, server_dir=None):
    """The server folder for `profile` in `scratch`, ready to start: (folder, port)."""
    folder = scratch / 'server'
    if profile == 'vanilla-perfected':
        source = Path(server_dir or VANILLA_PERFECTED)
        if not (source / 'fabric-server-launch.jar').is_file():
            raise RuntimeError(f'{source} is not a prepared Fabric server folder')
        shutil.copytree(source, folder, symlinks=True, ignore=shutil.ignore_patterns('logs', 'crash-reports',
                                                                                   'session.lock'))
        properties = dict(PROPERTIES)
    else:
        folder.mkdir(parents=True)
        launcher = cached(f'fabric-server-mc.{MINECRAFT}-loader.{LOADER}-launcher.{INSTALLER}.jar',
                          f'https://meta.fabricmc.net/v2/versions/loader/{MINECRAFT}/{LOADER}/{INSTALLER}/server/jar')
        shutil.copy(launcher, folder / 'fabric-server-launch.jar')
        # What the launcher downloads on its first start (the vanilla server, the libraries), kept.
        runtime = CACHE / 'runtime'
        for part in ('server.jar', 'libraries', 'versions', '.fabric'):
            if (runtime / part).is_dir():
                shutil.copytree(runtime / part, folder / part)
            elif (runtime / part).is_file():
                shutil.copy(runtime / part, folder / part)
        (folder / 'mods').mkdir()
        for jar in mod_jars(['fabric-api']):
            shutil.copy(jar, folder / 'mods' / jar.name)
        properties = dict(PROPERTIES, **MINIMAL_PROPERTIES)
    port = pick_port(PORTS, port_is_free)
    path = folder / 'server.properties'
    text = path.read_text(encoding='iso-8859-1') if path.is_file() else ''
    path.write_text(set_properties(text, dict(properties, **{'server-port': str(port)})), encoding='iso-8859-1')
    (folder / 'eula.txt').write_text('eula=true\n')
    (folder / 'console.txt').write_text('')
    (folder / 'log4j2-e2e.xml').write_text(log4j_config(DEBUG_LOGGERS))
    return folder, port


def keep_runtime(folder):
    """After the first start of a downloaded server: keep what its launcher fetched for the next run."""
    runtime = CACHE / 'runtime'
    if (runtime / 'libraries').is_dir():
        return
    runtime.mkdir(parents=True, exist_ok=True)
    for part in ('libraries', 'versions', '.fabric'):
        if (folder / part).is_dir():
            shutil.copytree(folder / part, runtime / part, dirs_exist_ok=True)
    if (folder / 'server.jar').is_file():
        shutil.copy(folder / 'server.jar', runtime / 'server.jar')


class FabricServer:
    """A Fabric server with its console fed from console.txt through `tail -f` (so it never sees stdin's
    end); java and tail are two processes, each in its own process group, both ended by `stop`."""

    def __init__(self, folder, java, heap):
        self.folder = folder
        self.lines = []
        self.log = (folder.parent / 'server.log').open('w', encoding='utf-8')
        self.tail = subprocess.Popen(['tail', '-n', '+1', '-f', 'console.txt'], cwd=folder, stdout=subprocess.PIPE,
                                     start_new_session=True)
        self.process = subprocess.Popen([java, f'-Xmx{heap}', '-Dlog4j2.configurationFile=log4j2-e2e.xml',
                                         '-jar', 'fabric-server-launch.jar', 'nogui'], cwd=folder,
                                        stdin=self.tail.stdout, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                        text=True, encoding='utf-8', errors='replace', start_new_session=True)
        self.tail.stdout.close()
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()

    def _read(self):
        for line in self.process.stdout:
            self.lines.append(line.rstrip('\n'))
            self.log.write(line)
            self.log.flush()

    def console(self, command):
        with (self.folder / 'console.txt').open('a') as console:
            console.write(command + '\n')

    def wait_for(self, regex, timeout, start=0):
        deadline = time.time() + timeout
        while time.time() < deadline:
            if any(regex.search(l) for l in self.lines[start:]):
                return True
            if self.process.poll() is not None:
                time.sleep(0.5)
                raise RuntimeError('the server exited:\n' + '\n'.join(self.lines[-30:]))
            time.sleep(0.2)
        return False

    def stop(self, timeout=120):
        """`stop` on the console; SIGTERM, then SIGKILL, when java hasn't exited in time."""
        if self.process.poll() is None:
            self.console('stop')
            for sig, wait in ((None, timeout), (signal.SIGTERM, 30), (signal.SIGKILL, 10)):
                if sig is not None:
                    try:
                        os.killpg(self.process.pid, sig)
                    except ProcessLookupError:
                        pass
                try:
                    self.process.wait(timeout=wait)
                    break
                except subprocess.TimeoutExpired:
                    continue
        _kill(self.tail)
        self.tail.wait(timeout=10)
        self.reader.join(timeout=5)
        self.log.close()


_RUNNING = []


def _stop_all():
    while _RUNNING:
        thing = _RUNNING.pop()
        try:
            thing.stop() if isinstance(thing, FabricServer) else _kill(thing)
        except Exception as e:  # noqa: BLE001 (cleanup keeps going)
            print(f'cleanup: {e}', file=sys.stderr)


def _kill(process):
    if process.poll() is None:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass


atexit.register(_stop_all)


def join(binary, port, out):
    """The headless client joins, chats, runs /list and leaves: (report or None, its log, seconds)."""
    root = Path(tempfile.mkdtemp(prefix='client-', dir=out))
    report = out / 'client-report.json'
    env = game_environment(os.environ, root)
    env.update(HORIZON_NET_REPORT=str(report), HORIZON_NET_CHAT=f'{CHAT};/list', RUST_LOG='info')
    start = time.time()
    with (out / 'client.log').open('w') as log:
        process = subprocess.Popen([str(binary), '--resources', str(lab.RESOURCES), '--render-distance', '2',
                                    '--server', f'127.0.0.1:{port}', '--name', PLAYER],
                                   cwd=root, env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        _RUNNING.append(process)
        try:
            process.wait(timeout=300)
        except subprocess.TimeoutExpired:
            _kill(process)
        _RUNNING.remove(process)
    text = (out / 'client.log').read_text(errors='replace')
    for extra in sorted(root.rglob('*.log')):
        text += '\n' + extra.read_text(errors='replace')
    data = json.loads(report.read_text()) if report.is_file() else None
    return data, text, round(time.time() - start, 1)


def run_profile(profile, args, binary):
    scratch = Path(tempfile.mkdtemp(prefix=f'fabric-e2e-{profile}-', dir=args.scratch))
    summary = {'profile': profile, 'scratch': str(scratch)}
    server = None
    folder = scratch / 'server'
    try:
        folder, port = prepare(profile, scratch, args.server_dir)
        mods = sorted(p.name for p in (folder / 'mods').glob('*.jar'))
        summary['server'] = {'port': port, 'mods': len(mods), 'fabric_api': [m for m in mods if
                                                                            m.startswith('fabric-api-')]}
        uses = scan_mods(folder / 'mods')
        started = time.time()
        server = FabricServer(folder, args.java, args.heap)
        _RUNNING.append(server)
        if not server.wait_for(DONE, args.start_timeout):
            raise RuntimeError(f'no "Done" in {args.start_timeout} s:\n' + '\n'.join(server.lines[-30:]))
        summary['server']['ready_seconds'] = round(time.time() - started, 1)
        if profile != 'vanilla-perfected':
            keep_runtime(folder)
        report, client_log, seconds = join(binary, port, scratch)
        server.wait_for(re.compile(rf'{PLAYER} (?:\(\S+\) )?lost connection'), 30)
        parsed = parse_server_log(server.lines)
        parsed['went_down'] = server.process.poll() is not None or any(m in l for l in server.lines
                                                                        for m in SERVER_DOWN)
        summary['server'].update(synced=parsed['synced'], client_sent=sorted(set(parsed['inbound'])),
                                 lost=parsed['lost'])
        summary['client'] = {'seconds': seconds, 'report': report, 'decode_errors': decode_errors(client_log)}
        summary['module_mods'] = uses
        summary['checks'] = run_checks({'report': report, 'client_log': client_log, 'server': parsed,
                                        'uses': uses, 'fabric_api': bool(summary['server']['fabric_api'])})
    except Exception as e:  # noqa: BLE001 (the summary says what broke)
        summary['error'] = f'{type(e).__name__}: {e}'
        summary.setdefault('checks', []).append(result('setup', 'fail', summary['error']))
    finally:
        if server is not None:
            server.stop()
            if server in _RUNNING:
                _RUNNING.remove(server)
            crashes = folder / 'crash-reports'
            if crashes.is_dir() and any(crashes.iterdir()):
                shutil.copytree(crashes, scratch / 'crash-reports', dirs_exist_ok=True)
                summary['server']['crash_reports'] = sorted(p.name for p in crashes.iterdir())
        if not args.keep:
            shutil.rmtree(scratch / 'server', ignore_errors=True)
            for client_home in scratch.glob('client-*'):
                shutil.rmtree(client_home, ignore_errors=True)
    summary['ok'] = verdict(summary['checks'], args.strict)
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--profile', action='append', choices=['vanilla-perfected', 'minimal'],
                        help='repeatable (default: minimal)')
    parser.add_argument('--server-dir', help=f'the folder vanilla-perfected copies (default {VANILLA_PERFECTED})')
    parser.add_argument('--bin-dir', help='a prebuilt horizon-client (default: build it with the fast profile)')
    parser.add_argument('--scratch', default=None, help='where runs go (default: a new temp folder)')
    parser.add_argument('--java', default='java')
    parser.add_argument('--heap', default='4G')
    parser.add_argument('--start-timeout', type=int, default=600)
    parser.add_argument('--strict', action='store_true', help='a module a mod uses but Horizon lacks fails')
    parser.add_argument('--keep', action='store_true', help='keep the server folders and client homes')
    args = parser.parse_args()
    # A signal ends the run through sys.exit, so atexit stops what it started.
    for number in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(number, lambda *_: sys.exit(130))
    args.scratch = args.scratch or tempfile.mkdtemp(prefix='fabric-e2e-')
    Path(args.scratch).mkdir(parents=True, exist_ok=True)
    if args.bin_dir:
        binary = Path(args.bin_dir) / 'horizon-client'
    else:
        subprocess.run(['cargo', 'build', '--profile', 'fast', '-p', 'horizon-client', '--bin', 'horizon-client'],
                       cwd=ROOT, check=True)
        binary = Path(lab.cargo_target()) / 'fast' / 'horizon-client'
    summaries = [run_profile(p, args, binary) for p in (args.profile or ['minimal'])]
    for s in summaries:
        for c in s['checks']:
            print(f"{s['profile']:>18} {c['status'].upper():<11} {c['name']}: {c['detail']}", file=sys.stderr)
    print(json.dumps(summaries[0] if len(summaries) == 1 else summaries, indent=2))
    sys.exit(0 if all(s['ok'] for s in summaries) else 1)


if __name__ == '__main__':
    main()
