"""tools/fabric_e2e.py: properties, ports, the jar scan, the Fabric log parser and the checks (no server runs).

The log lines are in the format of the log4j config the tool passes, with messages copied from Fabric API
0.162's sources (RegistrySyncManager, AbstractChanneledNetworkAddon) and vanilla's server log.
"""
import io
import unittest
import zipfile

from tools import fabric_e2e as e2e

P = e2e.PLAYER
SERVER_LOG = [
    '[12:00:01] [Server thread/INFO] [net.minecraft.server.MinecraftServer]: Done (4.211s)! For help, type "help"',
    '[12:00:05] [Netty Server IO #1/DEBUG] [FabricRegistrySync]: Syncing registry: minecraft:attribute',
    '[12:00:05] [Netty Server IO #1/DEBUG] [FabricRegistrySync]: Syncing registry: minecraft:menu',
    '[12:00:05] [Netty Server IO #1/DEBUG] [FabricRegistrySync]: Skipping un-modded registry: minecraft:block',
    f'[12:00:05] [Server thread/DEBUG] [ServerConfigurationNetworkAddon for {P}]: Handling inbound packet from '
    'channel with name "c:version"',
    '[12:00:05] [Server thread/DEBUG] [ServerConfigurationNetworkAddon for Other]: Handling inbound packet from '
    'channel with name "fabric:recipe_sync/supported_serializers"',
    f'[12:00:05] [Server thread/DEBUG] [ServerConfigurationNetworkAddon for {P}]: Handling inbound packet from '
    'channel with name "fabric:registry/sync/complete"',
    f'[12:00:06] [Server thread/INFO] [net.minecraft.server.players.PlayerList]: {P}[/127.0.0.1:50123] logged in '
    'with entity id 20 at (0.5, -60.0, 0.5)',
    f'[12:00:10] [Server thread/INFO] [net.minecraft.server.MinecraftServer]: [Not Secure] <{P}> '
    f'{e2e.CHAT}',
    f'[12:00:12] [Server thread/INFO] [net.minecraft.server.network.ServerGamePacketListenerImpl]: {P} lost '
    'connection: Disconnected',
]
REPORT = {'position': [0.5, -60.0, 0.5], 'chunks': 25, 'chat': [f'<{P}> {e2e.CHAT}', f'There are 1 of a max of 20 players online: {P}'],
          'error': None}


def jar(entries):
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, 'w') as archive:
        for name, body in entries.items():
            archive.writestr(name, body)
    return buf.getvalue()


def statuses(checks):
    return {c['name']: c['status'] for c in checks}


def ctx(**kw):
    base = dict(report=REPORT, client_log='', server=e2e.parse_server_log(SERVER_LOG), uses={}, fabric_api=True)
    base.update(kw)
    return base


class PropertiesAndPorts(unittest.TestCase):
    def test_set_properties_rewrites_in_place_and_appends(self):
        text = '#Minecraft server properties\nonline-mode=true\nmotd=A=B\nserver-port=25565\n'
        got = e2e.set_properties(text, {'online-mode': 'false', 'server-port': '26171', 'white-list': 'false'})
        self.assertEqual(got, '#Minecraft server properties\nonline-mode=false\nmotd=A=B\nserver-port=26171\n'
                              'white-list=false\n')

    def test_comment_lines_are_kept(self):
        self.assertEqual(e2e.set_properties('#online-mode=true\n', {'online-mode': 'false'}),
                         '#online-mode=true\nonline-mode=false\n')

    def test_pick_port_skips_busy_ones(self):
        self.assertEqual(e2e.pick_port(range(26170, 26200), lambda p: p >= 26173), 26173)
        with self.assertRaises(RuntimeError):
            e2e.pick_port(range(26170, 26200), lambda p: False)

    def test_log4j_config_names_each_debug_logger(self):
        xml = e2e.log4j_config(['FabricRegistrySync', f'ServerConfigurationNetworkAddon for {P}'])
        self.assertIn('<Logger name="FabricRegistrySync" level="debug"', xml)
        self.assertIn(f'<Logger name="ServerConfigurationNetworkAddon for {P}" level="debug"', xml)
        self.assertIn('%d{HH:mm:ss}', xml)


class JarScan(unittest.TestCase):
    def test_finds_a_mods_own_use_of_an_api(self):
        data = jar({'com/example/Mod.class': b'\xca\xfe..net/fabricmc/fabric/api/attachment/v1/AttachmentSyncPredicate..',
                    'data/example/recipe/x.json': b'{"ingredient": {"fabric:type": "fabric:all"}}'})
        self.assertEqual(e2e.scan_jar(data), {'attachment': ['com/example/Mod.class'],
                                              'ingredient': ['data/example/recipe/x.json']})

    def test_bundled_fabric_modules_and_fabric_classes_do_not_count(self):
        bundled = jar({'net/fabricmc/fabric/impl/networking/PayloadTypeRegistryImpl.class': b'registerLarge'})
        data = jar({'META-INF/jars/fabric-networking-api-v1-6.3.9.jar': bundled,
                    'net/fabricmc/fabric/api/networking/v1/PayloadTypeRegistry.class': b'registerLarge',
                    'assets/example/models/x.json': b'"fabric:type"'})
        self.assertEqual(e2e.scan_jar(data), {})

    def test_nested_mod_jars_are_scanned(self):
        inner = jar({'lib/Menus.class': b'net/fabricmc/fabric/api/menu/v1/ExtendedMenuType'})
        self.assertEqual(e2e.scan_jar(jar({'META-INF/jars/somelib-1.0.jar': inner})),
                         {'menu': ['META-INF/jars/somelib-1.0.jar!lib/Menus.class']})


