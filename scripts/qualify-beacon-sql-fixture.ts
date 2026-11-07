import assert from 'node:assert/strict'
import { Buffer } from 'node:buffer'
import { randomUUID } from 'node:crypto'
import process from 'node:process'
import { executeBeaconSql } from '../tests/e2e/beacon-relay-fixture'

// Run only against CI's disposable Postgres service, using the real TS consumer
// and the example from the same browser-backend artifact as the journeys.
function qualify() {
  assert.ok(process.env.CAIRN_BEACON_TEST_DATABASE_URL, 'Disposable database URL required')
  const database = new URL(process.env.CAIRN_BEACON_TEST_DATABASE_URL)
  const schema = `sql_fixture_${randomUUID().replaceAll('-', '')}`
  const isolated = new URL(database)
  isolated.searchParams.set('options', `-c search_path=${schema} -c statement_timeout=5000`)

  function expectFailure(url: URL, sql: string, status: number, code: string) {
    try {
      executeBeaconSql(url, sql)
    }
    catch (error) {
      assert.ok(error instanceof Error)
      assert.equal(Reflect.get(error, 'status'), status)
      const stderr = Reflect.get(error, 'stderr')
      const stdout = Reflect.get(error, 'stdout')
      assert.ok(Buffer.isBuffer(stderr))
      assert.ok(Buffer.isBuffer(stdout))
      assert.equal(stderr.toString(), `${code}\n`)
      assert.equal(stdout.length, 0)
      assert.ok(!error.message.includes(url.toString()))
      assert.ok(!error.message.includes(sql))
      return
    }

    assert.fail('Expected a child SQL failure')
  }

  executeBeaconSql(database, `CREATE SCHEMA ${schema}`)
  try {
    executeBeaconSql(isolated, `
      CREATE TABLE entries (value integer);
      INSERT INTO entries VALUES (1), (2);
      DO $$ BEGIN
        IF current_schema() <> '${schema}' OR current_setting('statement_timeout') <> '5s'
          OR (SELECT sum(value) FROM entries) <> 3 THEN
          RAISE EXCEPTION 'fixture isolation or batch execution failed';
        END IF;
      END $$;
    `)
    expectFailure(isolated, `
      INSERT INTO entries VALUES (10);
      DO $$ BEGIN RAISE EXCEPTION 'synthetic_sensitive_sql_payload'; END $$;
      INSERT INTO entries VALUES (20);
    `, 3, 'BEACON_SQL_FIXTURE_QUERY_FAILED')
    executeBeaconSql(isolated, `DO $$ BEGIN
      IF (SELECT count(*) FROM entries) <> 2 THEN
        RAISE EXCEPTION 'failed batch did not stop and roll back';
      END IF;
    END $$;`)
    expectFailure(isolated, `
      BEGIN; INSERT INTO entries VALUES (30); COMMIT;
      DO $$ BEGIN RAISE EXCEPTION 'synthetic_sensitive_sql_payload'; END $$;
    `, 3, 'BEACON_SQL_FIXTURE_QUERY_FAILED')
    executeBeaconSql(isolated, `DO $$ BEGIN
      IF (SELECT count(*) FROM entries WHERE value = 30) <> 1 THEN
        RAISE EXCEPTION 'explicit commit was not preserved';
      END IF;
    END $$;`)

    const invalidAccount = new URL(database)
    invalidAccount.username = `missing_${randomUUID().replaceAll('-', '')}`
    invalidAccount.password = 'synthetic_sensitive_password'
    expectFailure(invalidAccount, 'SELECT 1', 2, 'BEACON_SQL_FIXTURE_CONNECTION_FAILED')
    expectFailure(new URL('https://fixture.invalid'), 'SELECT 1', 1, 'BEACON_SQL_FIXTURE_INVALID_INPUT')
  }
  finally {
    executeBeaconSql(database, `DROP SCHEMA ${schema} CASCADE`)
  }
}

try {
  qualify()
  console.log('BEACON_SQL_FIXTURE_QUALIFICATION_PASSED')
}
catch {
  // No Error dump: child arguments/input and credentials must stay private.
  console.error('BEACON_SQL_FIXTURE_QUALIFICATION_FAILED')
  process.exitCode = 1
}
