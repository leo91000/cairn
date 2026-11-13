"""Keep real Moto version/purge semantics while bounding native fixture RAM."""
import copy
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import resource
import select
import sqlite3
import stat
import subprocess
import sys
import tempfile
import textwrap
from threading import Event
from types import SimpleNamespace
import unittest
import uuid
import time
from unittest.mock import Mock, patch

import boto3
from botocore.config import Config
from moto import mock_aws
from moto.s3.models import s3_backends
import node_s3_test as fixture
import native_runtime_test as runtime


class NativeRuntimeEntrypointTests(unittest.TestCase):
    def test_disposable_entrypoint_reaches_mocked_prerequisites_before_host_operations(self):
        class PrerequisitesReached(Exception):
            pass

        prerequisites = Mock(side_effect=PrerequisitesReached)
        # Give main only this mocked host method. No installer, service or
        # mapping operation can run even if the expected early stop regresses.
        host_stub = SimpleNamespace(require_docker_host=prerequisites)
        temporary_directory = tempfile.TemporaryDirectory
        environment = dict(CAIRN_NATIVE_DISPOSABLE_HOST='1',
                           CAIRN_NODE_TEST_S3_ENDPOINT='http://127.0.0.1:1',
                           MOTO_S3_DEFAULT_KEY_BUFFER_SIZE='65536')
        with temporary_directory(prefix='native-entrypoint-test-') as root:
            def isolated_directory(**options):
                return temporary_directory(**dict(options, dir=root))

            with patch.dict(os.environ, environment), \
                    patch.object(runtime, 'host', host_stub), \
                    patch.object(runtime.os, 'geteuid', return_value=0), \
                    patch.object(sys, 'argv', ['native_runtime_test.py', sys.executable, sys.executable]), \
                    patch.object(runtime.tempfile, 'TemporaryDirectory', side_effect=isolated_directory), \
                    patch.object(runtime, 'command', side_effect=AssertionError('Unexpected host command.')), \
                    patch('builtins.print'):
                with self.assertRaises(PrerequisitesReached):
                    runtime.main()
        prerequisites.assert_called_once_with()


