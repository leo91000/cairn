"""Qualify local disks and retention of legacy remote objects in a synthetic bucket.

Run: uv run --with 'moto[server]==5.2.3' python scripts/tests/node_s3_test.py
Requires the AWS CLI and the project's pinned pnpm/Rust tools.
"""
import argparse
import os
from pathlib import Path
import resource
import subprocess
from threading import Lock

import boto3
from moto.core.model_instances import model_data
from moto.server import ThreadedMotoServer

NATIVE_S3_KEY_BUFFER_BYTES = 64 * 1024
NATIVE_S3_OPEN_FILES = 65536


class SnapshotS3Application:
    """Moto's model operations need a consistent snapshot during version GC."""

    def __init__(self, application):
        self.application = application
        self.lock = Lock()

    def __call__(self, environment, start_response):
        # Moto 5.2.3 snapshots key names, then looks up their mutable version
        # lists. A concurrent DELETE can turn that lookup into None and make
        # ListObjectVersions return 500. Finish model execution and materialize
        # its response atomically; serve those bytes outside the model lock.
        # Native objects are disk blocks (<=4 MiB plus framing), not whole disks.
        with self.lock:
            response = self.application(environment, start_response)
            try:
                return list(response)
            finally:
                try:
                    if hasattr(response, 'close'):
                        response.close()
                finally:
                    # Moto's inspection registries strongly retain copied keys
                    # and delete markers that own payloads from version listings.
                    # Backend dictionaries retain live S3 state; the dashboard
                    # history must not keep completed request spools alive.
                    for model in model_data['s3'].values():
                        model.instances_tracked.clear()


def s3_server(native=False):
    server = ThreadedMotoServer(ip_address='127.0.0.1', port=0, verbose=False)
    server.start()
    if native:
        # Moto exposes no application constructor parameter. This is installed
        # before creating any client, on the pinned fixture server only.
        server._server.app = SnapshotS3Application(server._server.app)
    return server


def configure_native_s3():
    # Moto 5.2.3 deep-copies every bucket payload for each version listing before
    # prefix filtering. Concurrent real GC requests otherwise multiply a Docker
    # store into several GiB of emulator RAM. Use Moto's supported file-spooling
    # setting; preserve the real version-list/delete APIs and every purge check.
    os.environ['MOTO_S3_DEFAULT_KEY_BUFFER_SIZE'] = str(NATIVE_S3_KEY_BUFFER_BYTES)
    # Spilled originals and simultaneous version-list copies each hold a file.
    # Provision this test process before starting threads, within its inherited
    # hard limit. Never change the host configuration or production unit limits.
    soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
    if hard != resource.RLIM_INFINITY and hard < NATIVE_S3_OPEN_FILES:
        raise RuntimeError('Native Moto fixture requires an open-file hard limit of at least 65536.')
    if soft != resource.RLIM_INFINITY and soft < NATIVE_S3_OPEN_FILES:
        resource.setrlimit(resource.RLIMIT_NOFILE, (NATIVE_S3_OPEN_FILES, hard))