class ServerLog(unittest.TestCase):
    def test_parse(self):
        got = e2e.parse_server_log(SERVER_LOG)
        self.assertEqual(got['synced'], ['minecraft:attribute', 'minecraft:menu'])
        # Another player's channels are not ours.
        self.assertEqual(got['inbound'], ['c:version', 'fabric:registry/sync/complete'])
        self.assertTrue(got['logged_in'])
        self.assertEqual(got['lost'], 'Disconnected')
        self.assertEqual(got['chat'], [e2e.CHAT])
        self.assertFalse(got['refused'])

    def test_fabric_refusal(self):
        got = e2e.parse_server_log([
            f'[12:00:05] [Server thread/INFO] [net.minecraft.server.network.ServerConfigurationPacketListenerImpl]: '
            f'{P} (8b0c0e9c-0f52-3b7a-9d5e-12c5e0f5a3b1) lost connection: This server requires Fabric Loader and '
            'Fabric API installed on your client!'])
        self.assertTrue(got['refused'])
        self.assertFalse(got['logged_in'])
        self.assertTrue(got['lost'].startswith('This server requires'))

    def test_done_line(self):
        self.assertTrue(e2e.DONE.search(SERVER_LOG[0]))
        self.assertFalse(e2e.DONE.search('[12:00:01] [Server thread/INFO]: Done preparing level'))

    def test_decode_errors(self):
        log = ('[2026-11-06 12:00:07 WARN] Horizon: skipped undecodable packet level_particles: unknown particle 117\n'
               'other line\n')
        self.assertEqual(e2e.decode_errors(log), ['level_particles: unknown particle 117'])


class Checks(unittest.TestCase):
    def test_a_clean_join_passes_and_unused_modules_are_skipped(self):
        got = statuses(e2e.run_checks(ctx()))
        for name in ('joined', 'chunks', 'chat_round_trip', 'no_decode_errors', 'no_disconnect', 'registry_sync',
                     'particles'):
            self.assertEqual(got[name], 'pass', name)
        for name in ('splitter', 'menu', 'recipe_sync', 'ingredient_sync', 'attachment_sync', 'state_ids'):
            self.assertEqual(got[name], 'skipped', name)
        self.assertTrue(e2e.verdict(e2e.run_checks(ctx()), strict=True))

    def test_a_used_module_without_the_clients_channel_is_unsupported(self):
        checks = e2e.run_checks(ctx(uses={'recipe': ['justenoughbreeding.jar'], 'menu': ['scholar.jar']}))
        got = statuses(checks)
        self.assertEqual(got['recipe_sync'], 'unsupported')
        self.assertEqual(got['menu'], 'pass')
        self.assertTrue(e2e.verdict(checks))
        self.assertFalse(e2e.verdict(checks, strict=True))

    def test_a_decode_error_fails_its_module_and_the_run(self):
        log = 'WARN Horizon: skipped undecodable packet open_screen: unknown menu type 25\n'
        checks = e2e.run_checks(ctx(client_log=log, uses={'menu': ['scholar.jar']}))
        got = statuses(checks)
        self.assertEqual(got['menu'], 'fail')
        self.assertEqual(got['no_decode_errors'], 'fail')
        self.assertEqual(got['particles'], 'pass')
        self.assertFalse(e2e.verdict(checks))

    def test_registry_sync_needs_the_complete_answer(self):
        lines = [l for l in SERVER_LOG if 'sync/complete' not in l]
        got = statuses(e2e.run_checks(ctx(server=e2e.parse_server_log(lines))))
        self.assertEqual(got['registry_sync'], 'fail')

    def test_horizon_declining_modded_blocks_is_a_known_gap(self):
        lines = [l.replace('Skipping un-modded registry', 'Syncing registry') for l in SERVER_LOG[:5]]
        report = {'position': None, 'chunks': 0, 'chat': [],
                  'error': "disconnected: This Fabric server's mods add content Horizon can't play yet: feeding_trough"}
        checks = e2e.run_checks(ctx(report=report, server=e2e.parse_server_log(lines)))
        got = statuses(checks)
        self.assertEqual(got['joined'], 'unsupported')
        self.assertEqual(got['state_ids'], 'unsupported')
        self.assertEqual(got['chunks'], 'skipped')
        self.assertTrue(e2e.verdict(checks))
        self.assertFalse(e2e.verdict(checks, strict=True))

    def test_any_other_failed_join_fails(self):
        report = {'position': None, 'chunks': 0, 'chat': [], 'error': 'disconnected: Connection refused'}
        checks = e2e.run_checks(ctx(report=report, server=e2e.parse_server_log(SERVER_LOG[:1])))
        got = statuses(checks)
        self.assertEqual(got['joined'], 'fail')
        self.assertEqual(got['chunks'], 'fail')
        self.assertFalse(e2e.verdict(checks))

    def test_chat_needs_the_list_answer_too(self):
        report = dict(REPORT, chat=REPORT['chat'][:1])
        self.assertEqual(statuses(e2e.run_checks(ctx(report=report)))['chat_round_trip'], 'fail')

    def test_the_server_going_down_fails(self):
        server = dict(e2e.parse_server_log(SERVER_LOG), went_down=True, lost=None)
        self.assertEqual(statuses(e2e.run_checks(ctx(server=server)))['no_disconnect'], 'fail')

    def test_no_report_fails(self):
        self.assertEqual(statuses(e2e.run_checks(ctx(report=None)))['joined'], 'fail')

    def test_a_kick_fails(self):
        lines = SERVER_LOG[:-1] + [SERVER_LOG[-1].replace('Disconnected', 'Kicked by MinerTrack')]
        got = statuses(e2e.run_checks(ctx(server=e2e.parse_server_log(lines))))
        self.assertEqual(got['no_disconnect'], 'fail')


if __name__ == '__main__':
    unittest.main()
