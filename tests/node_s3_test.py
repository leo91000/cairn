"""Run encrypted node recovery against a loopback S3 server with synthetic credentials.

Run: uv run --with 'moto[server]==5.2.3' python tests/node_s3_test.py
Requires the AWS CLI and the project's pinned pnpm/Rust tools.
"""
import os
from pathlib import Path
import subprocess

import boto3
from moto.server import ThreadedMotoServer


def main():
    server = ThreadedMotoServer(ip_address='127.0.0.1', port=0, verbose=False)
    server.start()
    try:
        host, port = server.get_host_and_port()
        endpoint = f'http://{host}:{port}'
        client = boto3.client('s3', endpoint_url=endpoint, region_name='us-east-1',
                              aws_access_key_id='node-fixture', aws_secret_access_key='node-fixture-secret')
        client.create_bucket(Bucket='leo-node-test')
        client.put_public_access_block(Bucket='leo-node-test', PublicAccessBlockConfiguration={
            'BlockPublicAcls': True, 'IgnorePublicAcls': True, 'BlockPublicPolicy': True, 'RestrictPublicBuckets': True})
        env = {key: value for key, value in os.environ.items()
               if not key.startswith(('AWS_', 'ARCHIVE_S3_'))}
        env['LEO_NODE_TEST_S3_ENDPOINT'] = endpoint
        subprocess.run(['pnpm', 'test:backend', '--test', 'nodes', 'encrypted_recovery_points'],
                       cwd=Path(__file__).resolve().parents[1], env=env, check=True)
        remaining = client.list_objects_v2(Bucket='leo-node-test')
        assert remaining.get('KeyCount', 0) == 0, 'Conversation purge must remove all remote recovery objects'
    finally:
        server.stop()


if __name__ == '__main__':
    main()
