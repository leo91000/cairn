"""Keep real Moto version/purge semantics while bounding native fixture RAM."""
import copy
import hashlib
import os
from pathlib import Path
import resource
import subprocess
import sys
import textwrap
import unittest
from unittest.mock import patch

import boto3
from moto import mock_aws
from moto.s3.models import s3_backends
import node_s3_test as fixture


class NativeS3MemoryTests(unittest.TestCase):
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


if __name__ == '__main__':
    unittest.main()