class NativeS3MemoryTests(unittest.TestCase):
    def test_fixture_purge_preserves_old_remote_objects_and_queued_tombstones(self):
        binary = Path(os.environ.get('CAIRN_TEST_NATIVE_QUALIFIER',
                      str(Path(__file__).resolve().parents[2] / 'target/debug/examples/native_storage_qualifier')))
        self.assertTrue(binary.is_file(), 'Build/download the candidate manager fixture first.')
        binary.chmod(binary.stat().st_mode | 0o100)
        server = fixture.s3_server(native=True)
        host, port = server.get_host_and_port()
        endpoint = f'http://{host}:{port}'
        client = boto3.client('s3', endpoint_url=endpoint, region_name='us-east-1',
                              aws_access_key_id='node-fixture', aws_secret_access_key='node-fixture-secret')
        client.create_bucket(Bucket='cairn-node-test')
        helper = None
        try:
            with tempfile.TemporaryDirectory(prefix='native-gc-clock-') as directory:
                root = Path(directory)
                (root / 'disposable-fixture').touch()
                (root / 'data').mkdir()
                (root / 'data/runner-secret').write_text('synthetic-fixture-secret')
                # A minimal child environment cannot inherit a coding agent's
                # live authentication broker or cloud credentials.
                environment = dict(PATH=str(Path(sys.executable).parent) + os.pathsep + os.environ['PATH'],
                                   CAIRN_NATIVE_DISPOSABLE_HOST='1', CAIRN_NODE_TEST_S3_ENDPOINT=endpoint,
                                   AWS_ACCESS_KEY_ID='node-fixture', AWS_SECRET_ACCESS_KEY='node-fixture-secret',
                                   AWS_REGION='us-east-1', AWS_ENDPOINT_URL_S3=endpoint, AWS_EC2_METADATA_DISABLED='true',
                                   AWS_CONFIG_FILE='/dev/null', AWS_SHARED_CREDENTIALS_FILE='/dev/null')
                helper = subprocess.Popen([str(binary), str(root), 'http://127.0.0.1:9'], env=environment,
                                          stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)

                def manager(op, **values):
                    helper.stdin.write(json.dumps(dict(op=op, **values)) + '\n')
                    helper.stdin.flush()
                    self.assertTrue(select.select([helper.stdout], [], [], 20)[0], 'Manager fixture timed out.')
                    response = json.loads(helper.stdout.readline())
                    self.assertTrue(response['ok'], response)

                manager('metrics')
                object_id, run = str(uuid.uuid4()), str(uuid.uuid4())
                block_hash = 'b3-' + 'a' * 64
                key = f'shared-blocks/v3/{block_hash}/{object_id}'
                client.put_object(Bucket='cairn-node-test', Key=key, Body=b'garbage')
                database = root / 'manager/manager.db'
                with sqlite3.connect(database) as db:
                    location = json.dumps(dict(destination='s3', bucket='cairn-node-test', endpoint=None, blockEnvelope=3))
                    db.execute("INSERT INTO shared_objects VALUES(?,?,?,?,?,'deleting',0,?)",
                               (object_id, 'fixture', block_hash, 4096, location, int(time.time() * 1000) + 3600000))
                    db.execute('INSERT INTO direct_storage_grants VALUES(?,0,0)', (object_id,))
                manager('purge', run=run)
                self.assertEqual(client.list_objects_v2(Bucket='cairn-node-test')['KeyCount'], 1,
                                 'Local cleanup must preserve old remote objects.')
                before = runtime.advance_fixture_garbage_clock(root)
                self.assertEqual(before['states'][0]['deferred'], 1)
                manager('purge', run=run)
                self.assertEqual(client.list_objects_v2(Bucket='cairn-node-test')['KeyCount'], 1)
                with sqlite3.connect(database) as db:
                    state, retry, settled = db.execute('''SELECT o.state,o.retry_at,g.settled FROM shared_objects o
                        JOIN direct_storage_grants g ON g.object=o.id WHERE o.id=?''', (object_id,)).fetchone()
                    self.assertEqual((state, settled), ('deleting', 0))
                    self.assertEqual(retry, 0)
        finally:
            if helper:
                helper.stdin.close()
                try:
                    helper.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    helper.kill()
                    helper.wait()
                helper.stdout.close()
                helper.stderr.close()
            server.stop()

    def test_version_listing_survives_a_concurrent_real_delete(self):
        server = fixture.s3_server(native=True)
        host, port = server.get_host_and_port()
        client = boto3.client('s3', endpoint_url=f'http://{host}:{port}', region_name='us-east-1',
                              aws_access_key_id='node-fixture', aws_secret_access_key='node-fixture-secret',
                              config=Config(retries={'max_attempts': 0}, read_timeout=10, connect_timeout=2))
        bucket, key = 'native-concurrent-gc', 'shared-blocks/retired'
        snapshot, resume, deleting, deleted = Event(), Event(), Event(), Event()
        try:
            client.create_bucket(Bucket=bucket)
            client.put_object(Bucket=bucket, Key=key, Body=b'payload' * 16384)
            versions = s3_backends['123456789012']['aws'].get_bucket(bucket).keys
            original_snapshot = versions._self_iterable

            def held_snapshot():
                copied = original_snapshot()
                snapshot.set()
                if not resume.wait(5):
                    raise AssertionError('GC snapshot fixture was not released.')
                return copied

            original_application = server._server.app

            def observe_delete(environment, start_response):
                if environment['REQUEST_METHOD'] == 'DELETE':
                    deleting.set()
                response = original_application(environment, start_response)
                if environment['REQUEST_METHOD'] == 'DELETE':
                    deleted.set()
                return response

            server._server.app = observe_delete
            with ThreadPoolExecutor(max_workers=2) as workers, \
                    patch.object(versions, '_self_iterable', side_effect=held_snapshot):
                listing = workers.submit(client.list_object_versions, Bucket=bucket)
                try:
                    self.assertTrue(snapshot.wait(5), 'Version listing did not reach its snapshot.')
                    deletion = workers.submit(client.delete_object, Bucket=bucket, Key=key)
                    self.assertTrue(deleting.wait(5), 'Concurrent delete did not reach the server.')
                    # Give unprotected Moto time to delete between its key-name
                    # snapshot and version lookup. The fixed server holds it.
                    deleted.wait(1)
                finally:
                    resume.set()
                listed = listing.result(timeout=10)
                deletion.result(timeout=10)
            self.assertEqual([item['Key'] for item in listed['Versions']], [key])
            self.assertEqual(client.list_objects_v2(Bucket=bucket)['KeyCount'], 0)
        finally:
            resume.set()
            server.stop()

    def test_insufficient_hard_limit_fails_before_starting_server(self):
        with patch.object(resource, 'getrlimit', return_value=(128, 1024)), \
                patch.object(resource, 'setrlimit') as change:
            with self.assertRaisesRegex(RuntimeError, 'hard limit of at least 65536'):
                fixture.configure_native_s3()
            change.assert_not_called()

    def test_sufficient_soft_limit_does_not_change_inherited_limits(self):
        for limits in [(131072, 131072), (resource.RLIM_INFINITY, resource.RLIM_INFINITY)]:
            with self.subTest(limits=limits), patch.object(resource, 'getrlimit', return_value=limits), \
                    patch.object(resource, 'setrlimit') as change:
                fixture.configure_native_s3()
                change.assert_not_called()

    def test_real_spooled_version_copies_exceed_initial_soft_limit(self):
        # Alter only a disposable child process, never this runner's limits.
        program = textwrap.dedent('''
            import os
            import resource
            import boto3
            from moto import mock_aws
            from moto.s3.models import s3_backends
            import node_s3_test as fixture

            soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
            resource.setrlimit(resource.RLIMIT_NOFILE, (128, hard))
            fixture.configure_native_s3()
            assert resource.getrlimit(resource.RLIMIT_NOFILE) == (65536, hard)
            with mock_aws():
                client = boto3.client('s3', region_name='us-east-1',
                                      aws_access_key_id='fixture', aws_secret_access_key='fixture')
                client.create_bucket(Bucket='native-descriptor-test')
                client.put_bucket_versioning(Bucket='native-descriptor-test',
                                             VersioningConfiguration={'Status': 'Enabled'})
                for index in range(48):
                    client.put_object(Bucket='native-descriptor-test', Key=str(index), Body=b'x' * 131072)
                backend = s3_backends['123456789012']['aws']
                copies = [backend.list_object_versions('native-descriptor-test')[0] for _ in range(4)]
                assert len(os.listdir('/proc/self/fd')) > 128
                for keys in copies:
                    assert len(keys) == 48
                    for key in keys:
                        assert key._value_buffer._rolled
                        assert key.value == b'x' * 131072
                        key.dispose()
                entries = client.list_object_versions(Bucket='native-descriptor-test')['Versions']
                client.delete_objects(Bucket='native-descriptor-test', Delete={'Objects': [
                    dict(Key=item['Key'], VersionId=item['VersionId']) for item in entries]})
                assert client.list_objects_v2(Bucket='native-descriptor-test')['KeyCount'] == 0
        ''')
        environment = dict(os.environ, AWS_EC2_METADATA_DISABLED='true', AWS_CONFIG_FILE='/dev/null',
                           AWS_SHARED_CREDENTIALS_FILE='/dev/null')
        subprocess.run([sys.executable, '-c', program], cwd=Path(__file__).parent,
                       env=environment, check=True, timeout=60)

    def test_large_version_payloads_and_listing_copies_spill_without_changing_data(self):
        environment = dict(AWS_EC2_METADATA_DISABLED='true', AWS_CONFIG_FILE='/dev/null',
                           AWS_SHARED_CREDENTIALS_FILE='/dev/null')
        with patch.dict(os.environ, environment, clear=True), mock_aws():
            fixture.configure_native_s3()
            client = boto3.client('s3', region_name='us-east-1',
                                  aws_access_key_id='node-fixture', aws_secret_access_key='node-fixture-secret')
            bucket, name = 'native-memory-test', 'layers/payload'
            client.create_bucket(Bucket=bucket)
            client.put_bucket_versioning(Bucket=bucket, VersioningConfiguration={'Status': 'Enabled'})
            payloads = [bytes(range(256)) * 1024, b'other' * 65536]
            versions = {}
            for payload in payloads:
                result = client.put_object(Bucket=bucket, Key=name, Body=payload)
                versions[result['VersionId']] = payload
            marker = client.delete_object(Bucket=bucket, Key=name)['VersionId']
            backend = s3_backends['123456789012']['aws']
            listed, _, markers, _, _ = backend.list_object_versions(bucket, prefix='layers/')
            self.assertEqual({key.version_id for key in listed}, set(versions))
            self.assertEqual([key.version_id for key in markers], [marker])
            for key in listed:
                self.assertTrue(key._value_buffer._rolled, 'Version-list payload retained in emulator RAM.')
                cloned = copy.deepcopy(key)
                try:
                    self.assertTrue(cloned._value_buffer._rolled, 'GC listing copy retained in RAM.')
                    self.assertEqual(cloned.value, versions[key.version_id])
                finally:
                    cloned.dispose()
                response = client.get_object(Bucket=bucket, Key=name, VersionId=key.version_id)
                self.assertEqual(response['Body'].read(), versions[key.version_id])
                self.assertEqual(response['ETag'].strip('"'), hashlib.md5(versions[key.version_id]).hexdigest())
            # Exercise the public paginated listing and actual version deletion;
            # the workaround must not skip versions, markers or purge calls.
            pages = client.get_paginator('list_object_versions').paginate(Bucket=bucket, Prefix='layers/', MaxKeys=1)
            entries = [dict(Key=item['Key'], VersionId=item['VersionId'])
                       for page in pages for item in page.get('Versions', []) + page.get('DeleteMarkers', [])]
            self.assertEqual({item['VersionId'] for item in entries}, set(versions) | {marker})
            client.delete_objects(Bucket=bucket, Delete={'Objects': entries})
            self.assertFalse(client.list_object_versions(Bucket=bucket).get('Versions'))
            self.assertEqual(client.list_objects_v2(Bucket=bucket)['KeyCount'], 0)