def run_qualification(repo, env, arguments, label):
    result = subprocess.run(['pnpm', 'test:backend', *arguments], cwd=repo, env=env,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    print(result.stdout, end='', flush=True)
    result.check_returncode()
    import re
    if not re.search(r'^test result: ok\. [1-9][0-9]* passed; 0 failed;', result.stdout, re.MULTILINE):
        raise RuntimeError(f'No passing qualification selected by {label!r}')


def run_case(repo, env, case):
    run_qualification(repo, env, ['-p', 'cairn-installation', '--test', 'nodes', case,
                                 '--', '--ignored', '--nocapture'], case)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--native-runtime', type=Path)
    parser.add_argument('--qualifier', type=Path)
    parser.add_argument('--benchmark', action='store_true')
    parser.add_argument('--compression-benchmark', action='store_true')
    parser.add_argument('--repo', type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument('--repeat', type=int, default=3)
    parser.add_argument('--case', help='Run one node recovery test against the isolated S3 fixture')
    args = parser.parse_args()
    if args.native_runtime:
        configure_native_s3()
    server = s3_server(native=bool(args.native_runtime))
    try:
        host, port = server.get_host_and_port()
        endpoint = f'http://{host}:{port}'
        client = boto3.client('s3', endpoint_url=endpoint, region_name='us-east-1',
                              aws_access_key_id='node-fixture', aws_secret_access_key='node-fixture-secret')
        client.create_bucket(Bucket='cairn-node-test')
        client.put_public_access_block(Bucket='cairn-node-test', PublicAccessBlockConfiguration={
            'BlockPublicAcls': True, 'IgnorePublicAcls': True, 'BlockPublicPolicy': True, 'RestrictPublicBuckets': True})
        env = {key: value for key, value in os.environ.items()
               if not key.startswith(('AWS_', 'ARCHIVE_S3_', 'STORAGE_S3_'))}
        env['CAIRN_NODE_TEST_S3_ENDPOINT'] = endpoint
        env['AWS_ENDPOINT_URL_S3'] = endpoint
        env['AWS_ACCESS_KEY_ID'] = 'node-fixture'
        env['AWS_SECRET_ACCESS_KEY'] = 'node-fixture-secret'
        env['AWS_REGION'] = 'us-east-1'
        env['AWS_EC2_METADATA_DISABLED'] = 'true'
        env['AWS_CONFIG_FILE'] = '/dev/null'
        env['AWS_SHARED_CREDENTIALS_FILE'] = '/dev/null'
        if args.native_runtime:
            client.put_object(Bucket='cairn-node-test', Key='qualification-preserved-object', Body=b'keep legacy disk objects')
            if not args.qualifier:
                parser.error('--native-runtime requires --qualifier')
            subprocess.run([os.sys.executable, 'scripts/tests/native_runtime_test.py',
                            str(args.native_runtime.resolve()), str(args.qualifier.resolve())],
                           cwd=args.repo, env=env, check=True)
            remaining = client.list_objects_v2(Bucket='cairn-node-test')
            assert remaining.get('KeyCount', 0) == 1, 'Native qualification created or deleted disk objects'
            assert client.get_object(Bucket='cairn-node-test', Key='qualification-preserved-object')['Body'].read() == b'keep legacy disk objects'
            return
        if args.compression_benchmark:
            subprocess.run(['node', 'scripts/test-backend.mjs', '-p', 'cairn-installation', '--release', '--test', 'block_codec_performance',
                            '--', '--ignored', '--nocapture', '--test-threads=1'],
                           cwd=args.repo, env=env, check=True)
            return
        if args.case:
            # Keep isolated installation fixtures on the same dependency feature graph.
            run_case(args.repo, env, args.case)
            return
        if args.benchmark:
            for _ in range(args.repeat):
                # Each process gets the same empty fixture bucket. Moto's object
                # listing cost must not grow across benchmark repetitions.
                for page in client.get_paginator('list_objects_v2').paginate(Bucket='cairn-node-test'):
                    objects = [{'Key': obj['Key']} for obj in page.get('Contents', [])]
                    if objects:
                        client.delete_objects(Bucket='cairn-node-test', Delete={'Objects': objects})
                subprocess.run(['node', 'scripts/test-backend.mjs', '--release', '--test', 'node_performance',
                                'master_reuses_unchanged_blocks', '--', '--ignored', '--nocapture', '--test-threads=1'],
                               cwd=args.repo, env=env, check=True)
            return
        # The old recovery/movement/collection scenarios no longer describe
        # production. Exercise local storage, retirement and launch without S3.
        client.put_object(Bucket='cairn-node-test', Key='qualification-preserved-object', Body=b'keep legacy disk objects')
        for selected in ['storage::', 'storage_migration::', 'microvm::pool::', 'runner::']:
            run_qualification(args.repo, env, ['--workspace', '--lib', selected], selected)
        run_qualification(args.repo, env, ['-p', 'cairn-installation', '--test', 'worker'], 'worker')
        assert client.list_objects_v2(Bucket='cairn-node-test').get('KeyCount', 0) == 1
        assert client.get_object(Bucket='cairn-node-test', Key='qualification-preserved-object')['Body'].read() == b'keep legacy disk objects'

    finally:
        server.stop()


if __name__ == '__main__':
    main()
