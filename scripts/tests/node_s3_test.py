"""Run encrypted node recovery against a loopback S3 server with synthetic credentials.

Run: uv run --with 'moto[server]==5.2.3' python scripts/tests/node_s3_test.py
Requires the AWS CLI and the project's pinned pnpm/Rust tools.
"""
import argparse
import os
from pathlib import Path
import subprocess

import boto3
from moto.server import ThreadedMotoServer

NATIVE_S3_KEY_BUFFER_BYTES = 64 * 1024


def configure_native_s3():
    # Moto 5.2.3 deep-copies every bucket payload for each version listing before
    # prefix filtering. Concurrent real GC requests otherwise multiply a Docker
    # store into several GiB of emulator RAM. Use Moto's supported file-spooling
    # setting; preserve the real version-list/delete APIs and every purge check.
    os.environ['MOTO_S3_DEFAULT_KEY_BUFFER_SIZE'] = str(NATIVE_S3_KEY_BUFFER_BYTES)


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
    server = ThreadedMotoServer(ip_address='127.0.0.1', port=0, verbose=False)
    server.start()
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
            if not args.qualifier:
                parser.error('--native-runtime requires --qualifier')
            subprocess.run([os.sys.executable, 'scripts/tests/native_runtime_test.py',
                            str(args.native_runtime.resolve()), str(args.qualifier.resolve())],
                           cwd=args.repo, env=env, check=True)
            remaining = client.list_objects_v2(Bucket='cairn-node-test')
            assert remaining.get('KeyCount', 0) == 0, 'Native purge left remote objects'
            return
        if args.compression_benchmark:
            subprocess.run(['node', 'scripts/test-backend.mjs', '-p', 'cairn-installation', '--release', '--test', 'block_codec_performance',
                            '--', '--ignored', '--nocapture', '--test-threads=1'],
                           cwd=args.repo, env=env, check=True)
            return
        if args.case:
            # Keep isolated installation fixtures on the same dependency feature graph.
            subprocess.run(['pnpm', 'test:backend', '-p', 'cairn-installation', '--test', 'nodes', args.case,
                            '--', '--ignored', '--nocapture'], cwd=args.repo, env=env, check=True)
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
        subprocess.run(['pnpm', 'test:backend', '--test', 'nodes', 'encrypted_recovery_points', '--', '--ignored'],
                       cwd=Path(__file__).resolve().parents[2], env=env, check=True)
        remaining = client.list_objects_v2(Bucket='cairn-node-test')
        assert remaining.get('KeyCount', 0) == 0, 'Conversation purge must remove all remote recovery objects'
        for case in ['idle_conversations_move_twice', 'queued_capacity_transfer', 'active_captures_name', 'obsolete_object_collection', 'interrupted_first_publication', 'shared_publications', 'compressed_publications', 'coalesced_final_publication', 'backend_migration']:
            subprocess.run(['pnpm', 'test:backend', '--test', 'nodes', case, '--', '--ignored'],
                           cwd=Path(__file__).resolve().parents[2], env=env, check=True)

    finally:
        server.stop()


if __name__ == '__main__':
    main()