def allocated_spools(directory):
    files = {}
    for descriptor in Path('/proc/self/fd').iterdir():
        try:
            if not os.readlink(descriptor).startswith(directory + os.sep):
                continue
            metadata = descriptor.stat()
            if stat.S_ISREG(metadata.st_mode):
                files[metadata.st_dev, metadata.st_ino] = metadata.st_blocks * 512
        except FileNotFoundError:
            continue
    return sum(files.values())


class NativeS3DiskTests(unittest.TestCase):
    def test_paginated_version_snapshots_release_disk_after_each_materialized_response(self):
        fixture.configure_native_s3()
        with tempfile.TemporaryDirectory(prefix='native-spool-bound-') as directory, \
                patch.object(tempfile, 'tempdir', directory):
            server = fixture.s3_server(native=True)
            application = server._server.app.application
            materializing = []

            def measured(environment, start_response):
                response = application(environment, start_response)
                materializing.append(allocated_spools(directory))
                try:
                    for chunk in response:
                        materializing.append(allocated_spools(directory))
                        yield chunk
                finally:
                    if hasattr(response, 'close'):
                        response.close()

            server._server.app.application = measured
            host, port = server.get_host_and_port()
            client = boto3.client('s3', endpoint_url=f'http://{host}:{port}', region_name='us-east-1',
                                  aws_access_key_id='fixture', aws_secret_access_key='fixture',
                                  config=Config(retries={'max_attempts': 0}))
            try:
                bucket = 'native-spool-bound'
                client.create_bucket(Bucket=bucket)
                client.put_bucket_versioning(Bucket=bucket, VersioningConfiguration={'Status': 'Enabled'})
                versions = {}
                for key in range(8):
                    for version in range(3):
                        name = f'blocks/{key:02}'
                        payload = bytes([key, version]) * 65536
                        response = client.put_object(Bucket=bucket, Key=name, Body=payload)
                        versions[name, response['VersionId']] = payload
                marker = client.delete_object(Bucket=bucket, Key='blocks/00')['VersionId']
                baseline = allocated_spools(directory)
                self.assertGreater(baseline, 0, 'The real version payloads must spill to disk.')
                materializing.clear()
                completed = []
                for _ in range(4):
                    listed, markers = [], []
                    pages = client.get_paginator('list_object_versions').paginate(Bucket=bucket, Prefix='blocks/', MaxKeys=2)
                    for page in pages:
                        listed.extend((entry['Key'], entry['VersionId']) for entry in page.get('Versions', []))
                        markers.extend(entry['VersionId'] for entry in page.get('DeleteMarkers', []))
                        completed.append(allocated_spools(directory))
                    self.assertEqual(set(listed), set(versions))
                    self.assertEqual(markers, [marker])
                peak = max(materializing)
                retained = max(completed)
                for (name, version), payload in versions.items():
                    response = client.get_object(Bucket=bucket, Key=name, VersionId=version)
                    self.assertEqual(response['Body'].read(), payload)
                client.delete_objects(Bucket=bucket, Delete={'Objects': [
                    dict(Key=name, VersionId=version) for name, version in versions
                ] + [dict(Key='blocks/00', VersionId=marker)]})
                remaining = client.list_object_versions(Bucket=bucket)
                self.assertFalse(remaining.get('Versions'))
                self.assertFalse(remaining.get('DeleteMarkers'))
                self.assertEqual(client.list_objects_v2(Bucket=bucket)['KeyCount'], 0)
                allocation = dict(liveSpoolBytes=baseline, materializingPeakBytes=peak,
                                  completedPeakBytes=retained, afterDeleteBytes=allocated_spools(directory))
                print('Native S3 allocation: ' + json.dumps(allocation))
                self.assertLessEqual(peak, baseline * 3,
                                     f'Listing materialization retained stale payload snapshots: {peak} vs {baseline} bytes.')
                self.assertLessEqual(retained, baseline * 2,
                                     f'Completed listings retained stale payload snapshots: {retained} vs {baseline} bytes.')
                self.assertEqual(allocated_spools(directory), 0, 'Deleted versions retained temporary spool files.')
            finally:
                server.stop()


if __name__ == '__main__':
    unittest.main()
